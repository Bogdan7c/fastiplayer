use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use audio_core::{
    AudioOutputClockTiming, AudioOutputInputFrameCount, AudioOutputStreamFrameCount,
    AudioOutputWriteError, AudioOutputWriteIntent, AudioOutputWriteReport,
};
use bytes::Bytes;
use codec_core::{VideoCodec, VideoDecodeRequirement};
use media_core::{PacketKeyframe, TrackId, TrackKind};

use super::video_decoder_io::{
    VideoDecoderIoLimits, accept_video_packet_for_decoder_bootstrap, drain_decoded_video_frames,
    pending_video_packet_generation_drop_reason, player_error_from_decode_thread_error,
    send_pending_video_packets_to_decoder,
};
use super::*;
use super::{demux_admission::*, presentation_scheduler::*, wakeup::*};
use crate::{
    DecodeBackpressureReason, DecodeSendError, DecodeThreadError, FrameCounters,
    PendingAudioPacket, PendingVideoPacket, PlaybackPipeline, PlaybackRate, PlaybackResumeIntent,
    PlayerAudioClock, PlayerAudioOutput, PlayerCommand, PlayerDecodePacket, PlayerErrorKind,
    PlayerSession, PresentFrameResourceProviderHandle, WorkerWakeupReason,
};

/// Empty demuxer для проверки admission без реального container backend-а.
struct EmptyDemuxer {
    /// Track list хранится внутри fake, чтобы выполнить `Demuxer::tracks` contract.
    tracks: Vec<media_core::TrackInfo>,
}

impl EmptyDemuxer {
    /// Создаёт demuxer без packets: admission tests не читают из него данные.
    fn new() -> Self {
        Self { tracks: Vec::new() }
    }
}

impl media_core::Demuxer for EmptyDemuxer {
    /// Возвращает стабильный track list для boundary contract-а.
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &self.tracks
    }

    /// Duration в этих тестах не участвует в admission policy.
    fn duration(&self) -> Option<Duration> {
        None
    }

    /// Packet-ов нет: тесты проверяют только `demux_work_available`.
    fn next_packet(&mut self) -> anyhow::Result<Option<media_core::Packet>> {
        Ok(None)
    }

    /// Seek возвращает requested point, чтобы fake оставался честным `Demuxer`.
    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<media_core::DemuxSeekResult> {
        Ok(media_core::DemuxSeekResult {
            requested_position: media_core::MediaTime::from_duration(timestamp),
            actual_position: media_core::MediaTime::from_duration(timestamp),
            actual_track_timestamp: None,
        })
    }
}

/// Fake clock для output boundary; admission tests читают только buffer level.
struct FixedAudioClock;

impl PlayerAudioClock for FixedAudioClock {
    /// Clock position в этих тестах не участвует.
    fn now(&self) -> Duration {
        Duration::ZERO
    }

    /// Admission fake не моделирует queued или callback PCM tail.
    fn output_timing(&self) -> AudioOutputClockTiming {
        AudioOutputClockTiming::new(Duration::ZERO, Duration::ZERO)
    }

    /// Reset не имеет side effects для admission policy.
    fn reset(&self) {}

    /// Underrun diagnostics в этих тестах не участвуют.
    fn underrun_callbacks(&self) -> u64 {
        0
    }
}

/// Fake audio output с фиксированным buffer level для high-water admission tests.
struct FixedAudioOutput {
    /// Clock нужен output boundary, хотя demux admission его не читает.
    clock: Arc<FixedAudioClock>,

    /// Уровень audio buffer, который видит `PlayerSession::audio_buffer_level_ms`.
    buffer_level_ms: f64,
}

impl FixedAudioOutput {
    /// Создаёт output с заданным buffer level.
    fn new(buffer_level_ms: f64) -> Self {
        Self {
            clock: Arc::new(FixedAudioClock),
            buffer_level_ms,
        }
    }
}

impl PlayerAudioOutput for FixedAudioOutput {
    /// Тестовый output принимает samples как полностью записанные frames.
    fn write_samples(
        &mut self,
        samples: &[f32],
        _intent: AudioOutputWriteIntent,
    ) -> std::result::Result<AudioOutputWriteReport, AudioOutputWriteError> {
        Ok(AudioOutputWriteReport::complete(
            AudioOutputInputFrameCount::new(samples.len()),
            AudioOutputStreamFrameCount::new(samples.len()),
        ))
    }

    /// Запуск stream-а в admission tests не имеет side effects.
    fn play(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Pause в admission tests возвращает нейтральный нулевой snapshot.
    fn pause_and_freeze_clock(&mut self) -> anyhow::Result<AudioOutputClockTiming> {
        Ok(self.clock.output_timing())
    }

    /// Clear подтверждает ровно тот generation, который попросил caller.
    fn clear_buffer_for_seek(&mut self, generation: u64) -> anyhow::Result<u64> {
        Ok(generation)
    }

    /// Volume не влияет на demux admission.
    fn set_volume(&mut self, _volume: f32) {}

    /// Возвращает scripted buffer level.
    fn buffer_level_ms(&self) -> f64 {
        self.buffer_level_ms
    }

    /// Возвращает fake clock для соблюдения audio output boundary.
    fn clock(&self) -> Arc<dyn PlayerAudioClock> {
        let clock: Arc<dyn PlayerAudioClock> = self.clock.clone();
        clock
    }
}

/// Подключает empty demuxer и оставляет track selection под контролем теста.
fn install_empty_demuxer(session: &mut PlayerSession) {
    session
        .pipeline
        .install_opened_media(Box::new(EmptyDemuxer::new()), None, None, Vec::new());
}

/// Подключает fake output с явно заданным buffer level.
fn install_fixed_audio_output(session: &mut PlayerSession, buffer_level_ms: f64) {
    session
        .pipeline
        .install_audio_output_for_tests(Box::new(FixedAudioOutput::new(buffer_level_ms)));
}

/// Добавляет один pending audio packet на выбранной generation.
fn enqueue_pending_audio_packet(session: &mut PlayerSession, track_id: TrackId) {
    let generation = session.pipeline.seek_generation();
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::new(
            track_id,
            Duration::ZERO,
            None,
            None,
            generation,
            Bytes::from_static(b"audio"),
        ));
}

/// Создаёт минимальный VP9 video track для packet refinement tests.
fn video_track_for_tests(track_id: u32) -> media_core::TrackInfo {
    media_core::TrackInfo {
        id: TrackId::new(track_id),
        kind: TrackKind::Video,
        codec_id: "V_VP9".to_string(),
        codec_private: None,
        time_base: media_core::TimeBase::new(1, 1_000),
        duration: Some(Duration::from_secs(30)),
        sample_rate: None,
        channels: None,
        video: None,
    }
}

/// Создаёт test frame без реальных GPU resources с явным decoded contract.
fn decoded_frame_with_format(
    pts: Duration,
    handle: u64,
    format: video_core::DecodedPixelFormat,
) -> video_core::DecodedFrame {
    let frame_contract = match format {
        video_core::DecodedPixelFormat::Nv12 => {
            video_frame_contract::VideoFrameContract::dma_buf_nv12(
                video_frame_contract::DmaBufImageLayout::ComposedLayers,
            )
        }
        video_core::DecodedPixelFormat::P010 => {
            video_frame_contract::VideoFrameContract::dma_buf_p010(
                video_frame_contract::DmaBufImageLayout::SeparateLayers,
            )
        }
        video_core::DecodedPixelFormat::Rgba8 => {
            panic!("RGBA8 is not a production decoded video test format")
        }
        video_core::DecodedPixelFormat::Yuv420Planar8
        | video_core::DecodedPixelFormat::Yuv420Planar10Le
        | video_core::DecodedPixelFormat::Yuv420Planar12Le
        | video_core::DecodedPixelFormat::Yuv422Planar8
        | video_core::DecodedPixelFormat::Yuv422Planar10Le
        | video_core::DecodedPixelFormat::Yuv422Planar12Le
        | video_core::DecodedPixelFormat::Yuv444Planar8
        | video_core::DecodedPixelFormat::Yuv444Planar10Le => {
            panic!("host-planar layouts are not production decoded video test formats")
        }
    };

    video_core::DecodedFrame {
        generation: 0,
        pts,
        frame_contract,
        width: 640,
        height: 360,
        render_width: 640,
        render_height: 360,
        display_orientation: codec_core::VideoDisplayOrientation::Identity,
        color: codec_core::VideoColorMetadata::sdr_bt709_limited(),
        resource_handle: video_core::FrameResourceHandle(handle),
        diagnostics: video_core::VideoFrameDiagnostics::default(),
    }
}

/// Создаёт текущий production NV12 test frame без привязки scheduler assertions к формату.
fn decoded_frame(pts: Duration, handle: u64) -> video_core::DecodedFrame {
    decoded_frame_with_format(pts, handle, video_core::DecodedPixelFormat::Nv12)
}

/// Формирует decoder I/O limits для focused tests без чтения config внутри scheduler-а.
fn decoder_io_limits_for_tests(
    max_frames_to_drain: usize,
    max_packets_to_send: usize,
) -> VideoDecoderIoLimits {
    video_decoder_io_limits(
        &PlayerTickConfig::default(),
        max_frames_to_drain,
        max_packets_to_send,
        Duration::from_millis(250),
    )
}

/// Создаёт texture snapshot с явно заданным количеством занятых slots.
fn decoder_resource_snapshot_for_tests(
    capacity: usize,
    in_use: usize,
) -> crate::DecoderResourceSnapshot {
    crate::DecoderResourceSnapshot {
        capacity,
        slots: capacity,
        in_use,
        free_surfaces: capacity.saturating_sub(in_use),
        waiting_gpu_completion: 0,
        waiting_decoder_reuse: 0,
        import_failures: 0,
        imports_created: 0,
        imports_reused: 0,
        imports_replaced: 0,
    }
}

