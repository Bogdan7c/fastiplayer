use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use codec_core::VideoColorMetadata;
use crossbeam_channel::unbounded;
use media_core::{
    DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer, MediaTime, TrackId, TrackInfo,
    TrackKind,
};
use video_core::{DecodedFrame, FrameResourceHandle, VideoDecoderActivitySnapshot};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
use video_present_core::VideoFrameLeaseConfig;

use super::*;

mod command_loss;
mod commands;
mod media_install;
#[path = "tests/queued_intent.rs"]
mod queued_intent;
mod runtime_settings;
mod shutdown;
#[path = "tests/snapshot_read.rs"]
mod snapshot_read;
mod wakeup;
use crate::media_install::AcceptedPlaybackIntent;
use crate::{
    ExactMediaTransportAction, MediaInstallCompletion, MediaInstallPhase, MediaInstanceId,
    MediaSource, PlaybackIntentUpdateOutcome, PlaybackRate, PlaybackState,
    PlayerRuntimeApplyOutcome, PlayerRuntimeSettingId, ScrubCommitPolicy, SeekRequest,
};

fn worker_config_for_tests() -> PlayerWorkerConfig {
    PlayerWorkerConfig {
        coarse_wakeup_interval: Duration::from_millis(10),
        decoder_readiness_poll_interval: Duration::from_millis(2),
        tick_config: PlayerTickConfig::default(),
        decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
        default_volume: 1.0,
        audio_decoder_factory: missing_audio_decoder_factory(),
        audio_output_factory: missing_audio_output_factory(),
        audio_tempo_processor_factory: missing_audio_tempo_processor_factory(),
        frame_server_config: frame_server_core::FrameServerConfig::default()
            .validate()
            .expect("default frame-server config must validate"),
        timeline_activity_wake: None,
    }
}

fn seek_to_millis(milliseconds: u64) -> SeekRequest {
    SeekRequest::absolute(MediaTime::from_millis(milliseconds))
}

/// Fake demuxer для worker-level scrub tests без реального файла и backend resources.
struct WorkerFakeDemuxer {
    /// Media tracks, которые session увидит после load boundary.
    tracks: Vec<media_core::TrackInfo>,

    /// Длительность нужна timeline-у, чтобы source был seekable.
    duration: Option<Duration>,

    /// Полный log seek request-ов, дошедших до demux boundary.
    seek_request_log: Arc<Mutex<Vec<DemuxSeekRequest>>>,
}

impl WorkerFakeDemuxer {
    /// Создаёт seekable fake media с tracks для worker/session boundary tests.
    fn seekable_with_tracks(
        tracks: Vec<TrackInfo>,
        seek_request_log: Arc<Mutex<Vec<DemuxSeekRequest>>>,
    ) -> Self {
        Self {
            tracks,
            duration: Some(Duration::from_secs(30)),
            seek_request_log,
        }
    }

    /// Записывает seek request и возвращает нейтральный successful seek result.
    fn record_seek_request(
        &mut self,
        request: DemuxSeekRequest,
    ) -> anyhow::Result<DemuxSeekResult> {
        self.seek_request_log
            .lock()
            .expect("worker fake seek request log lock")
            .push(request);

        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: MediaTime::from_duration(request.timestamp),
            actual_track_timestamp: None,
        })
    }
}

/// Создаёт минимальный track для worker runtime tests без реального media backend.
fn worker_fake_track(track_id: u32, kind: TrackKind) -> TrackInfo {
    TrackInfo {
        id: TrackId::new(track_id),
        kind,
        codec_id: match kind {
            TrackKind::Video => "V_VP9".to_string(),
            TrackKind::Audio => "A_OPUS".to_string(),
        },
        codec_private: None,
        time_base: media_core::TimeBase::new(1, 1_000),
        duration: Some(Duration::from_secs(30)),
        sample_rate: (kind == TrackKind::Audio).then_some(48_000),
        channels: (kind == TrackKind::Audio).then_some(2),
        video: None,
    }
}

impl Demuxer for WorkerFakeDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
        Ok(media_core::DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.record_seek_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.record_seek_request(request)
    }
}

/// Минимальный fake decoder для worker activity wait tests.
#[derive(Clone)]
struct WorkerActivityDecoderThread {
    /// Snapshot neutral activity boundary-а, который видит worker planner.
    activity_snapshot: VideoDecoderActivitySnapshot,

    /// Scripted packet queue depth нужен, чтобы wakeup planner выбрал DecodeReadiness.
    packet_queue_depth: usize,

    /// Fatal errors не нужны большинству сценариев, но trait требует nonblocking drain.
    errors: Arc<Mutex<VecDeque<video_core::DecodeThreadError>>>,
}

