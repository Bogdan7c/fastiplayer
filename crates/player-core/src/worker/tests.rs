use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
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
use crate::{
    MediaSource, PlaybackState, PlayerRuntimeApplyOutcome, PlayerRuntimeSettingId,
    ScrubCommitPolicy, SeekRequest,
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
        timeline_hover_prepare_handoff: PlayerTimelineHoverPrepareHandoff::default(),
        frame_server_config: frame_server_core::FrameServerConfig::default()
            .validate()
            .expect("default frame-server config must validate"),
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

    fn next_packet(&mut self) -> anyhow::Result<Option<media_core::Packet>> {
        Ok(None)
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

fn runtime_for_tests(last_tick_at: Instant) -> PlayerWorkerRuntime {
    runtime_for_tests_with_command_sender(last_tick_at).0
}

fn runtime_for_tests_with_command_sender(
    last_tick_at: Instant,
) -> (PlayerWorkerRuntime, Sender<WorkerCommand>) {
    let (command_tx, command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    let (snapshot_tx, snapshot_rx) = bounded(SNAPSHOT_CHANNEL_CAPACITY);
    let (event_tx, _event_rx) = bounded(EVENT_CHANNEL_CAPACITY);
    let (render_bridge, _render_bridge_client) = RenderLeaseBridge::new();
    let (_shutdown_tx, shutdown_rx) = bounded(1);
    let config = worker_config_for_tests();

    (
        PlayerWorkerRuntime {
            session: PlayerSession::new(),
            worker_scheduler: WorkerScheduler,
            decoder_activity: WorkerDecoderActivityState::default(),
            command_rx,
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
    let (snapshot_tx, snapshot_rx) = bounded(SNAPSHOT_CHANNEL_CAPACITY);
    let (event_tx, _event_rx) = bounded(EVENT_CHANNEL_CAPACITY);
    let (render_bridge, render_bridge_client) = RenderLeaseBridge::new();
    let (shutdown_tx, shutdown_rx) = bounded(1);
    let config = worker_config_for_tests();

    (
        PlayerWorkerRuntime {
            session: PlayerSession::new(),
            worker_scheduler: WorkerScheduler,
            decoder_activity: WorkerDecoderActivityState::default(),
            command_rx,
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
    let command_sender = PlayerCommandSender { command_tx };

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
    let command_sender = PlayerCommandSender { command_tx };

    (
        PlayerWorker {
            command_sender,
            snapshot_rx,
            cached_snapshot: PlayerSnapshot::empty(),
            event_rx,
            render_bridge_client,
            decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
            shutdown_tx,
            join_handle: None,
        },
        render_acquire_sample_rx,
        render_timing_sample_rx,
        render_resource_previous_frame_reuse_sample_rx,
    )
}

#[test]
fn worker_starts_accepts_commands_publishes_snapshot_and_shutdowns() {
    let mut worker = PlayerWorker::spawn(worker_config_for_tests()).unwrap();

    worker.try_send_command(PlayerCommand::Play).unwrap();
    let snapshot = wait_for_snapshot(&mut worker, |snapshot| {
        snapshot.playback_state == PlaybackState::Playing
    });

    assert_eq!(snapshot.playback_state, PlaybackState::Playing);
    worker.shutdown().unwrap();
}

#[test]
fn player_worker_exposes_decoder_thread_config_for_backend_factory() {
    let decoder_thread_config = PlayerVideoDecoderThreadConfig {
        packet_channel_frames: 2,
        frame_channel_frames: 3,
        control_channel_frames: 4,
        decoder_ready_queue_frames: 5,
        decoder_surface_pool_frames: 6,
        software_frame_pool_frames: 8,
        software_decode_thread_budget: video_core::SoftwareDecodeThreadBudget::auto(),
        zero_copy_surface_pool_slots: 7,
        flush_timeout: Duration::from_millis(75),
    };
    let mut config = worker_config_for_tests();
    config.decoder_thread_config = decoder_thread_config;

    let mut worker = PlayerWorker::spawn(config).unwrap();

    assert_eq!(worker.decoder_thread_config(), decoder_thread_config);
    worker.shutdown().unwrap();
}

#[test]
fn decoder_thread_config_maps_software_surface_pool_independently() {
    // sw_decoder_surface_pool_frames должен попадать именно в software_frame_pool_frames,
    // не затрагивая hardware decoder_surface_pool_frames.
    let mut config = rustiplayer_config::AppConfig::default();
    config.video.decoder_surface_pool_frames = 24;
    config.video.sw_decoder_surface_pool_frames = 6;

    let thread_config = PlayerWorkerConfig::decoder_thread_config_from_app_config(&config);

    assert_eq!(thread_config.software_frame_pool_frames, 6);
    assert_eq!(thread_config.decoder_surface_pool_frames, 24);
}

#[test]
fn runtime_apply_tick_config_updates_worker_owned_config() {
    let mut runtime = runtime_for_tests(Instant::now());
    let mut tick_config = runtime.config.tick_config;
    tick_config.max_demux_packets_per_tick += 1;

    let report =
        runtime.apply_runtime_settings(PlayerRuntimeSettingsUpdate::empty().with_tick_config(
            tick_config,
            [PlayerRuntimeSettingId::VideoSchedulerDemuxPacketsPerTick],
        ));

    assert_eq!(runtime.config.tick_config, tick_config);
    let tick_report = apply_group_report(&report, PlayerRuntimeApplyGroup::TickConfig);
    assert_eq!(
        tick_report.outcome,
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Applied)
    );
    assert_eq!(
        tick_report.affected_settings,
        vec![PlayerRuntimeSettingId::VideoSchedulerDemuxPacketsPerTick]
    );
}

#[test]
fn runtime_apply_frame_server_policy_updates_worker_and_session_owned_config() {
    let mut runtime = runtime_for_tests(Instant::now());
    let requested_frame_server_config = frame_server_core::FrameServerConfig {
        live_scrub_max_hz: 120,
        hover_prepare_window_slots: 2,
        recent_superseded_prepare_slots: 0,
        ..frame_server_core::FrameServerConfig::default()
    }
    .validate()
    .expect("test frame-server policy must validate");

    let report = runtime.apply_runtime_settings(
        PlayerRuntimeSettingsUpdate::empty().with_frame_server_policy(
            requested_frame_server_config,
            [
                PlayerRuntimeSettingId::FrameServerLiveScrubMaxHz,
                PlayerRuntimeSettingId::FrameServerHoverPrepareWindowSlots,
                PlayerRuntimeSettingId::FrameServerRecentSupersededPrepareSlots,
            ],
        ),
    );

    assert_eq!(
        runtime.config.frame_server_config,
        requested_frame_server_config
    );
    assert_eq!(
        runtime.session.frame_server_policy_config(),
        requested_frame_server_config
    );
    let frame_server_report =
        apply_group_report(&report, PlayerRuntimeApplyGroup::FrameServerPolicy);
    assert_eq!(
        frame_server_report.outcome,
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Applied)
    );
    assert_eq!(
        frame_server_report.affected_settings,
        vec![
            PlayerRuntimeSettingId::FrameServerLiveScrubMaxHz,
            PlayerRuntimeSettingId::FrameServerHoverPrepareWindowSlots,
            PlayerRuntimeSettingId::FrameServerRecentSupersededPrepareSlots,
        ]
    );
}

#[test]
fn runtime_apply_default_volume_does_not_mutate_current_playback_volume() {
    let mut runtime = runtime_for_tests(Instant::now());
    runtime
        .session
        .dispatch_command(PlayerCommand::SetVolume(0.25))
        .unwrap();

    let report = runtime.apply_runtime_settings(
        PlayerRuntimeSettingsUpdate::empty()
            .with_default_volume(0.75, [PlayerRuntimeSettingId::AudioDefaultVolume]),
    );

    assert_eq!(runtime.config.default_volume, 0.75);
    assert_eq!(runtime.session.snapshot().volume, 0.25);
    let volume_report = apply_group_report(&report, PlayerRuntimeApplyGroup::DefaultVolume);
    assert_eq!(
        volume_report.outcome,
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Applied)
    );
}

#[test]
fn runtime_apply_invalid_and_unsupported_settings_are_reported() {
    let mut runtime = runtime_for_tests(Instant::now());
    let original_tick_config = runtime.config.tick_config;
    let mut invalid_tick_config = original_tick_config;
    invalid_tick_config.max_demux_packets_per_tick = 0;

    let report = runtime.apply_runtime_settings(
        PlayerRuntimeSettingsUpdate::empty()
            .with_tick_config(
                invalid_tick_config,
                [PlayerRuntimeSettingId::VideoSchedulerDemuxPacketsPerTick],
            )
            .with_unsupported_settings([PlayerRuntimeSettingId::PlayerPreferredVideoCodecOrder]),
    );

    assert_eq!(runtime.config.tick_config, original_tick_config);
    let tick_report = apply_group_report(&report, PlayerRuntimeApplyGroup::TickConfig);
    assert_eq!(tick_report.outcome, PlayerRuntimeApplyOutcome::Invalid);
    let unsupported_report =
        apply_group_report(&report, PlayerRuntimeApplyGroup::UnsupportedSettings);
    assert_eq!(
        unsupported_report.outcome,
        PlayerRuntimeApplyOutcome::Unsupported
    );
    assert_eq!(
        unsupported_report.affected_settings,
        vec![PlayerRuntimeSettingId::PlayerPreferredVideoCodecOrder]
    );
}

#[test]
fn runtime_apply_decoder_thread_config_accepts_controlled_rebuild() {
    let mut runtime = runtime_for_tests(Instant::now());
    let original_decoder_thread_config = runtime.config.decoder_thread_config;
    let requested_decoder_thread_config = PlayerVideoDecoderThreadConfig {
        packet_channel_frames: original_decoder_thread_config.packet_channel_frames + 1,
        ..original_decoder_thread_config
    };

    let report = runtime.apply_runtime_settings(
        PlayerRuntimeSettingsUpdate::empty().with_decoder_thread_config(
            requested_decoder_thread_config,
            [PlayerRuntimeSettingId::VideoDecoderPacketChannelFrames],
        ),
    );

    assert_eq!(
        runtime.config.decoder_thread_config,
        requested_decoder_thread_config
    );
    let decoder_report = apply_group_report(&report, PlayerRuntimeApplyGroup::DecoderThreadConfig);
    assert_eq!(
        decoder_report.outcome,
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Applied)
    );
}

#[test]
fn worker_apply_runtime_settings_command_sends_real_report_response() {
    let mut runtime = runtime_for_tests(Instant::now());
    let (response_tx, response_rx) = bounded(1);

    runtime.handle_worker_command(WorkerCommand::ApplyRuntimeSettings {
        update: Box::new(
            PlayerRuntimeSettingsUpdate::empty()
                .with_default_volume(0.5, [PlayerRuntimeSettingId::AudioDefaultVolume]),
        ),
        response_tx,
    });

    let report = response_rx.recv().unwrap();
    let volume_report = apply_group_report(&report, PlayerRuntimeApplyGroup::DefaultVolume);
    assert_eq!(
        volume_report.outcome,
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Applied)
    );
}

#[test]
fn apply_runtime_settings_sender_distinguishes_backpressure_and_disconnected() {
    let (full_command_tx, _full_command_rx) = bounded(1);
    let full_command_sender = PlayerCommandSender {
        command_tx: full_command_tx,
    };
    full_command_sender.try_send(PlayerCommand::Play).unwrap();

    let update = PlayerRuntimeSettingsUpdate::empty()
        .with_default_volume(0.5, [PlayerRuntimeSettingId::AudioDefaultVolume]);
    let full_result = full_command_sender.apply_runtime_settings(update.clone());

    assert_eq!(full_result, Err(PlayerRuntimeApplyError::Backpressure));

    let (disconnected_command_tx, disconnected_command_rx) = bounded(1);
    drop(disconnected_command_rx);
    let disconnected_command_sender = PlayerCommandSender {
        command_tx: disconnected_command_tx,
    };

    let disconnected_result = disconnected_command_sender.apply_runtime_settings(update);

    assert_eq!(
        disconnected_result,
        Err(PlayerRuntimeApplyError::Disconnected)
    );
}

#[test]
fn command_ordering_for_play_pause_stop_open_shutdown_is_preserved() {
    let mut worker = PlayerWorker::spawn(worker_config_for_tests()).unwrap();
    let request = MediaOpenRequest::new(MediaSource::ExternalLabel("sample".into()), false);

    worker.try_send_command(PlayerCommand::Play).unwrap();
    worker.try_send_command(PlayerCommand::Pause).unwrap();
    worker.try_send_command(PlayerCommand::Stop).unwrap();
    worker
        .try_send_command(PlayerCommand::OpenMedia(request.clone()))
        .unwrap();
    worker.try_send_command(PlayerCommand::Shutdown).unwrap();

    let events = drain_events_until(&worker, |events| {
        events.iter().any(|event| {
            matches!(
                event,
                PlayerWorkerEvent::Player(PlayerEvent::ShutdownRequested)
            )
        })
    });
    let player_events = events
        .iter()
        .filter_map(|event| match event {
            PlayerWorkerEvent::Player(event) => Some(event),
            PlayerWorkerEvent::Scrub(_) => None,
            PlayerWorkerEvent::RenderError(_) => None,
            PlayerWorkerEvent::Tick(_) => None,
        })
        .collect::<Vec<_>>();

    let playing_index = player_events
        .iter()
        .position(|event| {
            matches!(
                event,
                PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)
            )
        })
        .expect("missing Playing event");
    let paused_index = player_events
        .iter()
        .position(|event| {
            matches!(
                event,
                PlayerEvent::PlaybackStateChanged(PlaybackState::Paused)
            )
        })
        .expect("missing Paused event");
    let open_index = player_events
        .iter()
        .position(|event| {
            matches!(
                event,
                PlayerEvent::MediaOpenRequested(open_request) if *open_request == request
            )
        })
        .expect("missing OpenMedia event");
    let shutdown_index = player_events
        .iter()
        .position(|event| matches!(event, PlayerEvent::ShutdownRequested))
        .expect("missing Shutdown event");

    assert!(playing_index < paused_index);
    assert!(paused_index < open_index);
    assert!(open_index < shutdown_index);
    worker.shutdown().unwrap();
}

#[test]
fn command_sender_routes_player_commands_through_worker_queue() {
    let (command_sender, command_rx) = command_sender_for_tests();
    let open_request = MediaOpenRequest::new(MediaSource::ExternalLabel("sample".into()), false);
    let seek_request = seek_to_millis(500);

    command_sender.try_send(PlayerCommand::Play).unwrap();
    assert_eq!(receive_player_command(&command_rx), PlayerCommand::Play);

    command_sender
        .try_send(PlayerCommand::OpenMedia(open_request.clone()))
        .unwrap();
    assert_eq!(
        receive_player_command(&command_rx),
        PlayerCommand::OpenMedia(open_request)
    );

    command_sender
        .try_send(PlayerCommand::begin_scrub())
        .unwrap();
    assert_eq!(
        receive_player_command(&command_rx),
        PlayerCommand::begin_scrub()
    );

    command_sender
        .try_send(PlayerCommand::UpdateScrub(seek_request))
        .unwrap();
    assert_eq!(
        receive_player_command(&command_rx),
        PlayerCommand::UpdateScrub(seek_request)
    );

    command_sender
        .try_send(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitLatestTarget,
        ))
        .unwrap();
    assert_eq!(
        receive_player_command(&command_rx),
        PlayerCommand::end_scrub(ScrubCommitPolicy::CommitLatestTarget)
    );
}