/// Запускает Accurate seek с выбранным video track-ом и возвращает active generation.
fn start_accurate_seek_for_decoder_io(
    session: &mut PlayerSession,
    decoder_thread: RecordingVideoDecoderThread,
    target_position: Duration,
) -> u64 {
    session.pipeline.set_video_decoder_thread(decoder_thread);
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    let generation = session.pipeline.begin_seek_generation();
    session.begin_seek_trace_for_tests(generation);
    session.set_seek_commit_for_tests(Some(crate::seek_state::SeekCommitState {
        generation,
        seek_mode: crate::SeekMode::Accurate,
        target_position: media_core::MediaTime::from_duration(target_position),
        actual_position: media_core::MediaTime::from_duration(target_position),
        started_at: Instant::now(),
        resume_intent: PlaybackResumeIntent::Pause,
    }));
    session.mark_decoder_output_floor_applied_for_tests(generation, target_position);
    generation
}

/// Добавляет pending video packet для выбранного track-а текущей generation.
fn enqueue_selected_video_packet(
    session: &mut PlayerSession,
    pts: Duration,
    encoded_bytes: Bytes,
    keyframe: impl Into<PacketKeyframe>,
) {
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            pts,
            session.pipeline.seek_generation(),
            encoded_bytes,
            keyframe,
        ));
}

/// Fake decoder, который записывает отправленные packets без реального backend-а.
#[derive(Clone, Default)]
struct RecordingVideoDecoderThread {
    /// Packet log нужен тесту, чтобы отличить drop от отправки в decoder.
    sent_packets: Arc<Mutex<Vec<PlayerDecodePacket>>>,

    /// Scripted send results позволяют различить backpressure и fatal send.
    send_results: Arc<Mutex<VecDeque<Result<(), DecodeSendError>>>>,

    /// Очередь decoded frames для проверки decoder receive boundary.
    decoded_frames: Arc<Mutex<VecDeque<video_core::DecodedFrame>>>,

    /// Diagnostics events, которые fake отдаёт decoder I/O drain-у.
    diagnostic_events: Arc<Mutex<VecDeque<video_core::VideoDecoderDiagnosticEvent>>>,

    /// Texture handles, которые session вернула decoder boundary.
    released_handles: Arc<Mutex<Vec<video_core::FrameResourceHandle>>>,

    /// Snapshot texture/surface pressure для packet admission tests.
    resource_snapshot: Arc<Mutex<Option<crate::DecoderResourceSnapshot>>>,

    /// Software host-upload snapshot для decoder send backpressure tests.
    host_upload_resource_status: Arc<Mutex<Option<video_core::HostUploadResourceSnapshotStatus>>>,

    /// Snapshot control-channel pressure для neutral backpressure tests.
    control_pressure: Arc<Mutex<Option<crate::DecoderControlChannelPressureSnapshot>>>,

    /// Активный Accurate preroll floor внутри fake decoder-а.
    preroll_floor: Arc<Mutex<Option<video_core::VideoPrerollOutputFloor>>>,

    /// История clear-floor команд.
    preroll_floor_clears: Arc<Mutex<Vec<video_core::VideoPrerollOutputFloorClear>>>,

    /// Neutral activity snapshot для planner tests без real decoder thread-а.
    activity_snapshot: Option<video_core::VideoDecoderActivitySnapshot>,

    /// Scripted packet queue depth, который видит wakeup planner.
    packet_queue_depth: usize,
}

impl RecordingVideoDecoderThread {
    /// Создаёт пустой fake decoder для проверки routing/drop behavior.
    fn new() -> Self {
        Self::default()
    }

    /// Создаёт fake decoder с явно заданным neutral activity snapshot.
    fn new_with_activity_snapshot(snapshot: video_core::VideoDecoderActivitySnapshot) -> Self {
        Self {
            activity_snapshot: Some(snapshot),
            ..Self::default()
        }
    }

    /// Возвращает fake decoder с заданной глубиной packet queue.
    fn with_packet_queue_depth(mut self, packet_queue_depth: usize) -> Self {
        self.packet_queue_depth = packet_queue_depth;
        self
    }

    /// Возвращает snapshot отправленных packets без раскрытия mutex наружу.
    fn sent_packets(&self) -> Vec<PlayerDecodePacket> {
        self.sent_packets
            .lock()
            .expect("recording decoder packet log lock")
            .clone()
    }

    /// Кладёт synthetic decoded frame в receive queue fake decoder-а.
    fn push_decoded_frame(&self, frame: video_core::DecodedFrame) {
        self.decoded_frames
            .lock()
            .expect("recording decoder frame queue lock")
            .push_back(frame);
    }

    /// Задаёт результат следующей отправки packet-а в decoder boundary.
    fn push_send_result(&self, result: Result<(), DecodeSendError>) {
        self.send_results
            .lock()
            .expect("recording decoder send result lock")
            .push_back(result);
    }

    /// Публикует diagnostics event через fake decoder boundary.
    fn push_diagnostic_event(&self, event: video_core::VideoDecoderDiagnosticEvent) {
        self.diagnostic_events
            .lock()
            .expect("recording decoder diagnostics queue lock")
            .push_back(event);
    }

    /// Возвращает handles, освобождённые через decoder release path.
    fn released_handles(&self) -> Vec<video_core::FrameResourceHandle> {
        self.released_handles
            .lock()
            .expect("recording decoder release log lock")
            .clone()
    }

    /// Настраивает texture/resource snapshot для admission pressure tests.
    fn set_resource_snapshot(&self, resource_snapshot: crate::DecoderResourceSnapshot) {
        *self
            .resource_snapshot
            .lock()
            .expect("recording decoder resource snapshot lock") = Some(resource_snapshot);
    }

    /// Настраивает software host-upload snapshot/status без VA-API counters.
    fn set_host_upload_resource_status(
        &self,
        status: video_core::HostUploadResourceSnapshotStatus,
    ) {
        *self
            .host_upload_resource_status
            .lock()
            .expect("recording decoder host-upload resource status lock") = Some(status);
    }

    /// Настраивает snapshot decoder control channel-а.
    fn set_control_pressure(&self, pressure: crate::DecoderControlChannelPressureSnapshot) {
        *self
            .control_pressure
            .lock()
            .expect("recording decoder control pressure lock") = Some(pressure);
    }
}

impl video_core::VideoDecoderThreadHandle for RecordingVideoDecoderThread {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        "Recording fake decoder"
    }

    fn send_packet(&self, packet: PlayerDecodePacket) -> Result<(), DecodeSendError> {
        let scripted_result = self
            .send_results
            .lock()
            .expect("recording decoder send result lock")
            .pop_front();
        if let Some(result) = scripted_result {
            if result.is_ok() {
                self.sent_packets
                    .lock()
                    .expect("recording decoder packet log lock")
                    .push(packet);
            }
            return result;
        }

        self.sent_packets
            .lock()
            .expect("recording decoder packet log lock")
            .push(packet);
        Ok(())
    }

    fn release_frame(&self, handle: video_core::FrameResourceHandle) {
        self.released_handles
            .lock()
            .expect("recording decoder release log lock")
            .push(handle);
    }

    fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
        self.decoded_frames
            .lock()
            .expect("recording decoder frame queue lock")
            .pop_front()
    }

    fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        self.diagnostic_events
            .lock()
            .expect("recording decoder diagnostics queue lock")
            .pop_front()
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        None
    }

    fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn resource_provider(&self) -> PresentFrameResourceProviderHandle {
        panic!("recording fake decoder does not provide renderer resources")
    }

    fn decoder_resource_snapshot(&self) -> Option<crate::DecoderResourceSnapshot> {
        *self
            .resource_snapshot
            .lock()
            .expect("recording decoder resource snapshot lock")
    }

    fn host_upload_resource_snapshot(&self) -> video_core::HostUploadResourceSnapshotStatus {
        self.host_upload_resource_status
            .lock()
            .expect("recording decoder host-upload resource status lock")
            .unwrap_or(video_core::HostUploadResourceSnapshotStatus::UnsupportedBackend)
    }

    fn decoder_control_channel_pressure(
        &self,
    ) -> Option<crate::DecoderControlChannelPressureSnapshot> {
        *self
            .control_pressure
            .lock()
            .expect("recording decoder control pressure lock")
    }

    fn decoder_activity_snapshot(&self) -> video_core::VideoDecoderActivitySnapshot {
        self.activity_snapshot
            .clone()
            .unwrap_or_else(video_core::VideoDecoderActivitySnapshot::unsupported)
    }

    fn set_preroll_output_floor(
        &self,
        floor: video_core::VideoPrerollOutputFloor,
    ) -> video_core::VideoPrerollOutputFloorResult {
        *self
            .preroll_floor
            .lock()
            .expect("recording decoder preroll floor state lock") = Some(floor);
        video_core::VideoPrerollOutputFloorResult::Applied
    }

    fn clear_preroll_output_floor(
        &self,
        clear: video_core::VideoPrerollOutputFloorClear,
    ) -> video_core::VideoPrerollOutputFloorResult {
        self.preroll_floor_clears
            .lock()
            .expect("recording decoder preroll floor clear log lock")
            .push(clear);
        *self
            .preroll_floor
            .lock()
            .expect("recording decoder preroll floor state lock") = None;
        video_core::VideoPrerollOutputFloorResult::Cleared
    }

    fn packet_queue_depth(&self) -> usize {
        self.packet_queue_depth
    }

    fn drain_completed_packet_count(&self) -> usize {
        0
    }
}

/// Создаёт capabilities, где аппаратный backend принимает только VP9 Profile 0.
fn capabilities_with_vp9_profile0_for_decoder_io() -> capability_core::SystemCapabilities {
    let backend_id = codec_core::DecodeBackendId::new("decoder_io_test_backend")
        .expect("test backend id должен быть валидным");
    let output = capability_core::SupportedVideoOutput {
        backend: backend_id.clone(),
        decode_format: codec_core::SupportedVideoDecodeFormat {
            codec: VideoCodec::Vp9,
            profile: codec_core::VideoProfile::Vp9(codec_core::Vp9Profile::Profile0),
            bit_depth: codec_core::BitDepth::Eight,
            chroma: codec_core::ChromaSubsampling::Yuv420,
            max_width: Some(1920),
            max_height: Some(1080),
            max_fps: None,
            hdr_input: false,
        },
        frame_contract: video_frame_contract::VideoFrameContract::dma_buf_nv12(
            video_frame_contract::DmaBufImageLayout::ComposedLayers,
        ),
    };

    capability_core::SystemCapabilities {
        schema_version: capability_core::CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![capability_core::BackendCapabilities {
            backend_id: backend_id.clone(),
            display_name: "Decoder I/O test backend".to_string(),
            status: capability_core::BackendProbeStatus::Available,
            driver: capability_core::BackendDriverInfo::default(),
            raw_supported_outputs: vec![output.clone()],
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            diagnostics: Vec::new(),
        }],
        render_backends: vec![render_core::RenderCapabilities::wgpu_nv12(Some(4096))],
        playable_video_outputs: vec![output],
    }
}