impl WorkerActivityDecoderThread {
    /// Создаёт fake decoder с указанным activity snapshot-ом.
    fn new(activity_snapshot: VideoDecoderActivitySnapshot) -> Self {
        Self {
            activity_snapshot,
            packet_queue_depth: 0,
            errors: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Возвращает fake decoder с заданной глубиной packet queue.
    fn with_packet_queue_depth(mut self, packet_queue_depth: usize) -> Self {
        self.packet_queue_depth = packet_queue_depth;
        self
    }
}

impl video_core::VideoDecoderThreadHandle for WorkerActivityDecoderThread {
    type ResourceProvider = crate::PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        "Worker activity fake decoder"
    }

    fn send_packet(
        &self,
        _packet: video_core::DecodePacket,
    ) -> Result<(), video_core::DecodeSendError> {
        Ok(())
    }

    fn release_frame(&self, _handle: video_core::FrameResourceHandle) {}

    fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
        None
    }

    fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        None
    }

    fn try_recv_error(&self) -> Option<video_core::DecodeThreadError> {
        self.errors
            .lock()
            .expect("worker activity fake decoder error queue lock")
            .pop_front()
    }

    fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn resource_provider(&self) -> crate::PresentFrameResourceProviderHandle {
        panic!("worker activity fake decoder has no renderer resources")
    }

    fn decoder_resource_snapshot(&self) -> Option<crate::DecoderResourceSnapshot> {
        None
    }

    fn decoder_activity_snapshot(&self) -> VideoDecoderActivitySnapshot {
        self.activity_snapshot.clone()
    }

    fn packet_queue_depth(&self) -> usize {
        self.packet_queue_depth
    }

    fn drain_completed_packet_count(&self) -> usize {
        0
    }
}

fn wait_for_snapshot(
    worker: &mut PlayerWorker,
    predicate: impl Fn(&PlayerSnapshot) -> bool,
) -> PlayerSnapshot {
    let deadline = Instant::now() + Duration::from_secs(2);

    while Instant::now() < deadline {
        let snapshot = worker.latest_snapshot(FrameCounters::default());
        if predicate(&snapshot) {
            return snapshot;
        }
        thread::sleep(Duration::from_millis(2));
    }

    panic!("timed out waiting for worker snapshot");
}

fn drain_events_until(
    worker: &PlayerWorker,
    predicate: impl Fn(&[PlayerWorkerEvent]) -> bool,
) -> Vec<PlayerWorkerEvent> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut events = Vec::new();

    while Instant::now() < deadline {
        events.extend(worker.drain_events());
        if predicate(&events) {
            return events;
        }
        thread::sleep(Duration::from_millis(2));
    }

    events
}

pub(super) fn runtime_for_tests(last_tick_at: Instant) -> PlayerWorkerRuntime {
    runtime_for_tests_with_command_sender(last_tick_at).0
}

fn runtime_for_tests_with_command_sender(
    last_tick_at: Instant,
) -> (PlayerWorkerRuntime, Sender<WorkerCommand>) {
    let (runtime, command_tx, _command_queue) = runtime_for_tests_with_command_queue(last_tick_at);
    (runtime, command_tx)
}

/// Runtime и public sender поверх одной очереди с общим резервом (как в production).
fn runtime_for_tests_with_public_sender(
    last_tick_at: Instant,
) -> (PlayerWorkerRuntime, PlayerCommandSender) {
    let (runtime, _command_tx, command_queue) = runtime_for_tests_with_command_queue(last_tick_at);
    (
        runtime,
        PlayerCommandSender::for_tests_with_queue(command_queue).0,
    )
}