#[test]
fn public_scrub_api_uses_session_seek_landing_route() {
    let mut runtime = runtime_for_tests(Instant::now());
    let seek_request_log = Arc::new(Mutex::new(Vec::new()));

    install_worker_video_media(&mut runtime, Arc::clone(&seek_request_log));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::begin_scrub()));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::UpdateScrub(
        seek_to_millis(20_000),
    )));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::end_scrub(
        ScrubCommitPolicy::CommitLatestTarget,
    )));

    let expected_request = DemuxSeekRequest::decode_point_before(Duration::from_secs(20));
    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .as_slice(),
        &[expected_request]
    );
    assert!(runtime.session.has_active_seek_commit());
    assert!(!runtime.session.snapshot().timeline.seeking);
    assert!(runtime.session.snapshot().timeline.scrubbing);
    assert_eq!(
        runtime.session.snapshot().timeline.preview_state,
        media_core::TimelinePreviewState::Pending
    );
}

#[test]
fn stop_during_direct_scrub_is_plain_session_stop() {
    let mut runtime = runtime_for_tests(Instant::now());
    let seek_request_log = Arc::new(Mutex::new(Vec::new()));

    install_worker_video_media(&mut runtime, Arc::clone(&seek_request_log));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::begin_scrub()));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::UpdateScrub(
        seek_to_millis(900),
    )));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::Stop));

    assert_eq!(
        runtime.session.snapshot().playback_state,
        PlaybackState::Stopped
    );
    assert!(!runtime.session.snapshot().timeline.scrubbing);
    assert!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .is_empty()
    );
}