/// Собирает минимальный VP9 keyframe header для packet refinement tests.
fn vp9_profile2_10bit_keyframe_for_tests() -> Bytes {
    Bytes::from_static(&[0x92, 0x49, 0x83, 0x42, 0x50, 0x77, 0xf8, 0x43, 0x78])
}

#[test]
fn scheduler_presents_first_ready_frame() {
    let mut session = PlayerSession::new();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame(Duration::ZERO, 1));

    let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

    assert_eq!(tick_result.video_frames_presented, 1);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::ZERO)
    );
    assert!(session.pipeline.video_present_queue_is_empty());
}

#[test]
fn scheduler_waits_for_future_frame() {
    let mut session = PlayerSession::new();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame(Duration::from_secs(1), 1));

    let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

    assert_eq!(tick_result.video_frames_presented, 0);
    assert!(session.pipeline.present_video_frame().is_none());
    assert_eq!(session.pipeline.video_present_queue_len(), 1);
}

#[test]
fn scheduler_presents_frame_at_pts_minus_present_lead() {
    let mut session = PlayerSession::new();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.update_current_position(Duration::from_millis(8));
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame(Duration::from_millis(16), 1));

    let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

    assert_eq!(tick_result.video_frames_presented, 1);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_millis(16))
    );
}

#[test]
fn scheduler_uses_fallback_position_when_audio_clock_is_absent() {
    let mut session = PlayerSession::new();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.update_current_position(Duration::from_millis(100));
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame(Duration::from_millis(100), 1));

    let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

    assert_eq!(tick_result.video_frames_presented, 1);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_millis(100))
    );
    assert!(session.pipeline.video_present_queue_is_empty());
}

#[test]
fn worker_wakeup_for_5994_cadence_uses_pts_minus_present_lead_deadline() {
    let mut session = PlayerSession::new();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.update_current_position(Duration::ZERO);
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame(Duration::from_micros(16_683), 1));

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &PlayerTickConfig::default(),
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, WorkerWakeupReason::FramePtsDeadline);
    let planned_delay = plan.delay.expect("queued frame should create deadline");
    assert!(
        planned_delay > Duration::from_millis(6),
        "worker woke too early for a PTS-derived lead deadline: {planned_delay:?}"
    );
    assert!(
        planned_delay < Duration::from_millis(12),
        "worker must not wait until the exact PTS and miss render handoff: {planned_delay:?}"
    );
}

#[test]
fn worker_wakeup_for_24_and_30_fps_is_not_fixed_60hz_polling() {
    for frame_duration in [Duration::from_millis(33), Duration::from_millis(41)] {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.update_current_position(Duration::ZERO);
        session.pipeline.enqueue_queued_video_frame(decoded_frame(
            frame_duration,
            frame_duration.as_micros() as u64,
        ));

        let plan = session.worker_wakeup_plan(
            Instant::now(),
            &PlayerTickConfig::default(),
            Duration::from_millis(2),
            Duration::from_millis(250),
        );

        assert_eq!(plan.reason, WorkerWakeupReason::FramePtsDeadline);
        assert!(
            plan.delay
                .is_some_and(|delay| delay > Duration::from_millis(20))
        );
    }
}

#[test]
fn worker_wakeup_scales_no_audio_frame_deadline_by_playback_rate() {
    let tick_config = PlayerTickConfig {
        video_present_lead_frames: 0.0,
        ..PlayerTickConfig::default()
    };
    let cases = [
        (
            PlaybackRate::MIN,
            Duration::from_millis(3_900),
            Duration::from_millis(4_100),
        ),
        (
            PlaybackRate::new(2.0).expect("2x playback rate must validate"),
            Duration::from_millis(450),
            Duration::from_millis(550),
        ),
        (
            PlaybackRate::new(0.5).expect("0.5x playback rate must validate"),
            Duration::from_millis(1_900),
            Duration::from_millis(2_100),
        ),
        (
            PlaybackRate::MAX,
            Duration::from_millis(200),
            Duration::from_millis(300),
        ),
    ];

    for (playback_rate, min_delay, max_delay) in cases {
        let mut session = PlayerSession::new();
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.update_current_position(Duration::ZERO);
        session
            .dispatch_command(PlayerCommand::SetPlaybackRate(playback_rate))
            .unwrap();
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame(Duration::from_secs(1), 1));

        let plan = session.worker_wakeup_plan(
            Instant::now(),
            &tick_config,
            Duration::from_millis(2),
            Duration::from_millis(250),
        );
        let planned_delay = plan.delay.expect("future frame must create deadline");

        assert_eq!(plan.reason, WorkerWakeupReason::FramePtsDeadline);
        assert!(
            (min_delay..=max_delay).contains(&planned_delay),
            "rate-aware no-audio deadline {planned_delay:?} was outside {min_delay:?}..={max_delay:?}"
        );
    }
}

#[test]
fn front_frame_scheduler_delay_keeps_positive_deadline_nonzero_at_max_rate() {
    let mut session = PlayerSession::new();
    let now = Instant::now();
    let tick_config = PlayerTickConfig {
        video_present_lead_frames: 0.0,
        ..PlayerTickConfig::default()
    };

    session.set_playback_state(PlaybackState::Playing);
    session
        .dispatch_command(PlayerCommand::SetPlaybackRate(PlaybackRate::MAX))
        .unwrap();
    session
        .pipeline
        .start_monotonic_media_clock(Duration::ZERO, now, PlaybackRate::MAX);
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame(Duration::from_nanos(1), 1));

    let delay = front_frame_scheduler_delay(&session, &tick_config, now)
        .expect("queued future frame must create scheduler delay");

    assert_eq!(delay, Duration::from_nanos(1));
}

#[test]
fn front_frame_scheduler_delay_uses_audio_media_mapping_for_playback_rate() {
    fn audio_clock_scheduler_delay_for(playback_rate: PlaybackRate) -> Duration {
        let mut session = PlayerSession::new();
        let tick_config = PlayerTickConfig {
            video_present_lead_frames: 0.0,
            ..PlayerTickConfig::default()
        };

        session.set_playback_state(PlaybackState::Playing);
        session
            .pipeline
            .install_audio_clock(Arc::new(FixedAudioClock) as Arc<dyn PlayerAudioClock>);
        session
            .dispatch_command(PlayerCommand::SetPlaybackRate(playback_rate))
            .unwrap();
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame(Duration::from_secs(1), 1));

        front_frame_scheduler_delay(&session, &tick_config, Instant::now())
            .expect("queued future frame must create scheduler delay")
    }

    assert_eq!(
        audio_clock_scheduler_delay_for(PlaybackRate::NORMAL),
        Duration::from_secs(1)
    );
    assert_eq!(
        audio_clock_scheduler_delay_for(PlaybackRate::MAX),
        Duration::from_millis(250)
    );
    assert_eq!(
        audio_clock_scheduler_delay_for(
            PlaybackRate::new(0.5).expect("0.5x rate должен быть валиден")
        ),
        Duration::from_secs(2)
    );
}

#[test]
fn worker_wakeup_does_not_busy_spin_when_present_queue_is_healthy() {
    let mut session = PlayerSession::new();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.update_current_position(Duration::ZERO);
    let tick_config = PlayerTickConfig {
        target_video_present_queue: 4,
        max_video_present_queue: 8,
        ..PlayerTickConfig::default()
    };

    for frame_index in 0..video_present_queue_target(&tick_config) {
        session.pipeline.enqueue_queued_video_frame(decoded_frame(
            Duration::from_millis(100 + frame_index as u64 * 17),
            frame_index as u64,
        ));
    }
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_millis(180),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"future-video"),
            true,
        ));

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &tick_config,
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, WorkerWakeupReason::FramePtsDeadline);
    assert!(plan.delay.is_some_and(|delay| !delay.is_zero()));
}

#[test]
fn worker_wakeup_uses_audio_refill_deadline_before_coarse_progress() {
    let mut session = PlayerSession::new();
    let audio_track_id = TrackId::new(2);
    let tick_config = PlayerTickConfig {
        audio_buffer_high_water_mark_ms: 200.0,
        ..PlayerTickConfig::default()
    };

    install_empty_demuxer(&mut session);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.pipeline.select_audio_track(audio_track_id);
    install_fixed_audio_output(&mut session, 250.0);

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &tick_config,
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, WorkerWakeupReason::PipelineWorkReady);
    assert_eq!(plan.delay, Some(Duration::from_millis(50)));
}

#[test]
fn worker_wakeup_prefers_audio_refill_deadline_over_later_video_frame() {
    let mut session = PlayerSession::new();
    let audio_track_id = TrackId::new(2);
    let tick_config = PlayerTickConfig {
        audio_buffer_high_water_mark_ms: 200.0,
        ..PlayerTickConfig::default()
    };

    install_empty_demuxer(&mut session);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.update_current_position(Duration::ZERO);
    session.pipeline.select_audio_track(audio_track_id);
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame(Duration::from_millis(500), 1));
    install_fixed_audio_output(&mut session, 250.0);

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &tick_config,
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, WorkerWakeupReason::PipelineWorkReady);
    assert_eq!(plan.delay, Some(Duration::from_millis(50)));
}

#[test]
fn worker_wakeup_uses_audio_refill_deadline_while_draining_pending_audio_tail() {
    let mut session = PlayerSession::new();
    let audio_track_id = TrackId::new(2);
    let tick_config = PlayerTickConfig {
        audio_buffer_high_water_mark_ms: 200.0,
        ..PlayerTickConfig::default()
    };

    install_empty_demuxer(&mut session);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.pipeline.select_audio_track(audio_track_id);
    enqueue_pending_audio_packet(&mut session, audio_track_id);
    install_fixed_audio_output(&mut session, 250.0);
    session.enter_eof_drain();

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &tick_config,
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, WorkerWakeupReason::PipelineWorkReady);
    assert_eq!(plan.delay, Some(Duration::from_millis(50)));
}