fn runtime_for_tests_with_command_queue(
    last_tick_at: Instant,
) -> (
    PlayerWorkerRuntime,
    Sender<WorkerCommand>,
    WorkerCommandQueue,
) {
    let (command_tx, command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    let (command_queue, command_inbox) = worker_command_queue(command_tx.clone(), command_rx);
    let (snapshot_tx, snapshot_rx) = bounded(SNAPSHOT_CHANNEL_CAPACITY);
    let (event_tx, _event_rx) = bounded(EVENT_CHANNEL_CAPACITY);
    let (render_bridge, _render_bridge_client) = RenderLeaseBridge::new();
    let (_shutdown_tx, shutdown_rx) = bounded(1);
    let playback_intent_control = Arc::new(PlaybackIntentControl::default());
    let (playback_intent_wake_tx, playback_intent_wake_rx) = bounded(1);
    let config = worker_config_for_tests();

    (
        PlayerWorkerRuntime {
            session: PlayerSession::new()
                .with_playback_intent_control(Arc::clone(&playback_intent_control)),
            worker_scheduler: WorkerScheduler,
            decoder_activity: WorkerDecoderActivityState::default(),
            command_inbox,
            playback_intent_control,
            playback_intent_wake_rx,
            _playback_intent_wake_tx_guard: playback_intent_wake_tx,
            snapshot_publisher: LatestSnapshotPublisher::new(snapshot_tx, snapshot_rx),
            event_tx,
            render_bridge,
            shutdown_rx,
            config,
            last_tick_at,
            last_diagnostics_summary_at: last_tick_at,
            last_seek_stall_log_key: None,
            last_seek_stall_log_at: None,
        },
        command_tx,
        command_queue,
    )
}

fn runtime_for_tests_with_wakeup_handles(
    last_tick_at: Instant,
) -> (
    PlayerWorkerRuntime,
    Sender<WorkerCommand>,
    Sender<()>,
    RenderLeaseBridgeClient,
) {
    let (command_tx, command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    let (_command_queue, command_inbox) = worker_command_queue(command_tx.clone(), command_rx);
    let (snapshot_tx, snapshot_rx) = bounded(SNAPSHOT_CHANNEL_CAPACITY);
    let (event_tx, _event_rx) = bounded(EVENT_CHANNEL_CAPACITY);
    let (render_bridge, render_bridge_client) = RenderLeaseBridge::new();
    let (shutdown_tx, shutdown_rx) = bounded(1);
    let playback_intent_control = Arc::new(PlaybackIntentControl::default());
    let (playback_intent_wake_tx, playback_intent_wake_rx) = bounded(1);
    let config = worker_config_for_tests();

    (
        PlayerWorkerRuntime {
            session: PlayerSession::new()
                .with_playback_intent_control(Arc::clone(&playback_intent_control)),
            worker_scheduler: WorkerScheduler,
            decoder_activity: WorkerDecoderActivityState::default(),
            command_inbox,
            playback_intent_control,
            playback_intent_wake_rx,
            _playback_intent_wake_tx_guard: playback_intent_wake_tx,
            snapshot_publisher: LatestSnapshotPublisher::new(snapshot_tx, snapshot_rx),
            event_tx,
            render_bridge,
            shutdown_rx,
            config,
            last_tick_at,
            last_diagnostics_summary_at: last_tick_at,
            last_seek_stall_log_key: None,
            last_seek_stall_log_at: None,
        },
        command_tx,
        shutdown_tx,
        render_bridge_client,
    )
}

/// Подключает active Accurate preroll, где decoder queue уже заполнена.
fn install_active_decoder_activity_preroll(
    runtime: &mut PlayerWorkerRuntime,
    activity_snapshot: VideoDecoderActivitySnapshot,
) {
    let decoder_thread =
        WorkerActivityDecoderThread::new(activity_snapshot).with_packet_queue_depth(4);
    runtime
        .session
        .install_active_accurate_preroll_decoder_for_tests(
            decoder_thread,
            Duration::from_millis(500),
        );
}

/// Планирует wait, который обязан использовать decoder activity до fallback timeout-а.
fn planned_decoder_activity_wait(runtime: &mut PlayerWorkerRuntime) -> PlannedWorkerWait {
    let wait_plan = runtime
        .plan_next_worker_wakeup_with_decoder_activity()
        .expect("active Accurate preroll should plan worker wakeup");
    let WorkerWakeupDeadline::Playback { plan, .. } = wait_plan.deadline();

    assert_eq!(plan.reason, crate::WorkerWakeupReason::DecodeReadiness);
    assert!(plan.wait_for_decoder_activity);
    assert!(
        wait_plan.decoder_activity.is_some(),
        "available activity snapshot must be attached only after planner intent"
    );

    wait_plan
}

/// Устанавливает seekable fake media с video track для worker/session seek tests.
fn install_worker_video_media(
    runtime: &mut PlayerWorkerRuntime,
    seek_request_log: Arc<Mutex<Vec<DemuxSeekRequest>>>,
) {
    let tracks = vec![worker_fake_track(1, TrackKind::Video)];
    let demuxer = WorkerFakeDemuxer::seekable_with_tracks(tracks, seek_request_log);
    runtime
        .session
        .load_demuxer_with_autoplay("worker-fake".to_string(), Box::new(demuxer), false);
}

fn command_sender_for_tests() -> (PlayerCommandSender, Receiver<WorkerCommand>) {
    let (command_tx, command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    let command_sender = PlayerCommandSender::for_tests(command_tx).0;

    (command_sender, command_rx)
}

fn receive_player_command(command_rx: &Receiver<WorkerCommand>) -> PlayerCommand {
    match command_rx.try_recv().unwrap() {
        WorkerCommand::Player(command) => command,
        _ => panic!("PlayerCommand must use WorkerCommand::Player"),
    }
}

fn apply_group_report(
    report: &PlayerRuntimeApplyReport,
    group: PlayerRuntimeApplyGroup,
) -> &PlayerRuntimeApplyGroupReport {
    report
        .groups
        .iter()
        .find(|group_report| group_report.group == group)
        .expect("runtime apply group report must exist")
}

fn decoded_frame_for_tests(resource_handle: FrameResourceHandle) -> DecodedFrame {
    decoded_frame_with_pts_for_tests(Duration::ZERO, resource_handle)
}

/// Создаёт decoded frame с заданным PTS для session present-frame simulation.
fn decoded_frame_with_pts_for_tests(
    pts: Duration,
    resource_handle: FrameResourceHandle,
) -> DecodedFrame {
    DecodedFrame {
        generation: 0,
        pts,
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        width: 640,
        height: 360,
        render_width: 640,
        render_height: 360,
        display_orientation: codec_core::VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle,
        diagnostics: video_core::VideoFrameDiagnostics::default(),
    }
}

fn present_frame_lease_for_tests(
    render_generation: u64,
    resource_handle: FrameResourceHandle,
    stale: bool,
    release_tx: Sender<RenderLeaseRelease>,
) -> VideoFrameLease {
    let mut config = VideoFrameLeaseConfig::new(
        render_generation,
        decoded_frame_for_tests(resource_handle),
        Arc::new(RenderLeaseReleaseSink::new(release_tx)),
    );
    if stale {
        config = config.with_timeline_stale();
    }
    VideoFrameLease::new(config)
}

fn worker_with_latest_handoff_for_tests(
    latest_present_frame_handoff: Arc<LatestPresentFrameHandoff>,
) -> (
    PlayerWorker,
    Receiver<RenderAcquireSample>,
    Receiver<RenderTimingSample>,
    Receiver<RenderResourcePreviousFrameReuseSample>,
) {
    worker_with_latest_handoffs_for_tests(
        latest_present_frame_handoff,
        Arc::new(LatestPresentFrameHandoff::new()),
    )
}

fn worker_with_latest_handoffs_for_tests(
    latest_present_frame_handoff: Arc<LatestPresentFrameHandoff>,
    latest_scrub_visual_override_handoff: Arc<LatestPresentFrameHandoff>,
) -> (
    PlayerWorker,
    Receiver<RenderAcquireSample>,
    Receiver<RenderTimingSample>,
    Receiver<RenderResourcePreviousFrameReuseSample>,
) {
    let (command_tx, _command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    let (_snapshot_tx, snapshot_rx) = bounded(SNAPSHOT_CHANNEL_CAPACITY);
    let (_event_tx, event_rx) = bounded(EVENT_CHANNEL_CAPACITY);
    let (
        render_bridge_client,
        render_acquire_sample_rx,
        render_timing_sample_rx,
        render_resource_previous_frame_reuse_sample_rx,
    ) = RenderLeaseBridgeClient::with_handoff_for_tests(
        latest_present_frame_handoff,
        latest_scrub_visual_override_handoff,
    );
    let (shutdown_tx, _shutdown_rx) = bounded(1);
    let command_sender = PlayerCommandSender::for_tests(command_tx).0;

    (
        PlayerWorker {
            snapshot_publication_lock: Arc::new(Mutex::new(())),
            command_sender,
            snapshot_rx,
            cached_snapshot: PlayerSnapshot::empty(),
            event_rx,
            render_bridge_client,
            decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
            shutdown_tx,
            join_handle: None,
            terminal_state: PlayerWorkerTerminalState::Completed,
        },
        render_acquire_sample_rx,
        render_timing_sample_rx,
        render_resource_previous_frame_reuse_sample_rx,
    )
}

/// Собирает terminal fixture вокруг управляемого тестом JoinHandle без запуска real session.
fn worker_with_terminal_thread_for_tests(
    command_tx: Sender<WorkerCommand>,
    shutdown_tx: Sender<()>,
    join_handle: JoinHandle<()>,
) -> PlayerWorker {
    let (_snapshot_tx, snapshot_rx) = bounded(SNAPSHOT_CHANNEL_CAPACITY);
    let (_event_tx, event_rx) = bounded(EVENT_CHANNEL_CAPACITY);
    let (_render_bridge, render_bridge_client) = RenderLeaseBridge::new();
    let playback_intent_control = Arc::new(PlaybackIntentControl::default());
    let (playback_intent_wake_tx, _playback_intent_wake_rx) = bounded(1);
    let admission_closed = Arc::new(AtomicBool::new(false));

    PlayerWorker {
        snapshot_publication_lock: Arc::new(Mutex::new(())),
        command_sender: PlayerCommandSender {
            command_queue: WorkerCommandQueue::detached_for_tests(command_tx),
            playback_intent_control,
            playback_intent_wake_tx,
            admission_closed,
        },
        snapshot_rx,
        cached_snapshot: PlayerSnapshot::empty(),
        event_rx,
        render_bridge_client,
        decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
        shutdown_tx,
        join_handle: Some(join_handle),
        terminal_state: PlayerWorkerTerminalState::Running,
    }
}