#[test]
fn command_sender_returns_disconnected_after_worker_shutdown() {
    let mut worker = PlayerWorker::spawn(worker_config_for_tests()).unwrap();
    let command_sender = worker.command_sender();

    worker.shutdown().unwrap();
    let result = command_sender.try_send(PlayerCommand::Play);

    assert_eq!(result, Err(PlayerWorkerSendError::Disconnected));
}

#[test]
fn idle_worker_has_no_periodic_wakeup_timeout() {
    let runtime = runtime_for_tests(Instant::now());

    assert!(runtime.plan_next_worker_wakeup().is_none());
}

#[test]
fn active_worker_uses_media_plan_as_wakeup_timeout() {
    let mut runtime = runtime_for_tests(Instant::now());

    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::Play));

    assert!(runtime.plan_next_worker_wakeup().is_some());
}

#[test]
fn command_batch_yields_to_overdue_tick_during_command_storm() {
    let (mut runtime, command_tx) = runtime_for_tests_with_command_sender(Instant::now());
    runtime.config.coarse_wakeup_interval = Duration::ZERO;
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::Play));

    for command_index in 0..MAX_COMMANDS_PER_LOOP * 2 {
        command_tx
            .try_send(WorkerCommand::SetSystemCapabilities(
                SystemCapabilities::empty(command_index as u64),
            ))
            .unwrap();
    }

    let previous_tick_at = runtime.last_tick_at;
    let processed_commands = runtime.drain_pending_command_batch();
    runtime.service_worker_fairness_checkpoint(processed_commands);

    assert_eq!(processed_commands, MAX_COMMANDS_PER_LOOP);
    assert_eq!(runtime.command_rx.len(), MAX_COMMANDS_PER_LOOP);
    assert!(runtime.last_tick_at > previous_tick_at);
}