#[test]
fn worker_wakeup_records_front_frame_pts_diff_for_diagnostics() {
    let mut session = PlayerSession::new();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.update_current_position(Duration::from_millis(10));
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame(Duration::from_millis(30), 1));

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &PlayerTickConfig::default(),
        Duration::from_millis(2),
        Duration::from_millis(250),
    );
    let diagnostics = plan.diagnostics(Duration::from_millis(3));

    assert_eq!(
        diagnostics.reason,
        Some(WorkerWakeupReason::FramePtsDeadline)
    );
    assert_eq!(diagnostics.tick_late_by, Duration::from_millis(3));
    assert!(diagnostics.frame_timing.is_some_and(|timing| {
        timing.front_frame_pts == Duration::from_millis(30)
            && timing.front_frame_delta_from_target_us > 0
    }));
}

#[test]
fn decoder_readiness_poll_keeps_seek_decoder_inflight_from_idling() {
    let poll_needed = decoder_readiness_poll_needed_for_state(DecoderReadinessPollState {
        video_track_selected: true,
        video_frame_queue_len: 0,
        video_present_queue_target: video_present_queue_target(&PlayerTickConfig::default()),
        worker_has_pending_video_packets: false,
        worker_has_demuxer: false,
        decoder_has_queued_packets: false,
        decoder_has_in_flight_packets: true,
        seek_commit_active: true,
    });

    assert!(poll_needed);
}

#[test]
fn decoder_readiness_poll_keeps_seek_decoder_inflight_when_present_queue_is_full() {
    let tick_config = PlayerTickConfig::default();
    let present_queue_target = video_present_queue_target(&tick_config);
    let poll_needed = decoder_readiness_poll_needed_for_state(DecoderReadinessPollState {
        video_track_selected: true,
        video_frame_queue_len: present_queue_target,
        video_present_queue_target: present_queue_target,
        worker_has_pending_video_packets: false,
        worker_has_demuxer: false,
        decoder_has_queued_packets: false,
        decoder_has_in_flight_packets: true,
        seek_commit_active: true,
    });

    assert!(poll_needed);
}

#[test]
fn decoder_readiness_poll_ignores_decoder_inflight_outside_seek() {
    let poll_needed = decoder_readiness_poll_needed_for_state(DecoderReadinessPollState {
        video_track_selected: true,
        video_frame_queue_len: 0,
        video_present_queue_target: video_present_queue_target(&PlayerTickConfig::default()),
        worker_has_pending_video_packets: false,
        worker_has_demuxer: false,
        decoder_has_queued_packets: false,
        decoder_has_in_flight_packets: true,
        seek_commit_active: false,
    });

    assert!(!poll_needed);
}

#[test]
fn active_accurate_preroll_with_full_decoder_queue_waits_for_decoder_activity() {
    let (_activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let tick_config = PlayerTickConfig {
        max_pending_video_packets: 4,
        ..PlayerTickConfig::default()
    };
    let decoder_thread =
        RecordingVideoDecoderThread::new_with_activity_snapshot(activity_subscription.snapshot())
            .with_packet_queue_depth(4);
    let mut session = PlayerSession::new();
    session.set_playback_state(PlaybackState::Seeking);
    start_accurate_seek_for_decoder_io(&mut session, decoder_thread, Duration::from_millis(500));
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(400),
        Bytes::from_static(b"seek-preroll-full-queue"),
        true,
    );

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &tick_config,
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, WorkerWakeupReason::DecodeReadiness);
    assert_eq!(plan.delay, Some(Duration::from_millis(2)));
    assert!(plan.wait_for_decoder_activity);
}

#[test]
fn active_accurate_preroll_with_decoder_queue_capacity_keeps_immediate_work() {
    let (_activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let tick_config = PlayerTickConfig {
        max_pending_video_packets: 4,
        ..PlayerTickConfig::default()
    };
    let decoder_thread =
        RecordingVideoDecoderThread::new_with_activity_snapshot(activity_subscription.snapshot())
            .with_packet_queue_depth(3);
    let mut session = PlayerSession::new();
    session.set_playback_state(PlaybackState::Seeking);
    start_accurate_seek_for_decoder_io(&mut session, decoder_thread, Duration::from_millis(500));
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(400),
        Bytes::from_static(b"seek-preroll-queue-has-capacity"),
        true,
    );

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &tick_config,
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, WorkerWakeupReason::SeekOrPreroll);
    assert_eq!(plan.delay, Some(Duration::ZERO));
    assert!(!plan.wait_for_decoder_activity);
}

#[test]
fn active_accurate_preroll_with_full_decoder_queue_keeps_immediate_drop_work() {
    let (_activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let tick_config = PlayerTickConfig {
        max_pending_video_packets: 4,
        ..PlayerTickConfig::default()
    };
    let decoder_thread =
        RecordingVideoDecoderThread::new_with_activity_snapshot(activity_subscription.snapshot())
            .with_packet_queue_depth(4);
    let mut session = PlayerSession::new();
    session.set_playback_state(PlaybackState::Seeking);
    let active_generation = start_accurate_seek_for_decoder_io(
        &mut session,
        decoder_thread,
        Duration::from_millis(500),
    );
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_millis(400),
            active_generation.saturating_sub(1),
            Bytes::from_static(b"stale-seek-preroll-packet"),
            true,
        ));

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &tick_config,
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, WorkerWakeupReason::SeekOrPreroll);
    assert_eq!(plan.delay, Some(Duration::ZERO));
    assert!(!plan.wait_for_decoder_activity);
}

#[test]
fn active_accurate_preroll_with_full_decoder_queue_keeps_immediate_unknown_h264_drop_work() {
    let (_activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let tick_config = PlayerTickConfig {
        max_pending_video_packets: 4,
        ..PlayerTickConfig::default()
    };
    let decoder_thread =
        RecordingVideoDecoderThread::new_with_activity_snapshot(activity_subscription.snapshot())
            .with_packet_queue_depth(4);
    let mut session = PlayerSession::new();
    session.set_playback_state(PlaybackState::Seeking);
    start_accurate_seek_for_decoder_io(&mut session, decoder_thread, Duration::from_millis(500));
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::H264),
    );
    session.pipeline.require_video_decoder_keyframe();
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(400),
        Bytes::from_static(b"h264-unknown-seek-preroll-packet"),
        PacketKeyframe::Unknown,
    );

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &tick_config,
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, WorkerWakeupReason::SeekOrPreroll);
    assert_eq!(plan.delay, Some(Duration::ZERO));
    assert!(!plan.wait_for_decoder_activity);
}

#[test]
fn unsupported_decoder_activity_uses_bounded_readiness_fallback() {
    let tick_config = PlayerTickConfig {
        max_pending_video_packets: 4,
        ..PlayerTickConfig::default()
    };
    let decoder_thread = RecordingVideoDecoderThread::new().with_packet_queue_depth(4);
    let mut session = PlayerSession::new();
    session.set_playback_state(PlaybackState::Seeking);
    start_accurate_seek_for_decoder_io(&mut session, decoder_thread, Duration::from_millis(500));
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(400),
        Bytes::from_static(b"seek-preroll-unsupported-activity"),
        true,
    );

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &tick_config,
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, WorkerWakeupReason::DecodeReadiness);
    assert_eq!(plan.delay, Some(Duration::from_millis(2)));
    assert!(!plan.wait_for_decoder_activity);
}

#[test]
fn absent_decoder_activity_status_uses_bounded_readiness_fallback() {
    let tick_config = PlayerTickConfig {
        max_pending_video_packets: 4,
        ..PlayerTickConfig::default()
    };
    let mut session = PlayerSession::new();
    session.set_playback_state(PlaybackState::Seeking);
    start_accurate_seek_for_decoder_io(
        &mut session,
        RecordingVideoDecoderThread::new().with_packet_queue_depth(4),
        Duration::from_millis(500),
    );
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(400),
        Bytes::from_static(b"seek-preroll-absent-activity-status"),
        true,
    );

    let plan = session.worker_wakeup_plan_with_decoder_activity_status(
        Instant::now(),
        &tick_config,
        Duration::from_millis(2),
        Duration::from_millis(250),
        &crate::pipeline::VideoDecoderActivityStatus::AbsentDecoder,
    );

    assert_eq!(plan.reason, WorkerWakeupReason::DecodeReadiness);
    assert_eq!(plan.delay, Some(Duration::from_millis(2)));
    assert!(!plan.wait_for_decoder_activity);
}

#[test]
fn immediate_video_work_keeps_zero_wakeup_without_decoder_activity_wait() {
    let (_activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let tick_config = PlayerTickConfig {
        max_pending_video_packets: 4,
        ..PlayerTickConfig::default()
    };
    let decoder_thread =
        RecordingVideoDecoderThread::new_with_activity_snapshot(activity_subscription.snapshot())
            .with_packet_queue_depth(4);
    let mut session = PlayerSession::new();
    session.set_playback_state(PlaybackState::Playing);
    session.pipeline.set_video_decoder_thread(decoder_thread);
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(120),
        Bytes::from_static(b"ready-video-work"),
        true,
    );

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &tick_config,
        Duration::from_millis(2),
        Duration::from_millis(250),
    );

    assert_eq!(plan.reason, WorkerWakeupReason::PipelineWorkReady);
    assert_eq!(plan.delay, Some(Duration::ZERO));
    assert!(!plan.wait_for_decoder_activity);
}

#[test]
fn software_host_backpressure_does_not_schedule_immediate_video_work() {
    let tick_config = PlayerTickConfig::default();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut session = PlayerSession::new();

    decoder_thread.set_host_upload_resource_status(
        video_core::HostUploadResourceSnapshotStatus::Available(
            video_core::HostUploadResourceSnapshot {
                host_frames_ready: tick_config.decoder_ready_queue_frames,
                host_frames_in_flight: 0,
                upload_slots_capacity: 2,
                upload_slots_free: 2,
                upload_failures: 0,
            },
        ),
    );
    session.pipeline.set_video_decoder_thread(decoder_thread);
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(120),
        Bytes::from_static(b"blocked-video-work"),
        true,
    );

    assert!(!pending_video_work_available(&session, &tick_config));
}

#[test]
fn scheduler_late_drop_requires_next_frame_to_be_ready() {
    let mut queue = VecDeque::new();
    queue.push_back(decoded_frame(Duration::from_millis(0), 1));
    queue.push_back(decoded_frame(Duration::from_millis(33), 2));

    let should_drop = should_drop_front_frame_as_late(
        queue.front().zip(queue.get(1)),
        Duration::from_millis(70),
        Duration::from_millis(16),
    );

    assert!(should_drop);
}

#[test]
fn scheduler_keeps_late_frame_without_replacement() {
    let mut queue = VecDeque::new();
    queue.push_back(decoded_frame(Duration::from_millis(0), 1));

    let should_drop = should_drop_front_frame_as_late(
        queue.front().zip(queue.get(1)),
        Duration::from_millis(70),
        Duration::from_millis(16),
    );

    assert!(!should_drop);
}

#[test]
fn scheduler_5994_vs_60hz_phase_difference_is_not_late_drop() {
    let mut queue = VecDeque::new();
    queue.push_back(decoded_frame(Duration::ZERO, 1));
    queue.push_back(decoded_frame(Duration::from_micros(16_683), 2));

    let should_drop = should_drop_front_frame_as_late(
        queue.front().zip(queue.get(1)),
        Duration::from_micros(16_667),
        Duration::from_micros(33_366),
    );

    assert!(!should_drop);
}

#[test]
fn scheduler_repeats_current_frame_when_queue_is_empty() {
    let mut session = PlayerSession::new();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .pipeline
        .set_present_video_frame(decoded_frame(Duration::ZERO, 1));

    let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

    assert_eq!(tick_result.video_frames_repeated, 1);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::ZERO)
    );
    assert_eq!(
        session
            .snapshot_with_frame_counters(crate::FrameCounters::default())
            .diagnostics
            .repeated_video_frames,
        1
    );
    assert_eq!(
        session
            .snapshot_with_frame_counters(crate::FrameCounters::default())
            .diagnostics
            .drops
            .late,
        0
    );
}

#[test]
fn scheduler_preserves_p010_boundary_frame_without_format_branching() {
    let mut session = PlayerSession::new();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_with_format(
            Duration::ZERO,
            10,
            video_core::DecodedPixelFormat::P010,
        ));

    let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

    assert_eq!(tick_result.video_frames_presented, 1);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.format()),
        Some(video_core::DecodedPixelFormat::P010)
    );
    assert!(session.pipeline.video_present_queue_is_empty());
}

#[test]
fn decoder_thread_error_maps_to_runtime_player_error() {
    let decode_thread_error = DecodeThreadError::new("P010 DMA-BUF zero-copy import failed");

    let player_error = player_error_from_decode_thread_error(&decode_thread_error);

    assert_eq!(player_error.kind, PlayerErrorKind::RuntimeError);
    assert!(
        player_error
            .message
            .contains("P010 DMA-BUF zero-copy import failed")
    );
}

#[test]
fn audio_demux_catchup_requires_selected_audio_and_low_buffer() {
    assert!(audio_demux_catchup_needed_for_level(true, Some(40.0), 50.0));
    assert!(!audio_demux_catchup_needed_for_level(
        true,
        Some(60.0),
        50.0
    ));
    assert!(!audio_demux_catchup_needed_for_level(
        false,
        Some(40.0),
        50.0
    ));
    assert!(!audio_demux_catchup_needed_for_level(true, None, 50.0));
}

#[test]
fn scheduler_requests_catch_up_after_one_delayed_tick() {
    let mut session = PlayerSession::new();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    let tick_config = PlayerTickConfig::default();
    let frame_duration = session.pipeline.video_frame_duration_estimate();

    for frame_index in 0..video_present_queue_target(&tick_config) {
        session.pipeline.enqueue_queued_video_frame(decoded_frame(
            frame_duration.mul_f64(frame_index as f64),
            frame_index as u64,
        ));
    }

    assert_eq!(
        adaptive_catch_up_frame_need(&session, &tick_config, frame_duration),
        1
    );
    assert!(adaptive_catch_up_needed(
        &session,
        &tick_config,
        frame_duration
    ));
}

#[test]
fn scheduler_allows_decoder_burst_after_delay() {
    let mut session = PlayerSession::new();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    let tick_config = PlayerTickConfig {
        max_decoded_video_frames_drained_per_tick: 2,
        max_video_present_queue: 8,
        min_video_present_queue: 2,
        target_video_present_queue: 4,
        ..PlayerTickConfig::default()
    };

    let budget = adaptive_catch_up_budget(
        &session,
        &tick_config,
        session.pipeline.video_frame_duration_estimate(),
    );

    assert!(budget.decoded_frames > tick_config.max_decoded_video_frames_drained_per_tick);
    assert_eq!(budget.decoded_frames, 7);
}

#[test]
fn scheduler_requests_catch_up_when_present_queue_is_near_empty() {
    let mut session = PlayerSession::new();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    let tick_config = PlayerTickConfig {
        min_video_present_queue: 2,
        target_video_present_queue: 4,
        max_video_present_queue: 8,
        ..PlayerTickConfig::default()
    };
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame(Duration::ZERO, 1));

    assert_eq!(
        adaptive_catch_up_frame_need(&session, &tick_config, Duration::ZERO),
        3
    );
    assert!(adaptive_catch_up_needed(
        &session,
        &tick_config,
        Duration::ZERO
    ));
}

#[test]
fn scheduler_does_not_catch_up_when_present_queue_is_full() {
    let mut session = PlayerSession::new();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    let tick_config = PlayerTickConfig {
        max_video_present_queue: 4,
        min_video_present_queue: 2,
        target_video_present_queue: 3,
        ..PlayerTickConfig::default()
    };

    for frame_index in 0..video_present_queue_limit(&tick_config) {
        session.pipeline.enqueue_queued_video_frame(decoded_frame(
            Duration::from_millis(frame_index as u64),
            frame_index as u64,
        ));
    }

    assert!(!adaptive_catch_up_needed(
        &session,
        &tick_config,
        session.pipeline.video_frame_duration_estimate()
    ));
}

#[test]
fn seek_generation_transition_is_not_counted_as_late_drop() {
    let mut pipeline = PlaybackPipeline::default();
    let stale_generation = pipeline.seek_generation();
    let current_generation = pipeline.begin_seek_generation();

    assert_eq!(
        pending_video_packet_generation_drop_reason(&pipeline, stale_generation),
        Some(PlayerVideoDropReason::StaleGeneration)
    );
    assert_eq!(
        pending_video_packet_generation_drop_reason(&pipeline, current_generation),
        None
    );
}

#[test]
fn seek_video_preroll_is_capped_by_present_queue_capacity() {
    let tick_config = PlayerTickConfig {
        max_video_present_queue: 1,
        seek_resume_video_min_ready_frames: 8,
        ..PlayerTickConfig::default()
    };

    assert_eq!(
        effective_seek_resume_video_min_ready_frames(&tick_config),
        2
    );
}

#[test]
fn audio_demux_catchup_uses_bounded_video_packet_limit() {
    let mut session = PlayerSession::new();
    let tick_config = PlayerTickConfig {
        max_pending_video_packets: 2,
        max_pending_video_packets_during_audio_catchup: 4,
        ..PlayerTickConfig::default()
    };

    for packet_index in 0..tick_config.max_pending_video_packets {
        session
            .pipeline
            .enqueue_pending_video_packet(PendingVideoPacket::new(
                TrackId::new(1),
                Duration::from_millis(packet_index as u64),
                session.pipeline.seek_generation(),
                Bytes::new(),
                false,
            ));
    }

    assert!(!can_read_next_demux_packet_with_audio_priority(
        &session,
        &tick_config,
        false
    ));
    assert!(can_read_next_demux_packet_with_audio_priority(
        &session,
        &tick_config,
        true
    ));

    for packet_index in tick_config.max_pending_video_packets
        ..tick_config.max_pending_video_packets_during_audio_catchup
    {
        session
            .pipeline
            .enqueue_pending_video_packet(PendingVideoPacket::new(
                TrackId::new(1),
                Duration::from_millis(packet_index as u64),
                session.pipeline.seek_generation(),
                Bytes::new(),
                false,
            ));
    }

    assert!(!can_read_next_demux_packet_with_audio_priority(
        &session,
        &tick_config,
        true
    ));
}

#[test]
fn audio_catchup_backlog_shed_clears_video_queue_and_waits_for_keyframe() {
    let mut session = PlayerSession::new();

    for packet_index in 0..5u64 {
        session
            .pipeline
            .enqueue_pending_video_packet(PendingVideoPacket::new(
                TrackId::new(1),
                Duration::from_millis(packet_index),
                session.pipeline.seek_generation(),
                Bytes::new(),
                false,
            ));
    }

    let shed_packets = session
        .pipeline
        .shed_pending_video_backlog_for_audio_catchup();
    assert_eq!(shed_packets, 5);
    assert!(session.pipeline.pending_video_packet_is_empty());

    // После shed non-keyframe packets отбрасываются до первого keyframe.
    assert!(
        session
            .pipeline
            .video_admission_should_drop_for_keyframe_resync(PacketKeyframe::NotKeyframe)
    );
    assert!(
        session
            .pipeline
            .video_admission_should_drop_for_keyframe_resync(PacketKeyframe::NotKeyframe)
    );
    assert!(
        !session
            .pipeline
            .video_admission_should_drop_for_keyframe_resync(PacketKeyframe::Keyframe)
    );

    // Keyframe закрыл resync: обычный поток снова не фильтруется.
    assert!(
        !session
            .pipeline
            .video_admission_should_drop_for_keyframe_resync(PacketKeyframe::NotKeyframe)
    );
}