#[test]
fn active_accurate_preroll_with_full_decoder_queue_parks_until_activity() {
    let (activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let (mut runtime, _command_tx, _shutdown_tx, _render_client) =
        runtime_for_tests_with_wakeup_handles(Instant::now());
    runtime.config.decoder_readiness_poll_interval = Duration::from_millis(150);
    install_active_decoder_activity_preroll(&mut runtime, activity_subscription.snapshot());
    let wait_plan = planned_decoder_activity_wait(&mut runtime);
    let previous_tick_at = runtime.last_tick_at;

    let notifier_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        let _ = activity_notifier.notify_activity();
    });
    let wait_started_at = Instant::now();
    let shutdown_requested = runtime.wait_for_worker_wakeup_with_timeout(wait_plan);
    let waited_for = wait_started_at.elapsed();

    notifier_thread
        .join()
        .expect("activity notifier thread should finish");
    assert!(!shutdown_requested);
    assert!(runtime.last_tick_at > previous_tick_at);
    assert!(
        waited_for < Duration::from_millis(100),
        "worker should wake from decoder activity before fallback timeout, waited {waited_for:?}"
    );
}

#[test]
fn command_wakeup_wins_over_decoder_activity() {
    let (activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let (mut runtime, command_tx) = runtime_for_tests_with_command_sender(Instant::now());
    runtime.config.decoder_readiness_poll_interval = Duration::from_millis(100);
    install_active_decoder_activity_preroll(&mut runtime, activity_subscription.snapshot());
    let wait_plan = planned_decoder_activity_wait(&mut runtime);
    let previous_tick_at = runtime.last_tick_at;

    command_tx
        .try_send(WorkerCommand::SetSystemCapabilities(
            SystemCapabilities::empty(7),
        ))
        .expect("test command queue should accept command");
    let _ = activity_notifier.notify_activity();
    let shutdown_requested = runtime.wait_for_worker_wakeup_with_timeout(wait_plan);

    assert!(!shutdown_requested);
    assert_eq!(
        runtime.last_tick_at, previous_tick_at,
        "biased select must process command before simultaneous decoder activity"
    );
}

#[test]
fn render_feedback_does_not_postpone_playback_timeout() {
    let (mut runtime, _command_tx, _shutdown_tx, render_client) =
        runtime_for_tests_with_wakeup_handles(Instant::now());
    runtime.config.coarse_wakeup_interval = Duration::from_millis(5);
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::Play));
    let wakeup = runtime
        .plan_next_worker_wakeup()
        .expect("active playback should plan a worker wakeup");
    assert!(
        !wakeup.timeout().is_zero(),
        "test must exercise a delayed playback deadline"
    );
    let previous_tick_at = runtime.last_tick_at;

    render_client.report_gpu_submit_present_latency(Duration::from_millis(1));
    let wait_started_at = Instant::now();
    let shutdown_requested = runtime.wait_for_worker_wakeup_with_timeout(PlannedWorkerWait {
        wakeup,
        decoder_activity: None,
    });
    let waited_for = wait_started_at.elapsed();

    assert!(!shutdown_requested);
    assert!(runtime.last_tick_at > previous_tick_at);
    assert!(
        waited_for < Duration::from_millis(50),
        "render feedback must not slide the original playback deadline, waited {waited_for:?}"
    );
}