#[test]
fn keyframe_resync_accepts_unknown_classification_to_avoid_frozen_video() {
    let mut session = PlayerSession::new();
    session
        .pipeline
        .shed_pending_video_backlog_for_audio_catchup();

    assert!(
        !session
            .pipeline
            .video_admission_should_drop_for_keyframe_resync(PacketKeyframe::Unknown)
    );
    assert!(
        !session
            .pipeline
            .video_admission_should_drop_for_keyframe_resync(PacketKeyframe::NotKeyframe)
    );
}

#[test]
fn clear_pending_video_packets_resets_keyframe_resync_wait() {
    let mut session = PlayerSession::new();
    session
        .pipeline
        .shed_pending_video_backlog_for_audio_catchup();

    // Seek/media reset проходят через clear_pending_video_packets и снимают ожидание.
    session.pipeline.clear_pending_video_packets();
    assert!(
        !session
            .pipeline
            .video_admission_should_drop_for_keyframe_resync(PacketKeyframe::NotKeyframe)
    );
}

#[test]
fn selected_audio_without_output_allows_demux_until_first_audio_packet() {
    let mut session = PlayerSession::new();
    let audio_track_id = TrackId::new(2);
    let tick_config = PlayerTickConfig {
        max_pending_video_packets: 1,
        max_pending_video_packets_during_audio_catchup: 2,
        ..PlayerTickConfig::default()
    };

    install_empty_demuxer(&mut session);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.pipeline.select_audio_track(audio_track_id);
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::ZERO,
            session.pipeline.seek_generation(),
            Bytes::new(),
            true,
        ));

    assert!(can_read_next_demux_packet_with_audio_priority(
        &session,
        &tick_config,
        false
    ));
    assert!(demux_work_available(&session, &tick_config));

    enqueue_pending_audio_packet(&mut session, audio_track_id);

    assert!(!can_read_next_demux_packet_with_audio_priority(
        &session,
        &tick_config,
        false
    ));
    assert!(!demux_work_available(&session, &tick_config));
}

#[test]
fn high_water_audio_blocks_more_demux_when_pending_audio_waits() {
    let mut session = PlayerSession::new();
    let audio_track_id = TrackId::new(2);
    let tick_config = PlayerTickConfig {
        audio_buffer_high_water_mark_ms: 100.0,
        ..PlayerTickConfig::default()
    };

    install_empty_demuxer(&mut session);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.pipeline.select_audio_track(audio_track_id);
    install_fixed_audio_output(&mut session, 250.0);
    enqueue_pending_audio_packet(&mut session, audio_track_id);

    assert!(!can_read_next_demux_packet_with_audio_priority(
        &session,
        &tick_config,
        false
    ));
    assert!(!demux_work_available(&session, &tick_config));
    assert_eq!(
        demux_backpressure_reason(&session, &tick_config, false),
        Some(PipelinePauseReason::DemuxBackpressure)
    );
}

#[test]
fn audio_only_high_water_blocks_demux_when_pending_audio_is_empty() {
    let mut session = PlayerSession::new();
    let audio_track_id = TrackId::new(2);
    let tick_config = PlayerTickConfig {
        audio_buffer_high_water_mark_ms: 100.0,
        ..PlayerTickConfig::default()
    };

    install_empty_demuxer(&mut session);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.pipeline.select_audio_track(audio_track_id);
    install_fixed_audio_output(&mut session, 250.0);

    assert!(session.pipeline.pending_audio_packet_is_empty());
    assert!(!can_read_next_demux_packet_with_audio_priority(
        &session,
        &tick_config,
        false
    ));
    assert!(!demux_work_available(&session, &tick_config));
    assert_eq!(
        demux_backpressure_reason(&session, &tick_config, false),
        Some(PipelinePauseReason::DemuxBackpressure)
    );
}

#[test]
fn low_water_audio_catchup_still_allows_bounded_demux() {
    let mut session = PlayerSession::new();
    let audio_track_id = TrackId::new(2);
    let tick_config = PlayerTickConfig {
        max_pending_video_packets: 1,
        max_pending_video_packets_during_audio_catchup: 2,
        audio_buffer_high_water_mark_ms: 300.0,
        audio_demux_low_water_mark_ms: 100.0,
        ..PlayerTickConfig::default()
    };

    install_empty_demuxer(&mut session);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.pipeline.select_audio_track(audio_track_id);
    install_fixed_audio_output(&mut session, 40.0);
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::ZERO,
            session.pipeline.seek_generation(),
            Bytes::new(),
            true,
        ));

    assert!(audio_demux_catchup_needed(&session, &tick_config));
    assert!(!can_read_next_demux_packet_with_audio_priority(
        &session,
        &tick_config,
        false
    ));
    assert!(can_read_next_demux_packet_with_audio_priority(
        &session,
        &tick_config,
        true
    ));
    assert!(demux_work_available(&session, &tick_config));
}

#[test]
fn video_queue_pressure_does_not_block_selected_audio_bootstrap() {
    let mut session = PlayerSession::new();
    let tick_config = PlayerTickConfig {
        max_video_present_queue: 1,
        min_video_present_queue: 1,
        target_video_present_queue: 1,
        ..PlayerTickConfig::default()
    };

    install_empty_demuxer(&mut session);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.pipeline.select_audio_track(TrackId::new(2));
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame(Duration::ZERO, 1));

    assert!(demux_work_available(&session, &tick_config));
    assert!(immediate_pipeline_work_available(&session, &tick_config));
}

#[test]
fn video_only_demux_admission_still_waits_for_present_queue() {
    let mut session = PlayerSession::new();
    let tick_config = PlayerTickConfig {
        max_video_present_queue: 1,
        min_video_present_queue: 1,
        target_video_present_queue: 1,
        ..PlayerTickConfig::default()
    };

    install_empty_demuxer(&mut session);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame(Duration::ZERO, 1));

    assert!(!can_read_next_demux_packet_with_audio_priority(
        &session,
        &tick_config,
        false
    ));
    assert!(!demux_work_available(&session, &tick_config));
    assert!(!immediate_pipeline_work_available(&session, &tick_config));
    assert_eq!(
        demux_backpressure_reason(&session, &tick_config, false),
        Some(PipelinePauseReason::WaitingForPresentQueue)
    );
}

#[test]
fn demux_backpressure_reports_present_queue_reason() {
    let mut session = PlayerSession::new();
    let tick_config = PlayerTickConfig {
        max_video_present_queue: 1,
        ..PlayerTickConfig::default()
    };
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame(Duration::ZERO, 1));

    let reason = demux_backpressure_reason(&session, &tick_config, false);

    assert_eq!(reason, Some(PipelinePauseReason::WaitingForPresentQueue));
}

#[test]
fn demux_backpressure_reports_audio_priority_reason() {
    let mut session = PlayerSession::new();
    let tick_config = PlayerTickConfig {
        max_pending_video_packets: 2,
        max_pending_video_packets_during_audio_catchup: 4,
        ..PlayerTickConfig::default()
    };

    for packet_index in 0..audio_catchup_pending_video_limit(&tick_config) {
        session
            .pipeline
            .enqueue_pending_video_packet(PendingVideoPacket::new(
                TrackId::new(1),
                Duration::from_millis(packet_index as u64),
                session.pipeline.seek_generation(),
                Bytes::new(),
                false,
            ));
    }

    let reason = demux_backpressure_reason(&session, &tick_config, true);

    assert_eq!(
        reason,
        Some(PipelinePauseReason::WaitingForDemuxAudioPriority)
    );
}

#[test]
fn read_demux_packets_without_demuxer_is_noop() {
    let mut session = PlayerSession::new();
    let tick_config = PlayerTickConfig::default();
    let mut tick_result = PlayerTickResult::default();

    session.dispatch_command(PlayerCommand::Play).unwrap();

    let packets_read = read_demux_packets(
        &mut session,
        &tick_config,
        &mut tick_result,
        tick_config.max_demux_packets_per_tick,
        None,
    );

    assert_eq!(packets_read, 0);
    assert!(tick_result.demuxed_packets.is_empty());
    assert!(!tick_result.demux_backpressured);
    assert!(session.snapshot().last_error.is_none());
}

#[test]
fn route_demuxed_audio_packet_preserves_shared_payload_and_metadata() {
    let mut session = PlayerSession::new();
    let payload = Bytes::from(vec![0x4f, 0x70, 0x75, 0x73]);
    let payload_ptr = payload.as_ptr();
    let time_base = media_core::TimeBase::new(1, 48_000).expect("valid audio time base");
    let packet = media_core::Packet::new(
        TrackId::new(2),
        TrackKind::Audio,
        Duration::from_millis(42),
        None,
        false,
        payload.clone(),
    )
    .with_track_timestamps(
        Some(media_core::TrackTimestamp::new(
            TrackId::new(2),
            2_016,
            time_base,
        )),
        Some(media_core::TrackTimestamp::new(
            TrackId::new(2),
            1_920,
            time_base,
        )),
    )
    .with_track_duration(media_core::TrackDuration::new(
        TrackId::new(2),
        960,
        time_base,
    ));

    route_demuxed_packet(&mut session, packet);

    let pending_packet = session
        .pipeline
        .pop_pending_audio_packet_front()
        .expect("audio packet должен попасть в pending audio queue");

    assert_eq!(pending_packet.track_id, TrackId::new(2));
    assert_eq!(pending_packet.pts, Duration::from_millis(42));
    assert_eq!(
        pending_packet.generation,
        session.pipeline.seek_generation()
    );
    assert_eq!(pending_packet.timing.pts_units(), 2_016);
    assert_eq!(pending_packet.timing.dts_units(), Some(1_920));
    assert_eq!(pending_packet.timing.duration_units(), Some(960));
    assert_eq!(
        pending_packet
            .timing
            .time_base()
            .expect("audio pending packet должен сохранить raw time base")
            .denom(),
        48_000
    );
    assert_eq!(pending_packet.encoded_bytes.as_ptr(), payload_ptr);
    assert_eq!(&pending_packet.encoded_bytes[..], b"Opus");
}

#[test]
fn route_demuxed_video_packet_preserves_shared_payload_keyframe_and_pts() {
    let mut session = PlayerSession::new();
    let payload = Bytes::from(vec![0x82, 0x49, 0x83, 0x42]);
    let payload_ptr = payload.as_ptr();
    let time_base = media_core::TimeBase::new(1, 90_000).expect("valid video time base");
    let packet = media_core::Packet::new(
        TrackId::new(1),
        TrackKind::Video,
        Duration::from_millis(120),
        Some(Duration::from_millis(80)),
        true,
        payload.clone(),
    )
    .with_track_timestamps(
        Some(media_core::TrackTimestamp::new(
            TrackId::new(1),
            10_800,
            time_base,
        )),
        Some(media_core::TrackTimestamp::new(
            TrackId::new(1),
            7_200,
            time_base,
        )),
    );

    route_demuxed_packet(&mut session, packet);

    let pending_packet = session
        .pipeline
        .pop_pending_video_packet_front()
        .expect("video packet должен попасть в pending video queue");

    assert_eq!(pending_packet.track_id, TrackId::new(1));
    assert_eq!(pending_packet.pts, Duration::from_millis(120));
    assert_eq!(
        pending_packet.generation,
        session.pipeline.seek_generation()
    );
    assert_eq!(pending_packet.dts, Some(Duration::from_millis(80)));
    assert_eq!(
        pending_packet
            .track_dts
            .expect("video pending packet должен сохранить raw DTS")
            .units
            .get(),
        7_200
    );
    assert_eq!(pending_packet.keyframe, PacketKeyframe::Keyframe);
    assert_eq!(pending_packet.encoded_bytes.as_ptr(), payload_ptr);
    assert_eq!(&pending_packet.encoded_bytes[..], &[0x82, 0x49, 0x83, 0x42]);
}

#[test]
fn absent_decoder_send_is_noop_and_drain_drops_pending_packets() {
    let mut session = PlayerSession::new();
    let mut tick_result = PlayerTickResult::default();

    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(120),
        Bytes::from_static(b"pending-without-decoder"),
        true,
    );

    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(sent_packets, 0);
    assert!(!session.pipeline.pending_video_packet_is_empty());
    assert!(tick_result.dropped_video_frames.is_empty());

    let drained_frames = drain_decoded_video_frames(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(1, 0),
        None,
    );

    assert_eq!(drained_frames, 0);
    assert!(session.pipeline.pending_video_packet_is_empty());
    assert_eq!(
        tick_result.dropped_video_frames,
        vec![PlayerVideoFrameDrop {
            pts: Duration::from_millis(120),
            reason: PlayerVideoDropReason::DecoderStarvation,
        }]
    );
}

#[test]
fn pending_video_packet_preserves_dts_through_decode_boundary() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();
    let time_base = media_core::TimeBase::new(1, 90_000).expect("valid video time base");
    let track_dts = media_core::TrackTimestamp::new(TrackId::new(1), 7_200, time_base);

    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new_with_decode_timestamps(
            TrackId::new(1),
            Duration::from_millis(120),
            Some(Duration::from_millis(80)),
            Some(track_dts),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"decode-order-video"),
            true,
        ));

    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(sent_packets, 1);
    let sent_packet = decoder_thread
        .sent_packets()
        .pop()
        .expect("decoder должен получить packet");
    assert_eq!(sent_packet.pts, Duration::from_millis(120));
    assert_eq!(sent_packet.dts, Some(Duration::from_millis(80)));
    assert_eq!(sent_packet.track_dts, Some(track_dts));
}

#[test]
fn pending_video_packet_from_unselected_track_is_dropped_before_decoder_send() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();

    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(2),
            Duration::from_millis(120),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"foreign-video-track"),
            true,
        ));

    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(sent_packets, 0);
    assert!(session.pipeline.pending_video_packet_is_empty());
    assert!(decoder_thread.sent_packets().is_empty());
    assert!(tick_result.dropped_video_frames.is_empty());
}

#[test]
fn stale_pending_video_packet_is_dropped_as_stale_generation_before_decoder_send() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();
    let stale_generation = session.pipeline.seek_generation();

    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    session.pipeline.begin_seek_generation();
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_millis(120),
            stale_generation,
            Bytes::from_static(b"stale-video"),
            true,
        ));

    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(sent_packets, 0);
    assert!(session.pipeline.pending_video_packet_is_empty());
    assert!(decoder_thread.sent_packets().is_empty());
    assert_eq!(
        tick_result.dropped_video_frames,
        vec![PlayerVideoFrameDrop {
            pts: Duration::from_millis(120),
            reason: PlayerVideoDropReason::StaleGeneration,
        }]
    );
}

#[test]
fn decoder_packet_send_backpressure_records_typed_pause_reason() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();

    decoder_thread.push_send_result(Err(DecodeSendError::Backpressure(
        DecodeBackpressureReason::PacketQueueFull {
            queued_packets: 4,
            capacity: 4,
        },
    )));
    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(120),
        Bytes::from_static(b"backpressured-video"),
        true,
    );

    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(sent_packets, 0);
    assert!(!session.pipeline.pending_video_packet_is_empty());
    assert!(decoder_thread.sent_packets().is_empty());
    assert_eq!(
        tick_result.pipeline_pauses,
        vec![PlayerPipelinePause {
            reason: PipelinePauseReason::DecoderPacketQueueFull,
        }]
    );
}

#[test]
fn software_host_ready_queue_backpressure_stops_packet_send() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();

    decoder_thread.set_host_upload_resource_status(
        video_core::HostUploadResourceSnapshotStatus::Available(
            video_core::HostUploadResourceSnapshot {
                host_frames_ready: PlayerTickConfig::default().decoder_ready_queue_frames,
                host_frames_in_flight: 0,
                upload_slots_capacity: 2,
                upload_slots_free: 2,
                upload_failures: 0,
            },
        ),
    );
    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(120),
        Bytes::from_static(b"software-ready-queue-full"),
        true,
    );

    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(sent_packets, 0);
    assert!(decoder_thread.sent_packets().is_empty());
    assert_eq!(
        tick_result.pipeline_pauses,
        vec![PlayerPipelinePause {
            reason: PipelinePauseReason::HostUploadReadyQueueFull,
        }]
    );
    assert!(!session.pipeline.pending_video_packet_is_empty());
}

#[test]
fn software_upload_slots_freed_resume_packet_send() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut blocked_tick = PlayerTickResult::default();

    decoder_thread.set_host_upload_resource_status(
        video_core::HostUploadResourceSnapshotStatus::Available(
            video_core::HostUploadResourceSnapshot {
                host_frames_ready: 0,
                host_frames_in_flight: 2,
                upload_slots_capacity: 2,
                upload_slots_free: 0,
                upload_failures: 0,
            },
        ),
    );
    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(120),
        Bytes::from_static(b"software-upload-slot-resume"),
        true,
    );

    let blocked_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut blocked_tick,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(blocked_packets, 0);
    assert!(decoder_thread.sent_packets().is_empty());
    assert_eq!(
        blocked_tick.pipeline_pauses,
        vec![PlayerPipelinePause {
            reason: PipelinePauseReason::HostUploadSlotsExhausted,
        }]
    );

    decoder_thread.set_host_upload_resource_status(
        video_core::HostUploadResourceSnapshotStatus::Available(
            video_core::HostUploadResourceSnapshot {
                host_frames_ready: 0,
                host_frames_in_flight: 1,
                upload_slots_capacity: 2,
                upload_slots_free: 1,
                upload_failures: 0,
            },
        ),
    );
    let mut resumed_tick = PlayerTickResult::default();
    let resumed_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut resumed_tick,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(resumed_packets, 1);
    assert_eq!(decoder_thread.sent_packets().len(), 1);
    assert!(resumed_tick.pipeline_pauses.is_empty());
    assert!(session.pipeline.pending_video_packet_is_empty());
}

#[test]
fn software_decoder_control_backpressure_stops_packet_send() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();

    decoder_thread.set_host_upload_resource_status(
        video_core::HostUploadResourceSnapshotStatus::Available(
            video_core::HostUploadResourceSnapshot {
                host_frames_ready: 0,
                host_frames_in_flight: 0,
                upload_slots_capacity: 2,
                upload_slots_free: 2,
                upload_failures: 0,
            },
        ),
    );
    decoder_thread.set_control_pressure(crate::DecoderControlChannelPressureSnapshot {
        control_channel_len: 4,
        control_channel_capacity: 4,
        control_channel_full_count: 1,
        release_control_send_fail_count: 1,
        flush_control_send_fail_count: 0,
    });
    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(120),
        Bytes::from_static(b"software-control-full"),
        true,
    );

    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(sent_packets, 0);
    assert!(decoder_thread.sent_packets().is_empty());
    assert_eq!(
        tick_result.pipeline_pauses,
        vec![PlayerPipelinePause {
            reason: PipelinePauseReason::DecoderControlQueueFull,
        }]
    );
    assert!(!session.pipeline.pending_video_packet_is_empty());
}

#[test]
fn applied_output_floor_bypasses_texture_gate_only_for_pre_target_preroll_packet() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();
    let target_position = Duration::from_millis(200);

    decoder_thread.set_resource_snapshot(decoder_resource_snapshot_for_tests(1, 1));
    let generation =
        start_accurate_seek_for_decoder_io(&mut session, decoder_thread.clone(), target_position);
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_millis(120),
            generation,
            Bytes::from_static(b"pre-target-preroll"),
            true,
        ));

    let preroll_sent = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(preroll_sent, 1);
    assert_eq!(decoder_thread.sent_packets().len(), 1);
    assert!(tick_result.pipeline_pauses.is_empty());

    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            target_position,
            generation,
            Bytes::from_static(b"target-video"),
            PacketKeyframe::NotKeyframe,
        ));
    let target_sent = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(target_sent, 0);
    assert_eq!(decoder_thread.sent_packets().len(), 1);
    assert!(
        tick_result
            .pipeline_pauses
            .iter()
            .any(|pause| { pause.reason == PipelinePauseReason::WaitingForFreeSurface })
    );
}