#[test]
fn disconnected_and_fatal_decoder_activity_notifiers_do_not_tight_loop() {
    let (activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let (mut disconnected_runtime, _command_tx, _shutdown_tx, _render_client) =
        runtime_for_tests_with_wakeup_handles(Instant::now());
    disconnected_runtime.config.decoder_readiness_poll_interval = Duration::from_millis(20);
    install_active_decoder_activity_preroll(
        &mut disconnected_runtime,
        activity_subscription.snapshot(),
    );
    let disconnected_wait = planned_decoder_activity_wait(&mut disconnected_runtime);
    let disconnected_previous_tick_at = disconnected_runtime.last_tick_at;
    drop(activity_notifier);

    let disconnected_wait_started_at = Instant::now();
    let shutdown_requested =
        disconnected_runtime.wait_for_worker_wakeup_with_timeout(disconnected_wait);
    let disconnected_waited_for = disconnected_wait_started_at.elapsed();

    assert!(!shutdown_requested);
    assert!(disconnected_runtime.last_tick_at > disconnected_previous_tick_at);
    assert!(
        disconnected_waited_for >= Duration::from_millis(10),
        "disconnected activity receiver must fall back to bounded poll, waited {disconnected_waited_for:?}"
    );

    let (mut fatal_runtime, _command_tx, _shutdown_tx, _render_client) =
        runtime_for_tests_with_wakeup_handles(Instant::now());
    fatal_runtime.config.decoder_readiness_poll_interval = Duration::from_millis(20);
    install_active_decoder_activity_preroll(
        &mut fatal_runtime,
        VideoDecoderActivitySnapshot::unavailable(
            VideoDecoderActivityUnavailableReason::FatalNotifier(
                video_core::DecodeThreadError::new("worker activity fatal"),
            ),
        ),
    );
    let fatal_wait = fatal_runtime
        .plan_next_worker_wakeup_with_decoder_activity()
        .expect("fatal notifier should still use bounded fallback wakeup");
    let WorkerWakeupDeadline::Playback { plan, .. } = fatal_wait.deadline();
    assert_eq!(plan.reason, crate::WorkerWakeupReason::DecodeReadiness);
    assert!(!plan.wait_for_decoder_activity);
    assert!(fatal_wait.decoder_activity.is_none());
    let fatal_previous_tick_at = fatal_runtime.last_tick_at;

    let fatal_wait_started_at = Instant::now();
    let shutdown_requested = fatal_runtime.wait_for_worker_wakeup_with_timeout(fatal_wait);
    let fatal_waited_for = fatal_wait_started_at.elapsed();

    assert!(!shutdown_requested);
    assert!(fatal_runtime.last_tick_at > fatal_previous_tick_at);
    assert!(
        fatal_waited_for >= Duration::from_millis(10),
        "fatal activity notifier must fall back to bounded poll, waited {fatal_waited_for:?}"
    );
}

#[test]
fn lost_decoder_activity_between_planning_and_select_wakes_without_full_fallback() {
    let (activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let (mut runtime, _command_tx, _shutdown_tx, _render_client) =
        runtime_for_tests_with_wakeup_handles(Instant::now());
    runtime.config.decoder_readiness_poll_interval = Duration::from_millis(150);
    install_active_decoder_activity_preroll(&mut runtime, activity_subscription.snapshot());
    let wait_plan = planned_decoder_activity_wait(&mut runtime);
    let previous_tick_at = runtime.last_tick_at;

    let _ = activity_notifier.notify_activity();
    let wait_started_at = Instant::now();
    let shutdown_requested = runtime.wait_for_worker_wakeup_with_timeout(wait_plan);
    let waited_for = wait_started_at.elapsed();

    assert!(!shutdown_requested);
    assert!(runtime.last_tick_at > previous_tick_at);
    assert!(
        waited_for < Duration::from_millis(30),
        "pre-select activity_since check should close the lost-wakeup window, waited {waited_for:?}"
    );
}

#[test]
fn render_release_ack_is_drained_before_latest_publish() {
    let mut runtime = runtime_for_tests(Instant::now());
    runtime
        .session
        .register_render_lease(0, video_core::FrameResourceHandle(7));
    runtime
        .render_bridge
        .release_sender_for_tests()
        .try_send(RenderLeaseRelease {
            render_generation: 0,
            resource_handle: video_core::FrameResourceHandle(7),
            resource_provider: None,
            submitted_to_renderer: false,
            released_at: Instant::now(),
        })
        .unwrap();

    runtime
        .render_bridge
        .publish_latest_present_frame(&mut runtime.session);

    assert_eq!(runtime.session.render_lease_count(), 0);
    assert!(matches!(
        runtime.render_bridge.try_clone_latest_for_tests(),
        LatestPresentFrameAcquire::Empty
    ));
}

#[test]
fn latest_present_frame_handoff_reuses_one_drop_ack_until_replaced() {
    let handoff = LatestPresentFrameHandoff::new();
    let (release_tx, release_rx) = unbounded();
    let first_frame =
        present_frame_lease_for_tests(2, FrameResourceHandle(12), false, release_tx.clone());
    let second_frame = present_frame_lease_for_tests(2, FrameResourceHandle(13), false, release_tx);

    handoff.publish(Some(first_frame));
    let first_render_clone = match handoff.try_clone_latest() {
        LatestPresentFrameAcquire::Acquired(frame) => frame,
        LatestPresentFrameAcquire::Empty | LatestPresentFrameAcquire::Busy => {
            panic!("latest frame should be available")
        }
    };
    let repeated_render_clone = match handoff.try_clone_latest() {
        LatestPresentFrameAcquire::Acquired(frame) => frame,
        LatestPresentFrameAcquire::Empty | LatestPresentFrameAcquire::Busy => {
            panic!("latest frame should be reusable")
        }
    };

    drop(first_render_clone);
    drop(repeated_render_clone);
    assert!(release_rx.try_recv().is_err());

    handoff.publish(Some(second_frame));
    let release = release_rx.try_recv().unwrap();
    assert_eq!(release.render_generation, 2);
    assert_eq!(release.resource_handle, FrameResourceHandle(12));
    assert!(release_rx.try_recv().is_err());
}

#[test]
fn latest_present_frame_handoff_keeps_generation_safe_stale_identity() {
    let handoff = LatestPresentFrameHandoff::new();
    let (release_tx, release_rx) = unbounded();
    let old_generation_frame =
        present_frame_lease_for_tests(4, FrameResourceHandle(31), false, release_tx);

    handoff.publish(Some(old_generation_frame));
    let acquired_frame = match handoff.try_clone_latest() {
        LatestPresentFrameAcquire::Acquired(frame) => frame,
        LatestPresentFrameAcquire::Empty | LatestPresentFrameAcquire::Busy => {
            panic!("old generation frame should be observable as stale")
        }
    };

    assert!(acquired_frame.stale_for_generation(5));

    drop(acquired_frame);
    handoff.clear();
    let release = release_rx.try_recv().unwrap();
    assert_eq!(release.render_generation, 4);
    assert_eq!(release.resource_handle, FrameResourceHandle(31));
}

#[test]
fn player_worker_try_acquire_present_frame_reads_latest_slot_without_reply_wait() {
    let latest_present_frame_handoff = Arc::new(LatestPresentFrameHandoff::new());
    let (release_tx, _release_rx) = unbounded();
    let expected_resource_handle = FrameResourceHandle(44);
    let frame =
        present_frame_lease_for_tests(3, expected_resource_handle, false, release_tx.clone());
    latest_present_frame_handoff.publish(Some(frame));
    let (
        worker,
        render_acquire_sample_rx,
        _render_timing_sample_rx,
        _render_resource_previous_frame_reuse_sample_rx,
    ) = worker_with_latest_handoff_for_tests(Arc::clone(&latest_present_frame_handoff));

    let acquired_frame = worker.try_acquire_present_frame().unwrap();

    assert_eq!(acquired_frame.render_generation(), 3);
    assert_eq!(acquired_frame.resource_handle(), expected_resource_handle);
    assert!(render_acquire_sample_rx.try_recv().is_ok());
}

#[test]
fn player_worker_scrub_visual_override_handoff_stays_separate_from_playback_slot() {
    let playback_handoff = Arc::new(LatestPresentFrameHandoff::new());
    let scrub_override_handoff = Arc::new(LatestPresentFrameHandoff::new());
    let (release_tx, _release_rx) = unbounded();
    let playback_handle = FrameResourceHandle(44);
    let scrub_override_handle = FrameResourceHandle(45);
    playback_handoff.publish(Some(present_frame_lease_for_tests(
        3,
        playback_handle,
        false,
        release_tx.clone(),
    )));
    scrub_override_handoff.publish(Some(present_frame_lease_for_tests(
        3,
        scrub_override_handle,
        false,
        release_tx,
    )));
    let (
        worker,
        _render_acquire_sample_rx,
        _render_timing_sample_rx,
        _render_resource_previous_frame_reuse_sample_rx,
    ) = worker_with_latest_handoffs_for_tests(playback_handoff, scrub_override_handoff);

    let playback_frame = worker.try_acquire_present_frame().unwrap();
    let scrub_override_frame = worker.try_acquire_scrub_visual_override_frame().unwrap();

    assert_eq!(playback_frame.resource_handle(), playback_handle);
    assert_eq!(
        scrub_override_frame.resource_handle(),
        scrub_override_handle
    );
}

#[test]
fn player_worker_reports_gpu_submit_present_latency_without_command_queue() {
    let latest_present_frame_handoff = Arc::new(LatestPresentFrameHandoff::new());
    let (
        worker,
        _render_acquire_sample_rx,
        render_timing_sample_rx,
        _render_resource_previous_frame_reuse_sample_rx,
    ) = worker_with_latest_handoff_for_tests(latest_present_frame_handoff);

    worker.report_gpu_submit_present_latency(Duration::from_millis(1));

    let sample = render_timing_sample_rx
        .try_recv()
        .expect("render timing sample should be queued");
    assert_eq!(sample.submit_present_elapsed, Duration::from_millis(1));
}

#[test]
fn player_worker_reports_render_resource_previous_frame_reuse_without_command_queue() {
    let latest_present_frame_handoff = Arc::new(LatestPresentFrameHandoff::new());
    let (
        worker,
        _render_acquire_sample_rx,
        _render_timing_sample_rx,
        render_resource_previous_frame_reuse_sample_rx,
    ) = worker_with_latest_handoff_for_tests(latest_present_frame_handoff);

    worker.report_render_resource_previous_frame_reuse();

    render_resource_previous_frame_reuse_sample_rx
        .try_recv()
        .expect("render resource previous-frame reuse sample should be queued");
}

#[test]
fn tick_runs_while_render_lease_is_active() {
    let mut runtime = runtime_for_tests(Instant::now());
    runtime
        .session
        .register_render_lease(0, video_core::FrameResourceHandle(11));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::Play));
    let previous_tick_at = runtime.last_tick_at;
    let plan = runtime.session.worker_wakeup_plan(
        Instant::now(),
        &runtime.config.tick_config,
        runtime.config.decoder_readiness_poll_interval,
        runtime.config.coarse_wakeup_interval,
    );

    runtime.run_tick_for_wakeup_plan(plan, Instant::now());

    assert!(runtime.last_tick_at > previous_tick_at);
}