#[test]
fn applied_output_floor_does_not_bypass_decoder_packet_queue_backpressure() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();
    let target_position = Duration::from_millis(200);

    decoder_thread.set_resource_snapshot(decoder_resource_snapshot_for_tests(1, 1));
    decoder_thread.push_send_result(Err(DecodeSendError::Backpressure(
        DecodeBackpressureReason::PacketQueueFull {
            queued_packets: 4,
            capacity: 4,
        },
    )));
    let generation =
        start_accurate_seek_for_decoder_io(&mut session, decoder_thread.clone(), target_position);
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_millis(120),
            generation,
            Bytes::from_static(b"pre-target-preroll"),
            true,
        ));

    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(sent_packets, 0);
    assert!(!session.pipeline.pending_video_packet_is_empty());
    assert!(decoder_thread.sent_packets().is_empty());
    assert_eq!(
        tick_result.pipeline_pauses,
        vec![PlayerPipelinePause {
            reason: PipelinePauseReason::DecoderPacketQueueFull,
        }]
    );
}

#[test]
fn suppressed_preroll_diagnostic_increments_matching_active_seek_counter_only() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();
    let target_position = Duration::from_millis(200);
    let generation =
        start_accurate_seek_for_decoder_io(&mut session, decoder_thread.clone(), target_position);

    decoder_thread.push_diagnostic_event(
        video_core::VideoDecoderDiagnosticEvent::SeekPrerollFrameSuppressed {
            pts: Duration::from_millis(120),
            generation,
            floor_pts: target_position,
        },
    );
    decoder_thread.push_diagnostic_event(
        video_core::VideoDecoderDiagnosticEvent::SeekPrerollFrameSuppressed {
            pts: Duration::from_millis(130),
            generation: generation.saturating_add(1),
            floor_pts: target_position,
        },
    );

    let drained_frames = drain_decoded_video_frames(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 0),
        None,
    );
    let diagnostics = session
        .active_seek_diagnostics(Instant::now(), &PlayerTickConfig::default())
        .expect("active seek diagnostics должны оставаться доступны");

    assert_eq!(drained_frames, 0);
    assert_eq!(
        diagnostics
            .accurate_preroll
            .counters
            .decoded_pre_target_frames_dropped,
        1
    );
    assert!(tick_result.dropped_video_frames.is_empty());
}

#[test]
fn decoder_packet_send_fatal_error_marks_session_failed() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();

    decoder_thread.push_send_result(Err(DecodeSendError::Fatal(DecodeThreadError::new(
        "fatal send failure",
    ))));
    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(120),
        Bytes::from_static(b"fatal-video"),
        true,
    );

    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(sent_packets, 0);
    assert!(!session.pipeline.pending_video_packet_is_empty());
    assert!(decoder_thread.sent_packets().is_empty());
    let last_error = session
        .snapshot()
        .last_error
        .as_ref()
        .expect("fatal decoder send должен записать last_error");
    assert_eq!(last_error.kind, PlayerErrorKind::RuntimeError);
    assert!(last_error.message.contains("fatal send failure"));
}

#[test]
fn stale_decoded_video_frame_is_dropped_before_presentation_queue() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();
    let stale_generation = session.pipeline.seek_generation();

    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    session.pipeline.begin_seek_generation();
    let mut stale_frame = decoded_frame(Duration::from_millis(120), 77);
    stale_frame.generation = stale_generation;
    decoder_thread.push_decoded_frame(stale_frame);

    let drained_frames = drain_decoded_video_frames(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(1, 0),
        None,
    );

    assert_eq!(drained_frames, 1);
    assert!(session.pipeline.video_present_queue_is_empty());
    assert_eq!(
        decoder_thread.released_handles(),
        vec![video_core::FrameResourceHandle(77)]
    );
    assert_eq!(
        tick_result.dropped_video_frames,
        vec![PlayerVideoFrameDrop {
            pts: Duration::from_millis(120),
            reason: PlayerVideoDropReason::StaleGeneration,
        }]
    );
}

#[test]
fn decoded_frame_contract_mismatch_is_reported_without_renderer_knowledge() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();

    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_bit_depth(codec_core::BitDepth::Eight)
            .with_chroma(codec_core::ChromaSubsampling::Yuv420),
    );
    decoder_thread.push_decoded_frame(decoded_frame_with_format(
        Duration::from_millis(120),
        78,
        video_core::DecodedPixelFormat::P010,
    ));

    let drained_frames = drain_decoded_video_frames(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(1, 0),
        None,
    );

    assert_eq!(drained_frames, 1);
    assert!(session.pipeline.video_present_queue_is_empty());
    assert_eq!(
        decoder_thread.released_handles(),
        vec![video_core::FrameResourceHandle(78)]
    );
    let last_error = session
        .snapshot()
        .last_error
        .as_ref()
        .expect("contract mismatch must be reported to player state");
    assert_eq!(last_error.kind, PlayerErrorKind::RuntimeError);
    assert!(last_error.message.contains("contract mismatch"));
}

#[test]
fn unknown_keyframe_bootstrap_packet_is_sent_as_decoder_decode_start() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();

    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    session.pipeline.require_video_decoder_keyframe();
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_millis(120),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"unknown-keyframe"),
            PacketKeyframe::Unknown,
        ));

    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );
    let decoder_packets = decoder_thread.sent_packets();

    assert_eq!(sent_packets, 1);
    assert_eq!(decoder_packets.len(), 1);
    assert!(decoder_packets[0].keyframe);
    assert!(!session.pipeline.video_decoder_needs_keyframe());
    let diagnostics = session
        .snapshot_with_frame_counters(FrameCounters::default())
        .diagnostics
        .seek_bootstrap;
    assert_eq!(diagnostics.dropped_until_keyframe, 0);
    assert_eq!(
        diagnostics.first_accepted_keyframe,
        Some(PacketKeyframe::Unknown)
    );
}

#[test]
fn h264_unknown_keyframe_bootstrap_packet_waits_for_codec_proof() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();

    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::H264),
    );
    session.pipeline.require_video_decoder_keyframe();
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_millis(120),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"h264-unknown-keyframe"),
            PacketKeyframe::Unknown,
        ));

    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(sent_packets, 0);
    assert!(decoder_thread.sent_packets().is_empty());
    assert!(session.pipeline.pending_video_packet_is_empty());
    assert!(session.pipeline.video_decoder_needs_keyframe());
    assert_eq!(
        tick_result.dropped_video_frames,
        vec![PlayerVideoFrameDrop {
            pts: Duration::from_millis(120),
            reason: PlayerVideoDropReason::SeekPreroll,
        }]
    );

    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_millis(160),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"h264-idr"),
            PacketKeyframe::Keyframe,
        ));
    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );
    let decoder_packets = decoder_thread.sent_packets();

    assert_eq!(sent_packets, 1);
    assert_eq!(decoder_packets.len(), 1);
    assert!(decoder_packets[0].keyframe);
    assert!(!session.pipeline.video_decoder_needs_keyframe());
}

#[test]
fn h265_unknown_keyframe_bootstrap_packet_waits_for_codec_proof() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();

    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::H265),
    );
    session.pipeline.require_video_decoder_keyframe();
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_millis(120),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"h265-unknown-keyframe"),
            PacketKeyframe::Unknown,
        ));

    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(sent_packets, 0);
    assert!(decoder_thread.sent_packets().is_empty());
    assert!(session.pipeline.pending_video_packet_is_empty());
    assert!(session.pipeline.video_decoder_needs_keyframe());

    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_millis(160),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"h265-irap"),
            PacketKeyframe::Keyframe,
        ));
    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );
    let decoder_packets = decoder_thread.sent_packets();

    assert_eq!(sent_packets, 1);
    assert_eq!(decoder_packets.len(), 1);
    assert!(decoder_packets[0].keyframe);
    assert!(!session.pipeline.video_decoder_needs_keyframe());
}

#[test]
fn packet_refinement_rejection_stops_before_decoder_send() {
    let mut session = PlayerSession::new();
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut tick_result = PlayerTickResult::default();

    session.set_system_capabilities(capabilities_with_vp9_profile0_for_decoder_io());
    session
        .pipeline
        .apply_demux_track_list_update(vec![video_track_for_tests(1)]);
    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(120),
        vp9_profile2_10bit_keyframe_for_tests(),
        true,
    );

    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );

    assert_eq!(sent_packets, 0);
    assert!(session.pipeline.pending_video_packet_is_empty());
    assert!(decoder_thread.sent_packets().is_empty());
    let last_error = session
        .snapshot()
        .last_error
        .as_ref()
        .expect("rejected bitstream refinement должен стать fatal before send");
    assert_eq!(last_error.kind, PlayerErrorKind::UnsupportedVideoProfile);
    assert!(last_error.message.contains("profile VP9 Profile 2"));
}

#[test]
fn decoder_bootstrap_drops_interframes_until_keyframe() {
    let mut session = PlayerSession::new();
    session.pipeline.require_video_decoder_keyframe();

    assert!(!accept_video_packet_for_decoder_bootstrap(
        &mut session,
        PacketKeyframe::NotKeyframe,
        Duration::from_millis(10)
    ));
    assert!(session.pipeline.video_decoder_needs_keyframe());
    assert_eq!(
        session
            .snapshot_with_frame_counters(FrameCounters::default())
            .diagnostics
            .seek_bootstrap
            .dropped_until_keyframe,
        1
    );

    assert!(accept_video_packet_for_decoder_bootstrap(
        &mut session,
        PacketKeyframe::Keyframe,
        Duration::from_millis(20)
    ));
    assert!(!session.pipeline.video_decoder_needs_keyframe());
    let diagnostics = session
        .snapshot_with_frame_counters(FrameCounters::default())
        .diagnostics
        .seek_bootstrap;
    assert_eq!(diagnostics.dropped_until_keyframe, 1);
    assert_eq!(
        diagnostics.first_accepted_keyframe,
        Some(PacketKeyframe::Keyframe)
    );

    assert!(accept_video_packet_for_decoder_bootstrap(
        &mut session,
        PacketKeyframe::NotKeyframe,
        Duration::from_millis(30)
    ));
}

#[test]
fn decoder_bootstrap_accepts_unknown_keyframe_as_decode_start() {
    let mut session = PlayerSession::new();
    session.pipeline.require_video_decoder_keyframe();

    assert!(accept_video_packet_for_decoder_bootstrap(
        &mut session,
        PacketKeyframe::Unknown,
        Duration::from_millis(10)
    ));
    assert!(!session.pipeline.video_decoder_needs_keyframe());
}