#[test]
fn present_frame_lease_drop_releases_frame_exactly_once() {
    let (release_tx, release_rx) = unbounded();
    let lease =
        present_frame_lease_for_tests(2, FrameResourceHandle(12), false, release_tx.clone());
    let lease_clone = lease.clone();

    drop(lease);
    assert!(release_rx.try_recv().is_err());

    drop(lease_clone);
    let release = release_rx.try_recv().unwrap();

    assert_eq!(release.render_generation, 2);
    assert_eq!(release.resource_handle, FrameResourceHandle(12));
    assert!(release_rx.try_recv().is_err());
}

#[test]
fn present_frame_lease_drop_times_out_when_release_queue_stays_full() {
    let (release_tx, release_rx) = bounded(1);
    release_tx
        .try_send(RenderLeaseRelease {
            render_generation: 1,
            resource_handle: FrameResourceHandle(1),
            resource_provider: None,
            submitted_to_renderer: false,
            released_at: Instant::now(),
        })
        .unwrap();
    let lease = present_frame_lease_for_tests(2, FrameResourceHandle(12), false, release_tx);
    let drop_started_at = Instant::now();

    drop(lease);

    assert!(drop_started_at.elapsed() < Duration::from_secs(1));
    assert_eq!(release_rx.len(), 1);
    let queued_release = release_rx.try_recv().unwrap();
    assert_eq!(queued_release.render_generation, 1);
    assert_eq!(queued_release.resource_handle, FrameResourceHandle(1));
}

#[test]
fn leased_frame_release_is_deferred_until_renderer_drops_lease() {
    let mut runtime = runtime_for_tests(Instant::now());
    let resource_handle = FrameResourceHandle(21);

    assert!(runtime.session.register_render_lease(0, resource_handle));
    runtime.session.release_video_texture(resource_handle);

    assert_eq!(runtime.session.render_lease_count(), 1);
    assert!(
        runtime
            .session
            .has_deferred_video_texture_release(resource_handle)
    );

    runtime.session.release_render_lease(0, resource_handle);

    assert_eq!(runtime.session.render_lease_count(), 0);
    assert_eq!(runtime.session.deferred_video_texture_release_count(), 0);
}

#[test]
fn new_generation_makes_old_lease_stale_without_dropping_it() {
    let (release_tx, release_rx) = unbounded();
    let lease = present_frame_lease_for_tests(4, FrameResourceHandle(31), false, release_tx);

    assert!(lease.stale_for_generation(5));
    assert!(release_rx.try_recv().is_err());

    drop(lease);

    let release = release_rx.try_recv().unwrap();
    assert_eq!(release.render_generation, 4);
    assert_eq!(release.resource_handle, FrameResourceHandle(31));
}

#[test]
fn render_error_command_updates_player_error_snapshot() {
    let mut runtime = runtime_for_tests(Instant::now());
    let render_error = PlayerRenderError {
        kind: PlayerRenderErrorKind::MissingRenderResources,
        render_generation: Some(6),
        frame_handle: Some(42),
        message: "missing Y/UV views for test frame".into(),
    };

    runtime.handle_worker_command(WorkerCommand::RenderError(render_error));

    let snapshot_error = runtime.session.snapshot().last_error.as_ref().unwrap();
    assert_eq!(
        snapshot_error.kind,
        PlayerErrorKind::UnsupportedRenderFormat
    );
    assert!(
        snapshot_error
            .message
            .contains("missing Y/UV views for test frame")
    );
    assert_eq!(runtime.session.playback_state(), PlaybackState::Failed);
    assert!(
        runtime
            .session
            .take_events()
            .iter()
            .any(|event| matches!(event, PlayerEvent::FatalError(error)
                if error.kind == PlayerErrorKind::UnsupportedRenderFormat))
    );
}
