use std::path::Path;
use std::time::{Duration, Instant};

use capability_core::{SystemCapabilities, UnsupportedVideoRequirement, VideoCapabilityRejection};
use codec_core::{
    VideoCodec, VideoDecodeRequirement, VideoMetadataSource, resolve_video_metadata,
    unsupported_requirement_can_be_refined_by_packet_probe as codec_requirement_can_be_refined_by_packet_probe,
    video_requirement_needs_packet_refinement,
};
use media_core::{
    DemuxSeekability, DemuxTrackListUpdate, MediaDemuxError, MediaDuration, MediaTime,
    TimelineNotSeekableReason, TimelinePreviewState, TrackInfo, TrackKind,
};
use tracing::{debug, info, trace, warn};

use crate::media_opening::PreparedMedia;
use crate::pipeline::AudioSeekRuntimeState;
use crate::seek_controller::PlaybackResumeIntent;
use crate::seek_state::{
    SeekCommitKind, SeekCommitState, SeekDemuxRequestError, demux_seek_request_for_transaction,
};
use crate::{
    ActiveSeekDiagnosticsSnapshot, FrameCounters, MediaOpenRequest, MediaSource, MediaSummary,
    PipelineLatencyStage, PipelinePauseReason, PipelineQueueDepthSnapshot, PlaybackDiagnostics,
    PlaybackDiagnosticsLogSummary, PlaybackPipeline, PlaybackState, PlayerCommand, PlayerError,
    PlayerErrorKind, PlayerEvent, PlayerResult, PlayerSnapshot, PlayerTickConfig, QualitySelection,
    ScrubCommitIntent, ScrubCommitPolicy, ScrubGeneration, ScrubUpdateIntent, SeekMode,
    SeekProgressBlocker, SeekRequest, SessionScrubCommand, TextureSlotPressureSnapshot, TrackId,
    TrackSelectionSnapshot, VideoBackendFactory, VideoDropReason, WorkerWakeupDiagnosticsSnapshot,
};

use self::scrub_state::{
    SessionScrubFinishDecision, SessionScrubState, SessionScrubUpdateAcceptance,
};

mod render_leases;
mod scrub_state;
mod snapshot_builder;

pub(crate) use self::render_leases::LeasedPresentFrame;

/// Чистый план выбора audio track-а без decoder/output side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AudioDecoderInitSpec {
    /// Track ID выбранного audio трека.
    track_id: TrackId,

    /// Container codec id, который audio crate мапит на Symphonia codec id.
    codec_id: String,

    /// Codec private / extra data из container metadata.
    codec_private: Option<Vec<u8>>,

    /// Sample rate, который demuxer уже сообщил до первого decoded packet-а.
    initial_sample_rate: Option<u32>,

    /// Количество каналов, которое demuxer уже сообщил до первого decoded packet-а.
    initial_channels: Option<u32>,
}

/// Минимальный защитный preroll, ниже которого seek resume не должен считать audio готовым.
const MIN_SEEK_AUDIO_PREROLL_MS: f64 = 1.0;

/// Typed результат проверки audio gate-а для seek commit-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeekAudioGateStatus {
    /// Audio gate готов и не блокирует commit.
    Ready,

    /// Output ещё не подтвердил очистку buffer-а текущего seek generation.
    WaitingForClear,

    /// Для выбранного audio track-а ещё нет установленного decoder-а.
    WaitingForDecoder,

    /// Decoder установлен, но output ещё не создан по decoded AudioSpec.
    WaitingForOutput,

    /// Output готов, но buffer ещё не содержит минимальный post-seek preroll.
    WaitingForPreroll,
}

impl SeekAudioGateStatus {
    /// Возвращает `true`, только если status не блокирует seek commit.
    const fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Мапит audio-gate status в diagnostics blocker без потери причины.
    const fn blocker(self) -> Option<SeekProgressBlocker> {
        match self {
            Self::Ready => None,
            Self::WaitingForClear => Some(SeekProgressBlocker::WaitingForAudioClear),
            Self::WaitingForDecoder => Some(SeekProgressBlocker::WaitingForAudioDecoder),
            Self::WaitingForOutput => Some(SeekProgressBlocker::WaitingForAudioOutput),
            Self::WaitingForPreroll => Some(SeekProgressBlocker::WaitingForAudioPreroll),
        }
    }
}

/// Read-only срез gate-состояния, нужный только для диагностики active seek-а.
#[derive(Debug, Clone, Copy)]
struct SeekProgressGateSnapshot {
    /// Target frame уже стал текущим non-stale present frame.
    target_frame_presented: bool,

    /// Video gate больше не блокирует commit.
    video_gate_ready: bool,

    /// Typed status audio gate-а с причиной возможного ожидания.
    audio_gate_status: SeekAudioGateStatus,

    /// Сколько video frames уже готовы для post-seek resume.
    ready_video_frames: usize,

    /// Сколько video frames требуется текущей resume policy.
    required_video_frames: usize,
}

/// Центральная session плеера: high-level state machine и владение playback pipeline.
pub struct PlayerSession {
    /// Последний базовый read-only snapshot без runtime diagnostics, зависящих от shell.
    snapshot: PlayerSnapshot,

    /// Media pipeline, перенесённый из `AppState` в Phase 3.
    pub(crate) pipeline: PlaybackPipeline,

    /// Codec/render-neutral diagnostics aggregator для текущего media pipeline.
    pub(crate) diagnostics: PlaybackDiagnostics,

    /// События, накопленные после последнего drain.
    pending_events: Vec<PlayerEvent>,

    /// Autoplay-флаг последнего open request до фактического media-open.
    pending_autoplay: bool,

    /// Был ли принят shutdown-запрос.
    shutdown_requested: bool,

    /// Флаг дорендера хвоста после EOF.
    pub draining_after_eof: bool,

    /// Последний системный capability report, полученный от shell/backend layer.
    capabilities: Option<SystemCapabilities>,

    /// Активная операция точного seek commit-а, если player ждёт pre-roll/gates.
    seek_commit: Option<SeekCommitState>,

    /// Scrub-only session state, сгруппированный по active generation.
    scrub_state: SessionScrubState,

    /// Локальный generation seed для прямых unit-test вызовов `dispatch_command`.
    direct_scrub_generation_seed: ScrubGeneration,

    /// Final seek near EOF завершился свежим frame-ом до requested target.
    seek_eof_fallback_video_position: Option<MediaTime>,
}

impl PlayerSession {
    /// Создаёт пустую player session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Возвращает последний базовый immutable snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &PlayerSnapshot {
        &self.snapshot
    }

    /// Собирает актуальный snapshot для UI, renderer и desktop integration.
    #[must_use]
    pub fn snapshot_with_frame_counters(&self, frame_counters: FrameCounters) -> PlayerSnapshot {
        snapshot_builder::build_snapshot(self, frame_counters)
    }

    /// Сообщает, что session уже получила shutdown-запрос.
    #[must_use]
    pub const fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested
    }

    /// Устанавливает capability report и публикует событие для UI/log layer.
    pub fn set_system_capabilities(&mut self, capabilities: SystemCapabilities) {
        let summary = capabilities.detailed_report_text();
        self.snapshot.capability_summary = Some(summary.clone());
        self.pending_events
            .push(PlayerEvent::CapabilityScanCompleted(
                crate::CapabilitySummary { summary },
            ));
        self.capabilities = Some(capabilities);
    }

    /// Возвращает effective playback state с учётом EOF-drain режима.
    #[must_use]
    pub const fn playback_state(&self) -> PlaybackState {
        if self.draining_after_eof {
            PlaybackState::Draining
        } else {
            self.snapshot.playback_state
        }
    }

    /// Возвращает `true`, если demux loop должен читать новые packets.
    #[must_use]
    pub const fn is_demuxing_active(&self) -> bool {
        matches!(
            self.snapshot.playback_state,
            PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Seeking
        )
    }

    /// Возвращает `true`, если scheduler может менять present frame.
    #[must_use]
    pub const fn can_present_video(&self) -> bool {
        matches!(
            self.snapshot.playback_state,
            PlaybackState::Playing | PlaybackState::Buffering | PlaybackState::Seeking
        ) || self.draining_after_eof
    }

    /// Возвращает `true`, если текущая session владеет открытым demuxer-ом.
    #[must_use]
    pub fn has_loaded_media_pipeline(&self) -> bool {
        self.pipeline.has_demuxer()
    }

    /// Возвращает путь текущего локального файла, если media было открыто с диска.
    #[must_use]
    pub fn current_file_path(&self) -> Option<&Path> {
        self.pipeline.source_file_path()
    }

    /// Применяет команду к state machine.
    pub fn dispatch_command(&mut self, command: PlayerCommand) -> PlayerResult<()> {
        match command {
            PlayerCommand::OpenMedia(request) => self.open_media(request),
            PlayerCommand::Play => self.play(),
            PlayerCommand::Pause => self.pause(),
            PlayerCommand::TogglePlayback => self.toggle_playback(),
            PlayerCommand::Seek(request) => self.seek(request),
            PlayerCommand::BeginScrub => {
                let generation = self.next_direct_scrub_generation();
                self.begin_scrub(generation)
            }
            PlayerCommand::UpdateScrub(request) => {
                let generation = self.current_or_next_direct_scrub_generation();
                self.update_scrub(ScrubUpdateIntent::new(generation, request))
            }
            PlayerCommand::PreviewScrub(request) => {
                let generation = self.current_or_next_direct_scrub_generation();
                self.preview_scrub(ScrubUpdateIntent::new(generation, request))
            }
            PlayerCommand::EndScrub { policy } => {
                let generation = self.scrub_state.current_generation().unwrap_or_default();
                self.end_scrub(ScrubCommitIntent::new(generation, policy))
            }
            PlayerCommand::Stop => self.stop(),
            PlayerCommand::SetVolume(volume) => self.set_volume(volume),
            PlayerCommand::SelectVideoTrack(track_id) => self.select_video_track(track_id),
            PlayerCommand::SelectAudioTrack(track_id) => self.select_audio_track(track_id),
            PlayerCommand::SelectSubtitleTrack(track_id) => self.select_subtitle_track(track_id),
            PlayerCommand::SelectQuality(selection) => self.select_quality(selection),
            PlayerCommand::ReloadConfig => self.reload_config(),
            PlayerCommand::Shutdown => self.shutdown(),
        }
    }

    /// Применяет scrub-команду с generation, выданным worker boundary.
    pub(crate) fn dispatch_scrub_command(
        &mut self,
        command: SessionScrubCommand,
    ) -> PlayerResult<()> {
        match command {
            SessionScrubCommand::Begin { generation } => self.begin_scrub(generation),
            SessionScrubCommand::Update(intent) => self.update_scrub(intent),
            SessionScrubCommand::Preview(intent) => self.preview_scrub(intent),
            SessionScrubCommand::End(intent) => self.end_scrub(intent),
        }
    }

    /// Выдаёт generation для прямого `PlayerSession::dispatch_command` в unit tests.
    fn next_direct_scrub_generation(&mut self) -> ScrubGeneration {
        self.direct_scrub_generation_seed = self.direct_scrub_generation_seed.next();
        self.direct_scrub_generation_seed
    }

    /// Возвращает текущий direct scrub generation или начинает legacy direct scrub.
    fn current_or_next_direct_scrub_generation(&mut self) -> ScrubGeneration {
        match self.scrub_state.current_generation() {
            Some(generation) => generation,
            None => {
                let generation = self.next_direct_scrub_generation();
                self.scrub_state.begin(generation);
                generation
            }
        }
    }

    /// Переключает playback между `Playing` и `Paused`.
    pub fn toggle_playback(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        match self.snapshot.playback_state {
            PlaybackState::Playing => self.pause(),
            _ => self.play(),
        }
    }

    /// Обновляет позицию playback из clock/UI без накопления high-frequency событий.
    pub fn update_current_position(&mut self, position: Duration) {
        if self.snapshot.current_position == position {
            return;
        }

        self.snapshot
            .set_timeline_position(MediaTime::from_duration(position));

        if self.snapshot.playback_state == PlaybackState::Playing
            && !self.pipeline.has_audio_clock()
        {
            self.pipeline
                .start_monotonic_media_clock(position, Instant::now());
        }
    }

    /// Возвращает media clock position на monotonic момент `now`.
    ///
    /// Audio clock остаётся главным источником времени. Если audio clock отсутствует,
    /// Playing-state использует внутренний monotonic anchor, а не частоту worker tick-а.
    #[must_use]
    pub(crate) fn presentation_clock_position_at(&self, now: Instant) -> Duration {
        if self.pipeline.has_audio_clock() {
            return self.audio_media_clock_position();
        }

        if let Some(seek_target_position) = self.seek_presentation_clock_override() {
            return seek_target_position;
        }

        if self.snapshot.playback_state == PlaybackState::Playing
            && let Some(position) = self.pipeline.monotonic_media_position(now)
        {
            return position;
        }

        self.snapshot.current_position
    }

    /// Синхронизирует snapshot position с monotonic fallback clock без изменения playback state.
    fn sync_monotonic_media_clock_position(&mut self, now: Instant) {
        if self.pipeline.has_audio_clock() {
            return;
        }

        let position = self.presentation_clock_position_at(now);
        self.update_current_position(position);
    }

    /// Запускает или перезапускает no-audio media clock от текущей snapshot position.
    fn anchor_monotonic_media_clock_if_needed(&mut self, now: Instant) {
        if self.pipeline.has_audio_clock() {
            self.pipeline.clear_monotonic_media_clock();
            return;
        }

        self.pipeline
            .start_monotonic_media_clock(self.snapshot.current_position, now);
    }

    /// Останавливает no-audio media clock, предварительно сохранив актуальную позицию.
    fn clear_monotonic_media_clock_anchor(&mut self, now: Instant) {
        self.sync_monotonic_media_clock_position(now);
        self.pipeline.clear_monotonic_media_clock();
    }

    /// Возвращает абсолютную media position по audio clock.
    fn audio_media_clock_position(&self) -> Duration {
        self.pipeline.media_position_from_audio_clock()
    }

    /// Добавляет delta к текущей позиции без panic при переполнении.
    pub fn advance_position(&mut self, delta: Duration) {
        let next_position = self
            .snapshot
            .current_position
            .checked_add(delta)
            .unwrap_or(Duration::MAX);
        self.update_current_position(next_position);
    }

    /// Загружает уже открытый demuxer для streaming source.
    pub fn load_demuxer(&mut self, label: String, demuxer: Box<dyn media_core::Demuxer + Send>) {
        self.load_demuxer_with_autoplay(label, demuxer, false);
    }

    /// Загружает уже открытый demuxer для streaming source с явной autoplay-политикой.
    pub fn load_demuxer_with_autoplay(
        &mut self,
        label: String,
        demuxer: Box<dyn media_core::Demuxer + Send>,
        autoplay: bool,
    ) {
        let prepared_media = PreparedMedia::from_external_label(label, demuxer);
        self.load_prepared_media_with_autoplay(prepared_media, autoplay);
    }

    /// Устанавливает media, уже открытый shell/container adapter слоем.
    pub fn load_prepared_media(&mut self, prepared_media: PreparedMedia) {
        self.load_prepared_media_with_autoplay(prepared_media, false);
    }

    /// Устанавливает prepared media с явной autoplay-политикой.
    pub fn load_prepared_media_with_autoplay(
        &mut self,
        prepared_media: PreparedMedia,
        autoplay: bool,
    ) {
        if !self.begin_media_open(prepared_media.media_source(), autoplay) {
            return;
        }

        self.install_prepared_media(prepared_media);
    }

    /// Публикует failed open transition, когда adapter не смог подготовить demuxer.
    pub fn fail_media_open_with_error(&mut self, request: MediaOpenRequest, error: PlayerError) {
        if !self.begin_media_open(request.source, request.autoplay) {
            return;
        }

        self.mark_fatal_error(error);
    }

    /// Переводит state machine в `Opening` перед blocking/opened source phase.
    fn begin_media_open(&mut self, media_source: MediaSource, autoplay: bool) -> bool {
        self.reset_media_state();

        let open_request = MediaOpenRequest::new(media_source, autoplay);
        if let Err(error) = self.dispatch_command(PlayerCommand::OpenMedia(open_request)) {
            self.record_recoverable_error(error);
            return false;
        }

        true
    }

    /// Подключает уже открытый media к pipeline и публикует успешный open transition.
    fn install_prepared_media(&mut self, prepared_media: PreparedMedia) {
        info!(
            source = %prepared_media.source_label(),
            tracks = prepared_media.track_count(),
            duration = ?prepared_media.duration(),
            "Media demuxer загружен"
        );

        self.init_audio_pipeline(prepared_media.tracks());
        if let Err(error) = self.select_default_video_track(
            prepared_media.tracks(),
            prepared_media.missing_video_track_message(),
        ) {
            warn!(error = %error, "Video track rejected during media load");
            self.mark_fatal_error(error);
            return;
        }

        let summary = MediaSummary {
            title: prepared_media.media_title(),
            source_label: prepared_media.source_label(),
            duration: prepared_media.duration(),
        };
        let seekability = prepared_media.seekability();
        let (demuxer, file_path, source_label, tracks) = prepared_media.into_pipeline_slots();

        self.pipeline
            .install_opened_media(demuxer, file_path, source_label, tracks);
        self.clear_error();

        if let Err(error) = self.mark_media_opened(summary) {
            self.record_recoverable_error(error);
        }
        self.apply_demux_seekability(seekability);
    }

    /// Применяет lifecycle update от demuxer-а после container discontinuity.
    pub(crate) fn handle_demux_track_list_update(&mut self, track_update: DemuxTrackListUpdate) {
        let tracks = track_update.tracks;

        info!(
            tracks = tracks.len(),
            duration = ?track_update.duration,
            "Demuxer сообщил обновление track list"
        );

        self.pause_audio_output_for_seek();
        if let Err(error) = self.reset_video_decoder_for_seek() {
            self.mark_fatal_error(error);
            return;
        }

        let generation = self.pipeline.begin_seek_generation();
        self.clear_seek_preroll_fallback_frame();
        self.clear_queued_video_frames();
        self.pipeline.apply_demux_track_list_update(tracks.clone());
        self.pipeline.mark_audio_buffer_clear_ack(generation);
        self.set_snapshot_duration(track_update.duration);
        self.snapshot.selected_tracks = TrackSelectionSnapshot::default();
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame();

        self.init_audio_pipeline(&tracks);
        if let Err(error) =
            self.select_default_video_track(&tracks, "Video track не найден после demux reset")
        {
            warn!(error = %error, "Video track rejected after demux track-list update");
            self.mark_fatal_error(error);
        }
    }

    /// Отмечает успешное открытие media внешним demux/source слоем.
    pub fn mark_media_opened(&mut self, summary: MediaSummary) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.snapshot.media_title = summary.title.clone();
        self.set_snapshot_duration(summary.duration);
        self.snapshot.source_label = Some(summary.source_label.clone());
        self.clear_error();
        self.pending_events.push(PlayerEvent::MediaOpened(summary));

        if self.pending_autoplay {
            self.begin_autoplay_preroll()?;
        } else {
            self.pause()?;
        }

        Ok(())
    }

    /// Отмечает fatal error от media pipeline.
    pub fn mark_fatal_error(&mut self, error: PlayerError) {
        self.snapshot.last_error = Some(error.clone());
        self.set_playback_state(PlaybackState::Failed);
        self.pending_events.push(PlayerEvent::FatalError(error));
    }

    /// Переводит session в EOF-drain, сохраняя demuxer открытым для replay/seek.
    pub fn enter_eof_drain(&mut self) {
        self.set_playback_state(PlaybackState::Draining);
    }

    /// Забирает накопленные события и очищает внутреннюю очередь.
    #[must_use]
    pub fn take_events(&mut self) -> Vec<PlayerEvent> {
        std::mem::take(&mut self.pending_events)
    }

    /// Полностью сбрасывает состояние текущего media.
    pub fn reset_media_state(&mut self) {
        self.set_playback_state(PlaybackState::Paused);
        self.pipeline.clear_monotonic_media_clock();
        self.clear_video_frames();
        self.advance_render_generation();

        if let Err(error) = self.pipeline.flush_video_decoder_thread() {
            warn!(error = %error, "Не удалось сбросить video decoder thread");
        }

        self.pipeline.reset_media_slots();
        self.diagnostics.reset();

        self.pending_autoplay = false;
        self.seek_commit = None;
        self.scrub_state.clear();
        self.seek_eof_fallback_video_position = None;
        self.snapshot.source_label = None;
        self.snapshot.media_title = None;
        self.snapshot.clear_timeline();
        self.snapshot.selected_tracks = TrackSelectionSnapshot::default();
        self.snapshot.tracks.clear();
        self.clear_error();
    }

    /// Переводит renderer resource ids в новое поколение после полного reset media pipeline.
    fn advance_render_generation(&mut self) {
        self.pipeline.advance_render_generation();
    }

    /// Обновляет оценку длительности video frame по очередному decoded PTS.
    pub fn observe_video_frame_pts(&mut self, pts: Duration) {
        self.pipeline.observe_decoded_video_frame_pts(pts);
    }

    /// Очищает video frame queue и present frame, освобождая texture slots.
    pub fn clear_video_frames(&mut self) {
        self.clear_seek_preroll_fallback_frame();
        let queued_texture_handles = self.pipeline.clear_video_queues();
        let present_texture_handle = self
            .pipeline
            .take_present_video_frame()
            .map(|frame| frame.texture_handle);

        for texture_handle in queued_texture_handles {
            self.release_video_texture(texture_handle);
        }
        if let Some(texture_handle) = present_texture_handle {
            self.release_video_texture(texture_handle);
        }
    }

    /// Очищает только очередь будущих video frames, сохраняя текущий кадр на экране.
    pub fn clear_queued_video_frames(&mut self) {
        self.clear_seek_preroll_fallback_frame();
        let queued_texture_handles = self.pipeline.clear_video_queues();

        for texture_handle in queued_texture_handles {
            self.release_video_texture(texture_handle);
        }
    }

    /// Записывает latency sample с актуальными queue depths.
    pub(crate) fn record_pipeline_latency(
        &mut self,
        stage: PipelineLatencyStage,
        duration: Duration,
        pts: Option<Duration>,
        memory_path: Option<video_core::FrameMemoryPath>,
    ) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics
            .record_latency(stage, duration, pts, memory_path, queues);
    }

    /// Записывает decoded frame diagnostics из backend-neutral frame contract.
    pub(crate) fn record_decoded_frame_diagnostics(&mut self, frame: &video_core::DecodedFrame) {
        let mut queues = self.diagnostic_queue_depths();
        queues.decoder_ready_queue_depth = frame.diagnostics.decoder_ready_queue_depth;
        queues.texture_slots =
            frame
                .diagnostics
                .texture_pool
                .map(|texture_pool| TextureSlotPressureSnapshot {
                    capacity: texture_pool.capacity,
                    slots: texture_pool.slots,
                    in_use: texture_pool.in_use,
                    free_surfaces: texture_pool.free_surfaces,
                    waiting_gpu_completion: texture_pool.waiting_gpu_completion,
                    waiting_decoder_reuse: texture_pool.waiting_decoder_reuse,
                    import_failures: texture_pool.import_failures,
                    imports_created: texture_pool.imports_created,
                    imports_reused: texture_pool.imports_reused,
                    imports_replaced: texture_pool.imports_replaced,
                });
        self.diagnostics.observe_decoded_frame(frame, queues);
    }

    /// Записывает pressure counters decoder-thread publish boundary.
    pub(crate) fn record_decoded_frame_publish_pressure(
        &mut self,
        pressure: video_core::VideoFramePublishPressureDiagnostics,
    ) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics
            .record_decoded_frame_publish_pressure(pressure, queues);
    }

    /// Записывает typed video drop reason с текущими queue depths.
    pub(crate) fn record_video_drop(&mut self, pts: Option<Duration>, reason: VideoDropReason) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics.record_drop(pts, reason, queues);
    }

    /// Записывает typed pipeline pause с текущими queue depths.
    pub(crate) fn record_pipeline_pause(&mut self, reason: PipelinePauseReason) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics.record_pause(reason, queues);
    }

    /// Записывает повтор текущего present frame отдельно от drop counters.
    pub(crate) fn record_repeated_video_frame(&mut self) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics.record_repeated_video_frame(queues);
    }

    /// Записывает последнее решение worker wakeup planner-а.
    pub(crate) fn record_worker_wakeup(&mut self, wakeup: WorkerWakeupDiagnosticsSnapshot) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics.record_worker_wakeup(wakeup, queues);
    }

    /// Записывает результат render acquire.
    pub(crate) fn record_render_acquire_wait(&mut self, wait: Duration) {
        self.record_pipeline_latency(PipelineLatencyStage::RenderAcquire, wait, None, None);
    }

    /// Записывает ожидание texture pool lock-а внутри render-side `texture_views()`.
    pub(crate) fn record_texture_view_lock_wait(
        &mut self,
        wait: Duration,
        pts: Option<Duration>,
        memory_path: Option<video_core::FrameMemoryPath>,
    ) {
        self.record_pipeline_latency(
            PipelineLatencyStage::TextureViewLockWait,
            wait,
            pts,
            memory_path,
        );
    }

    /// Записывает busy outcome non-blocking texture view lookup-а.
    pub(crate) fn record_texture_view_lock_busy(&mut self) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics.record_texture_view_lock_busy(queues);
    }

    /// Записывает reuse предыдущего renderable frame-а из-за busy texture view lock-а.
    pub(crate) fn record_texture_view_previous_frame_reuse(&mut self) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics
            .record_texture_view_previous_frame_reuse(queues);
    }

    /// Записывает render acquire timeout как drop и pause attribution.
    #[allow(dead_code)]
    pub(crate) fn record_render_acquire_timeout(&mut self, wait: Duration) {
        self.record_pipeline_latency(PipelineLatencyStage::RenderAcquire, wait, None, None);
        if !self.pipeline.has_selected_video_track() {
            return;
        }
        self.record_video_drop(None, VideoDropReason::RenderAcquisitionTimeout);
        self.record_pipeline_pause(PipelinePauseReason::RenderAcquireTimeout);
    }

    /// Записывает latency release ack от render side до worker.
    pub(crate) fn record_release_ack_latency(&mut self, latency: Duration) {
        self.record_pipeline_latency(
            PipelineLatencyStage::ReleaseAcknowledgement,
            latency,
            None,
            None,
        );
    }

    /// Записывает renderer-side submit/present latency без доступа render loop-а к session.
    pub(crate) fn record_gpu_submit_present_latency(&mut self, latency: Duration) {
        self.record_pipeline_latency(PipelineLatencyStage::GpuSubmitPresent, latency, None, None);
    }

    /// Возвращает компактную diagnostics summary для throttled debug logs.
    #[must_use]
    pub(crate) fn diagnostics_log_summary(&self) -> PlaybackDiagnosticsLogSummary {
        self.diagnostics.log_summary(self.diagnostic_queue_depths())
    }

    /// Обрабатывает audio packet: decode -> write to AudioOutput.
    pub fn process_audio_packet(
        &mut self,
        track_id: TrackId,
        packet_pts: Duration,
        packet_dts: Option<Duration>,
        packet_duration: Option<Duration>,
        generation: u64,
        encoded_audio_bytes: &[u8],
    ) {
        self.process_audio_packet_with_timing(
            track_id,
            packet_pts,
            packet_dts,
            packet_duration,
            audio::AudioPacketTiming::unknown(),
            generation,
            encoded_audio_bytes,
        );
    }

    /// Обрабатывает audio packet с raw container timing для decoder boundary.
    pub(crate) fn process_audio_packet_with_timing(
        &mut self,
        track_id: TrackId,
        packet_pts: Duration,
        _packet_dts: Option<Duration>,
        _packet_duration: Option<Duration>,
        packet_timing: audio::AudioPacketTiming,
        generation: u64,
        encoded_audio_bytes: &[u8],
    ) {
        if self.pipeline.selected_audio_track_id() != Some(track_id) {
            return;
        }

        if !self.pipeline.packet_generation_is_current(generation) {
            return;
        }

        match self.ensure_audio_decoder_for_packet(track_id) {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                self.record_recoverable_error(error);
                return;
            }
        }

        let encoded_audio_packet =
            audio::EncodedAudioPacket::new(track_id.get(), packet_timing, encoded_audio_bytes);

        if let Some(decode_result) = self.pipeline.decode_audio_packet(&encoded_audio_packet) {
            match decode_result {
                Ok(decoded_audio) if !decoded_audio.samples.is_empty() => {
                    if let Err(error) = self.ensure_audio_output_for_decoded_spec(
                        decoded_audio.sample_rate,
                        decoded_audio.channels,
                    ) {
                        warn!(error = %error, "Не удалось подготовить audio output");
                        self.disable_selected_audio_path();
                        self.record_recoverable_error(error);
                        return;
                    }

                    let samples = trim_decoded_audio_to_clock_base(
                        &decoded_audio.samples,
                        packet_pts,
                        self.pipeline.media_clock_base(),
                        decoded_audio.sample_rate,
                        decoded_audio.channels,
                    );
                    if samples.is_empty() {
                        return;
                    }

                    let _written_samples = self.pipeline.write_audio_output_samples(samples);
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(error = %error, "Ошибка декодирования audio packet");
                    self.set_runtime_error(format!("Audio decode error: {error}"));
                }
            }
        }
    }

    /// Создаёт deferred audio decoder при первом packet-е выбранного track-а.
    fn ensure_audio_decoder_for_packet(&mut self, track_id: TrackId) -> PlayerResult<bool> {
        if self.pipeline.has_audio_decoder() {
            return Ok(true);
        }

        let Some(decoder_config) = self.pipeline.take_deferred_audio_decoder_config(track_id)
        else {
            return Ok(false);
        };

        let codec_id = decoder_config.codec_id().to_string();
        match audio::create_audio_decoder(decoder_config) {
            Ok(decoder) => {
                self.pipeline.install_audio_decoder(decoder);
                info!(
                    track_id = %track_id,
                    codec_id = %codec_id,
                    "Deferred audio decoder создан по первому packet-у"
                );
                Ok(true)
            }
            Err(error) => {
                let player_error = player_error_from_audio_decoder_factory_error(error);
                warn!(
                    error = %player_error,
                    codec_id = %codec_id,
                    "Не удалось создать deferred audio decoder"
                );
                self.disable_selected_audio_path();
                Err(player_error)
            }
        }
    }

    /// Создаёт CPAL output только после того, как decoder сообщил реальный decoded spec.
    fn ensure_audio_output_for_decoded_spec(
        &mut self,
        sample_rate: u32,
        channels: u32,
    ) -> PlayerResult<()> {
        if self.pipeline.has_audio_output() {
            return Ok(());
        }

        if sample_rate == 0 || channels == 0 {
            return Err(PlayerError::new(
                PlayerErrorKind::RuntimeError,
                format!(
                    "Audio decoder produced samples without complete AudioSpec: sample_rate={sample_rate}, channels={channels}"
                ),
            ));
        }

        let mut output = audio::AudioOutput::new(sample_rate, channels).map_err(|error| {
            PlayerError::new(
                PlayerErrorKind::AudioDeviceUnavailable,
                format!("Audio output init failed: {error}"),
            )
        })?;

        output.set_volume(self.snapshot.volume);
        self.pipeline.install_audio_output(output);

        if let Some(clock) = self.pipeline.audio_output_clock().cloned() {
            self.pipeline.install_audio_clock(clock);
        }

        if self.playback_state() == PlaybackState::Playing {
            if let Some(Err(error)) = self.pipeline.play_audio_output() {
                return Err(PlayerError::new(
                    PlayerErrorKind::AudioDeviceUnavailable,
                    format!("Audio play after lazy output init failed: {error}"),
                ));
            }

            let observed_at = Instant::now();
            let audio_now = self.audio_clock_now();
            self.pipeline
                .reset_audio_clock_sample(audio_now, observed_at);
        }

        info!(
            sample_rate,
            channels, "Audio output создан после первого decoded AudioSpec"
        );

        Ok(())
    }

    /// Отключает audio path после unrecoverable lazy-init ошибки, не трогая video state.
    fn disable_selected_audio_path(&mut self) {
        self.pipeline.clear_audio_decoder();
        self.pipeline.clear_audio_output();
        self.pipeline.clear_audio_clock();
        self.pipeline.clear_selected_audio_track();
        self.pipeline.clear_pending_audio_packets();
        self.snapshot.selected_tracks.audio_track = None;
    }

    /// Process pending audio packets с throttle по buffer level.
    pub fn process_pending_audio_packets(&mut self) {
        self.process_pending_audio_packets_with_buffer_limit(
            crate::PlayerTickConfig::default().audio_buffer_high_water_mark_ms,
        );
    }

    /// Возвращает audio clock time для отображения в UI.
    #[must_use]
    pub fn audio_clock_secs(&self) -> Option<f64> {
        self.pipeline
            .audio_output_clock()
            .map(|clock| clock.now_secs())
    }

    /// Возвращает уровень audio buffer в миллисекундах.
    #[must_use]
    pub fn audio_buffer_level_ms(&self) -> Option<f64> {
        self.pipeline.audio_output_buffer_level_ms()
    }

    /// Возвращает текущее время audio clock.
    #[must_use]
    pub fn audio_clock_now(&self) -> Duration {
        self.pipeline.audio_clock_now()
    }

    /// Проверяет, есть ли активный seek commit для scheduler/gate логики.
    #[must_use]
    pub(crate) const fn has_active_seek_commit(&self) -> bool {
        self.seek_commit.is_some()
    }

    /// Возвращает активный seek commit для scheduler/gate логики.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn seek_commit(&self) -> Option<SeekCommitState> {
        self.seek_commit
    }

    /// Собирает подробный snapshot активного seek-а для throttled stall logs.
    #[must_use]
    pub(crate) fn active_seek_diagnostics(
        &self,
        now: Instant,
        tick_config: &PlayerTickConfig,
    ) -> Option<ActiveSeekDiagnosticsSnapshot> {
        let seek_commit = self.seek_commit?;
        let target_position = seek_commit.target_position.as_duration();
        let queues = self.diagnostic_queue_depths();
        let required_video_frames = self.required_seek_resume_video_ready_frames(
            seek_commit,
            tick_config.effective_seek_resume_video_min_ready_frames(),
        );
        let ready_video_frames = self.seek_ready_video_frame_count(target_position);
        let target_frame_presented = self.seek_target_frame_presented(target_position);
        let video_gate_ready = self.seek_video_gate_ready(seek_commit, required_video_frames);
        let audio_gate_status =
            self.seek_audio_gate_status(seek_commit, tick_config.seek_resume_audio_min_buffer_ms);
        let audio_gate_ready = audio_gate_status.is_ready();
        let diagnostics_snapshot = self.diagnostics.snapshot_with_queues(queues);
        let gate_snapshot = SeekProgressGateSnapshot {
            target_frame_presented,
            video_gate_ready,
            audio_gate_status,
            ready_video_frames,
            required_video_frames,
        };
        let blocker = self.seek_progress_blocker(tick_config, queues, gate_snapshot);

        Some(ActiveSeekDiagnosticsSnapshot {
            kind: seek_commit_kind_name(seek_commit.kind),
            generation: seek_commit.generation,
            scrub_generation: seek_commit.scrub_generation.map(ScrubGeneration::as_u64),
            age: now.saturating_duration_since(seek_commit.started_at),
            target: target_position,
            actual: seek_commit.actual_position.as_duration(),
            resume_intent: playback_resume_intent_name(seek_commit.resume_intent),
            blocker,
            video_gate_ready,
            audio_gate_ready,
            target_frame_presented,
            ready_video_frames,
            required_video_frames,
            present_frame_pts: self.pipeline.present_video_frame_pts(),
            front_queued_frame_pts: self
                .pipeline
                .front_queued_video_frame()
                .map(|frame| frame.pts),
            demuxing_active: self.is_demuxing_active(),
            draining_after_eof: self.draining_after_eof,
            stale_frame: self.snapshot.timeline.stale_frame,
            last_pause_reason: diagnostics_snapshot.pauses.last.map(|pause| pause.reason),
            queues,
        })
    }

    /// Возвращает seek target как временный clock для scheduler-а, пока audio clock недоступен.
    ///
    /// Без этого video-only seek сравнивает decoded target frames со старой
    /// `current_position` и может бесконечно ждать, считая кадр "слишком ранним".
    #[must_use]
    pub(crate) fn seek_presentation_clock_override(&self) -> Option<Duration> {
        if self.pipeline.has_audio_clock() {
            return None;
        }

        self.seek_commit
            .map(|seek_commit| seek_commit.target_position.as_duration())
    }

    /// Проверяет, нужно ли выбросить decoded frame как pre-roll до seek target.
    ///
    /// Final seek остаётся точным: кадры до пользовательской позиции не попадают
    /// в presentation queue. Preview seek, наоборот, может показывать такие
    /// кадры как live feedback во время scrub, пока decoder догоняет target.
    #[must_use]
    pub(crate) fn should_drop_decoded_frame_for_seek(&self, frame_pts: Duration) -> bool {
        self.seek_commit.is_some_and(|seek_commit| {
            seek_commit.kind == SeekCommitKind::Final
                && frame_pts < seek_commit.target_position.as_duration()
        })
    }

    /// Возвращает target активного final seek-а, если такой transition сейчас идёт.
    #[must_use]
    pub(crate) fn active_final_seek_target(&self) -> Option<Duration> {
        self.seek_commit.and_then(|seek_commit| {
            (seek_commit.kind == SeekCommitKind::Final)
                .then_some(seek_commit.target_position.as_duration())
        })
    }

    /// Проверяет, можно ли сохранить pre-target frame как EOF fallback текущего seek-а.
    #[must_use]
    pub(crate) fn can_keep_seek_preroll_fallback(&self, frame_pts: Duration) -> bool {
        self.active_final_seek_target()
            .is_some_and(|target_position| frame_pts < target_position)
    }

    /// Заменяет EOF fallback frame и возвращает старый frame для явного release/drop учёта.
    pub(crate) fn replace_seek_preroll_fallback_frame(
        &mut self,
        frame: video_core::DecodedFrame,
    ) -> Option<video_core::DecodedFrame> {
        self.pipeline
            .replace_seek_preroll_fallback_video_frame(frame)
    }

    /// Забирает EOF fallback frame, если scheduler решил, что target frame уже не придёт.
    pub(crate) fn take_seek_preroll_fallback_frame(&mut self) -> Option<video_core::DecodedFrame> {
        self.pipeline.take_seek_preroll_fallback_video_frame()
    }

    /// Освобождает EOF fallback frame при новом seek/reset/точном target frame-е.
    pub(crate) fn clear_seek_preroll_fallback_frame(&mut self) {
        if let Some(frame) = self.pipeline.clear_seek_preroll_fallback_video_frame() {
            self.release_video_texture(frame.texture_handle);
        }
    }

    /// Отмечает, что final seek near EOF показал свежий fallback frame текущего transition-а.
    pub(crate) fn note_presented_seek_eof_fallback_frame(&mut self, frame_pts: Duration) {
        if self.active_final_seek_target().is_none() {
            return;
        }

        self.seek_eof_fallback_video_position = Some(MediaTime::from_duration(frame_pts));
        self.snapshot.timeline.stale_frame = false;
    }

    /// Отмечает, что target video frame уже стал текущим кадром presentation.
    pub(crate) fn note_presented_frame_for_seek(&mut self, frame_pts: Duration) {
        let Some(seek_commit) = self.seek_commit else {
            return;
        };

        trace!(
            kind = ?seek_commit.kind,
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            frame_pts_ms = frame_pts.as_millis(),
            generation = seek_commit.generation,
            scrub_generation = ?seek_commit.scrub_generation.map(ScrubGeneration::as_u64),
            "Seek transaction увидел presented frame"
        );

        match seek_commit.kind {
            SeekCommitKind::Preview => {
                if let Some(generation) = seek_commit.scrub_generation {
                    let _visible_preview_decision = self
                        .scrub_state
                        .note_visible_preview(generation, MediaTime::from_duration(frame_pts));
                }
                self.snapshot.timeline.preview_state =
                    if frame_pts >= seek_commit.target_position.as_duration() {
                        TimelinePreviewState::Ready
                    } else {
                        TimelinePreviewState::Visible
                    };
                self.snapshot.timeline.stale_frame = false;
            }
            SeekCommitKind::Final if frame_pts >= seek_commit.target_position.as_duration() => {
                self.seek_eof_fallback_video_position = None;
                self.snapshot.timeline.stale_frame = false;
            }
            SeekCommitKind::Final => {}
        }
    }

    /// Завершает seek commit, когда video/audio gates готовы, или применяет timeout.
    pub(crate) fn finish_seek_commit_if_ready(
        &mut self,
        now: Instant,
        commit_timeout: Duration,
        preview_timeout: Duration,
        resume_audio_min_buffer_ms: f64,
        resume_video_min_ready_frames: usize,
    ) {
        let Some(seek_commit) = self.seek_commit else {
            return;
        };

        let active_timeout = match seek_commit.kind {
            SeekCommitKind::Final => commit_timeout,
            SeekCommitKind::Preview => preview_timeout,
        };

        if now.saturating_duration_since(seek_commit.started_at) >= active_timeout {
            self.fail_seek_commit_on_timeout(seek_commit);
            return;
        }

        if !self.seek_commit_gates_ready(
            seek_commit,
            resume_audio_min_buffer_ms,
            resume_video_min_ready_frames,
        ) {
            return;
        }

        self.complete_seek_commit(seek_commit);
    }

    /// Переопределяет resume intent у уже запущенного seek transaction-а.
    ///
    /// Worker использует это после `EndScrub`: сама session видит временную
    /// pause-команду, которой scrub заглушил audio, а исходное желание
    /// пользователя продолжить playback хранится выше, в `SeekController`.
    pub(crate) fn override_active_seek_resume_intent(
        &mut self,
        resume_intent: PlaybackResumeIntent,
    ) -> bool {
        let Some(seek_commit) = self.seek_commit.as_mut() else {
            return false;
        };

        seek_commit.resume_intent = resume_intent;
        self.set_playback_state(PlaybackState::Seeking);

        if resume_intent == PlaybackResumeIntent::Pause {
            self.pause_audio_output_for_seek();
        }

        true
    }

    /// Проверяет video/audio gates для текущего seek commit-а.
    fn seek_commit_gates_ready(
        &self,
        seek_commit: SeekCommitState,
        resume_audio_min_buffer_ms: f64,
        resume_video_min_ready_frames: usize,
    ) -> bool {
        self.seek_video_gate_ready(seek_commit, resume_video_min_ready_frames)
            && self.seek_audio_gate_ready(seek_commit, resume_audio_min_buffer_ms)
    }

    /// Video gate готов, когда target frame показан и перед resume есть небольшой запас кадров.
    fn seek_video_gate_ready(
        &self,
        seek_commit: SeekCommitState,
        resume_video_min_ready_frames: usize,
    ) -> bool {
        if !self.pipeline.has_selected_video_track() {
            return true;
        }

        let target_position = seek_commit.target_position.as_duration();
        if self.seek_preview_visible_frame_ready(seek_commit) {
            return true;
        }

        let target_frame_presented = self.pipeline.present_video_frame_covers(target_position)
            && !self.snapshot.timeline.stale_frame;
        let eof_fallback_presented =
            self.seek_eof_fallback_video_ready(seek_commit, target_position);

        if eof_fallback_presented {
            return true;
        }

        if !target_frame_presented {
            return false;
        }

        let required_ready_frames = self
            .required_seek_resume_video_ready_frames(seek_commit, resume_video_min_ready_frames);

        self.seek_ready_video_frame_count(target_position) >= required_ready_frames
    }

    /// Проверяет, что target frame уже стал текущим non-stale present frame.
    fn seek_target_frame_presented(&self, target_position: Duration) -> bool {
        self.pipeline.present_video_frame_covers(target_position)
            && !self.snapshot.timeline.stale_frame
    }

    /// Preview gate готов после любого свежего кадра текущего scrub generation-а.
    fn seek_preview_visible_frame_ready(&self, seek_commit: SeekCommitState) -> bool {
        if seek_commit.kind != SeekCommitKind::Preview || self.snapshot.timeline.stale_frame {
            return false;
        }

        let Some(scrub_generation) = seek_commit.scrub_generation else {
            return false;
        };
        let Some(visible_preview) = self.scrub_state.visible_preview_for(scrub_generation) else {
            return false;
        };

        self.present_frame_matches_position(visible_preview.position.as_duration())
    }

    /// Классифицирует текущую причину, по которой active seek ещё не закрыл gates.
    fn seek_progress_blocker(
        &self,
        tick_config: &PlayerTickConfig,
        queues: PipelineQueueDepthSnapshot,
        gate_snapshot: SeekProgressGateSnapshot,
    ) -> SeekProgressBlocker {
        let audio_gate_ready = gate_snapshot.audio_gate_status.is_ready();
        if gate_snapshot.video_gate_ready && audio_gate_ready {
            return SeekProgressBlocker::ReadyToCommit;
        }

        if let Some(audio_blocker @ SeekProgressBlocker::WaitingForAudioClear) =
            gate_snapshot.audio_gate_status.blocker()
        {
            return audio_blocker;
        }

        if !self.pipeline.has_selected_video_track() {
            return gate_snapshot
                .audio_gate_status
                .blocker()
                .unwrap_or(SeekProgressBlocker::WaitingForAudioPreroll);
        }

        if let Some(texture_slots) = queues.texture_slots
            && texture_slots.available_slots() <= tick_config.min_texture_slots_available_for_decode
        {
            if texture_slots.waiting_gpu_completion > 0
                || texture_slots.waiting_decoder_reuse > 0
                || queues.active_render_leases > 0
                || queues.deferred_render_releases > 0
            {
                return SeekProgressBlocker::WaitingForGpuRelease;
            }

            return SeekProgressBlocker::WaitingForFreeSurface;
        }

        if !gate_snapshot.target_frame_presented {
            return self.video_target_frame_blocker(queues);
        }

        if gate_snapshot.ready_video_frames < gate_snapshot.required_video_frames {
            return SeekProgressBlocker::WaitingForVideoResumePreroll;
        }

        if !audio_gate_ready {
            return gate_snapshot
                .audio_gate_status
                .blocker()
                .unwrap_or(SeekProgressBlocker::WaitingForAudioPreroll);
        }

        SeekProgressBlocker::Unknown
    }

    /// Уточняет blocker для состояния, где seek ещё не показал target frame.
    fn video_target_frame_blocker(
        &self,
        queues: PipelineQueueDepthSnapshot,
    ) -> SeekProgressBlocker {
        if queues.decoder_send_queue_depth > 0 || queues.decoder_in_flight_packets > 0 {
            return SeekProgressBlocker::WaitingForDecoderOutput;
        }

        if queues.pending_video_packets > 0 {
            return SeekProgressBlocker::WaitingForDecoderInput;
        }

        if self.is_demuxing_active() && !self.draining_after_eof {
            return SeekProgressBlocker::WaitingForDemux;
        }

        if self.pipeline.has_present_video_frame() || !self.pipeline.video_present_queue_is_empty()
        {
            return SeekProgressBlocker::WaitingForScheduler;
        }

        SeekProgressBlocker::WaitingForVideoTargetFrame
    }

    /// Возвращает требуемый video preroll для конкретного seek transaction-а.
    fn required_seek_resume_video_ready_frames(
        &self,
        seek_commit: SeekCommitState,
        resume_video_min_ready_frames: usize,
    ) -> usize {
        match (seek_commit.kind, seek_commit.resume_intent) {
            (SeekCommitKind::Final, PlaybackResumeIntent::Play)
                if self.pipeline.has_selected_audio_track() =>
            {
                1
            }
            (SeekCommitKind::Final, PlaybackResumeIntent::Play) => {
                resume_video_min_ready_frames.max(1)
            }
            _ => 1,
        }
    }

    /// Считает target/current frame и уже декодированные future frames для seek resume.
    fn seek_ready_video_frame_count(&self, target_position: Duration) -> usize {
        let current_frame_ready = self.pipeline.present_video_frame_covers(target_position)
            && !self.snapshot.timeline.stale_frame;
        let queued_ready_frames = self
            .pipeline
            .queued_video_frames()
            .filter(|frame| frame.pts >= target_position)
            .count();

        usize::from(current_frame_ready) + queued_ready_frames
    }

    /// EOF fallback готов только если показан свежий frame текущего final seek transition-а.
    fn seek_eof_fallback_video_ready(
        &self,
        seek_commit: SeekCommitState,
        target_position: Duration,
    ) -> bool {
        if seek_commit.kind != SeekCommitKind::Final || !self.draining_after_eof {
            return false;
        }

        let Some(fallback_position) = self.seek_eof_fallback_video_position else {
            return false;
        };
        let fallback_position = fallback_position.as_duration();
        if fallback_position >= target_position || self.snapshot.timeline.stale_frame {
            return false;
        }

        self.pipeline.present_video_frame_matches(fallback_position)
    }

    /// Audio gate готов после clear ack, runtime decoder/output и минимального preroll.
    ///
    /// Preview и paused final seek не включают audio stream, поэтому после clear ack
    /// они не ждут decoder/output. Final resume в `Playing` ждёт selected audio path:
    /// unsupported/disabled audio должен быть явно снят с selection policy-слоем.
    fn seek_audio_gate_ready(
        &self,
        seek_commit: SeekCommitState,
        resume_audio_min_buffer_ms: f64,
    ) -> bool {
        self.seek_audio_gate_status(seek_commit, resume_audio_min_buffer_ms)
            .is_ready()
    }

    /// Возвращает typed status audio gate-а без схлопывания разных причин в `bool`.
    fn seek_audio_gate_status(
        &self,
        seek_commit: SeekCommitState,
        resume_audio_min_buffer_ms: f64,
    ) -> SeekAudioGateStatus {
        classify_seek_audio_gate(
            seek_commit,
            self.pipeline.audio_seek_runtime_state(),
            self.pipeline.audio_buffer_clear_generation(),
            self.audio_buffer_level_ms(),
            resume_audio_min_buffer_ms,
        )
    }

    /// Успешно закрывает seek transaction и применяет сохранённый resume intent.
    fn complete_seek_commit(&mut self, seek_commit: SeekCommitState) {
        match seek_commit.kind {
            SeekCommitKind::Final => self.complete_final_seek_commit(seek_commit),
            SeekCommitKind::Preview => self.complete_preview_seek_commit(seek_commit),
        }
    }

    /// Закрывает финальный seek и публикует новую playback позицию.
    fn complete_final_seek_commit(&mut self, seek_commit: SeekCommitState) {
        debug!(
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            generation = seek_commit.generation,
            resume_intent = ?seek_commit.resume_intent,
            "Final seek commit завершён"
        );
        self.seek_commit = None;
        self.scrub_state.clear();
        self.seek_eof_fallback_video_position = None;
        self.clear_seek_preroll_fallback_frame();
        self.pipeline
            .set_media_clock_base(seek_commit.target_position.as_duration());
        self.pipeline.clear_monotonic_media_clock();
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.stale_frame = false;
        self.snapshot.timeline.preview_state = TimelinePreviewState::Inactive;
        self.publish_position_changed(seek_commit.target_position.as_duration());

        match seek_commit.resume_intent {
            PlaybackResumeIntent::Pause => {
                self.pause_audio_output_for_seek();
                self.set_playback_state(PlaybackState::Paused);
            }
            PlaybackResumeIntent::Play => {
                if let Some(Err(error)) = self.pipeline.play_audio_output() {
                    warn!(error = %error, "Не удалось запустить audio после seek");
                    self.set_runtime_error(format!("Audio play after seek error: {error}"));
                }
                let observed_at = Instant::now();
                let audio_now = self.audio_clock_now();
                self.pipeline
                    .reset_audio_clock_sample(audio_now, observed_at);
                self.set_playback_state(PlaybackState::Playing);
                self.anchor_monotonic_media_clock_if_needed(observed_at);
            }
        }
    }

    /// Закрывает live preview seek, оставляя interactive scrub активным.
    fn complete_preview_seek_commit(&mut self, seek_commit: SeekCommitState) {
        let target_position = seek_commit.target_position.as_duration();
        let preview_state = if self.present_frame_covers_target(target_position) {
            TimelinePreviewState::Ready
        } else if self.seek_preview_visible_frame_ready(seek_commit) {
            TimelinePreviewState::Visible
        } else {
            TimelinePreviewState::Ready
        };

        self.seek_commit = None;
        if let Some(generation) = seek_commit.scrub_generation {
            let _preview_completion = self.scrub_state.complete_preview(generation);
        }
        self.seek_eof_fallback_video_position = None;
        self.clear_seek_preroll_fallback_frame();
        self.snapshot.timeline.target_position = Some(seek_commit.target_position);
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.stale_frame = false;
        self.snapshot.timeline.preview_state = preview_state;
        self.pause_audio_output_for_seek();
        self.set_playback_state(PlaybackState::Paused);
    }

    /// Прерывает seek transaction по timeout как recoverable error и оставляет media paused.
    fn fail_seek_commit_on_timeout(&mut self, seek_commit: SeekCommitState) {
        match seek_commit.kind {
            SeekCommitKind::Final => self.fail_final_seek_commit_on_timeout(seek_commit),
            SeekCommitKind::Preview => self.fail_preview_seek_commit_on_timeout(seek_commit),
        }
    }

    /// Прерывает финальный seek transaction по timeout как recoverable error.
    fn fail_final_seek_commit_on_timeout(&mut self, seek_commit: SeekCommitState) {
        self.seek_commit = None;
        self.scrub_state.clear();
        self.seek_eof_fallback_video_position = None;
        self.clear_seek_preroll_fallback_frame();
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.stale_frame = false;
        self.snapshot.timeline.preview_state = TimelinePreviewState::Inactive;
        self.pause_audio_output_for_seek();
        self.set_playback_state(PlaybackState::Paused);

        let error = PlayerError::new(
            PlayerErrorKind::SeekTimeout,
            format!(
                "Seek commit timeout after target={} ms, actual demux={} ms",
                seek_commit.target_position.as_duration().as_millis(),
                seek_commit.actual_position.as_duration().as_millis()
            ),
        );
        self.record_recoverable_error(error);
    }

    /// Прерывает только live preview: scrub остаётся живым, финальный commit ещё возможен.
    fn fail_preview_seek_commit_on_timeout(&mut self, seek_commit: SeekCommitState) {
        self.seek_commit = None;
        if let Some(generation) = seek_commit.scrub_generation {
            let _preview_invalidation = self.scrub_state.invalidate_completed_preview(generation);
        }
        self.seek_eof_fallback_video_position = None;
        self.clear_seek_preroll_fallback_frame();
        self.snapshot.timeline.target_position = Some(seek_commit.target_position);
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame()
            && !self.present_frame_covers_target(seek_commit.target_position.as_duration());
        self.snapshot.timeline.preview_state = TimelinePreviewState::Expired;
        self.pause_audio_output_for_seek();
        self.set_playback_state(PlaybackState::Paused);
        debug!(
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            "Live scrub preview seek timed out"
        );
    }

    /// Инициализирует video pipeline через backend factory boundary.
    pub fn init_video_pipeline(&mut self, backend_factory: &dyn VideoBackendFactory) {
        match backend_factory.start_video_backend() {
            Ok(started_backend) => {
                self.pipeline
                    .set_video_decoder_thread_handle(started_backend.into_decoder_thread());
                info!(
                    backend = self.pipeline.video_backend_name(),
                    "Video backend started"
                );
            }
            Err(error) => {
                warn!(error = %error, "Video backend unavailable, no hardware decode");
            }
        }
    }

    /// Принимает open request и переводит session в `Opening`.
    fn open_media(&mut self, request: MediaOpenRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.pending_autoplay = request.autoplay;
        self.snapshot.source_label = Some(request.source.label());
        self.snapshot.media_title = None;
        self.snapshot.clear_timeline();
        self.clear_error();
        self.pending_events
            .push(PlayerEvent::MediaOpenRequested(request));
        self.set_playback_state(PlaybackState::Opening);
        Ok(())
    }

    /// Переводит playback в `Playing` и запускает audio output.
    fn play(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        if let Some(seek_commit) = self.seek_commit.as_mut() {
            seek_commit.resume_intent = PlaybackResumeIntent::Play;
            self.set_playback_state(PlaybackState::Seeking);
            return Ok(());
        }

        if self.draining_after_eof {
            return self.restart_playback_after_eof();
        }

        self.set_playback_state(PlaybackState::Playing);

        if let Some(play_result) = self.pipeline.play_audio_output() {
            if let Err(error) = play_result {
                warn!(error = %error, "Не удалось запустить audio");
                self.set_runtime_error(format!("Audio play error: {error}"));
            }
            let observed_at = Instant::now();
            let audio_now = self.audio_clock_now();
            self.pipeline
                .reset_audio_clock_sample(audio_now, observed_at);
        }
        self.anchor_monotonic_media_clock_if_needed(Instant::now());

        Ok(())
    }

    /// Запускает повторное воспроизведение после штатного EOF через обычный seek pipeline.
    fn restart_playback_after_eof(&mut self) -> PlayerResult<()> {
        if !self.pipeline.has_demuxer() {
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Replay невозможен: media pipeline уже закрыт",
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        if !self.snapshot.timeline.seekable {
            let reason = self
                .snapshot
                .timeline
                .not_seekable_reason
                .unwrap_or(TimelineNotSeekableReason::UnknownTimeline);
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                format!("Replay невозможен: timeline не seekable ({reason:?})"),
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        self.start_seek_transaction(
            MediaTime::ZERO,
            crate::SeekMode::Accurate,
            SeekCommitKind::Final,
            None,
        )
    }

    /// Запускает preroll перед autoplay, не включая audio stream раньше заполнения buffer.
    fn begin_autoplay_preroll(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.pending_autoplay = false;
        self.set_playback_state(PlaybackState::Buffering);
        let observed_at = Instant::now();
        let audio_now = self.audio_clock_now();
        self.pipeline
            .reset_audio_clock_sample(audio_now, observed_at);
        Ok(())
    }

    /// Завершает autoplay preroll, когда audio/video уже готовы к слышимому старту.
    pub(crate) fn finish_autoplay_preroll_if_ready(
        &mut self,
        audio_preroll_target_ms: f64,
    ) -> PlayerResult<bool> {
        if self.snapshot.playback_state != PlaybackState::Buffering {
            return Ok(false);
        }

        if !self.autoplay_preroll_ready(audio_preroll_target_ms) {
            return Ok(false);
        }

        self.play()?;
        Ok(true)
    }

    /// Проверяет минимальный readiness для перехода из `Buffering` в `Playing`.
    fn autoplay_preroll_ready(&self, audio_preroll_target_ms: f64) -> bool {
        let audio_ready = self
            .audio_buffer_level_ms()
            .map(|level_ms| level_ms >= audio_preroll_target_ms.max(1.0))
            .unwrap_or(true);
        let video_ready =
            !self.pipeline.has_selected_video_track() || self.pipeline.has_present_video_frame();

        audio_ready && video_ready
    }

    /// Переводит playback в `Paused` и останавливает audio output.
    fn pause(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.clear_monotonic_media_clock_anchor(Instant::now());
        if let Some(seek_commit) = self.seek_commit.as_mut() {
            seek_commit.resume_intent = PlaybackResumeIntent::Pause;
            self.pause_audio_output_for_seek();
            self.set_playback_state(PlaybackState::Seeking);
            return Ok(());
        }

        self.set_playback_state(PlaybackState::Paused);
        self.clear_queued_video_frames();

        if let Some(Err(error)) = self.pipeline.pause_audio_output() {
            warn!(error = %error, "Не удалось остановить audio");
            self.set_runtime_error(format!("Audio pause error: {error}"));
        }

        Ok(())
    }

    /// Запускает настоящий seek transaction через текущий demuxer.
    fn seek(&mut self, request: SeekRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        let target_position = self.resolve_seek_target(request);
        self.pending_events
            .push(PlayerEvent::SeekRequested(request));

        if !self.pipeline.has_demuxer() {
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Seek невозможен: media pipeline ещё не открыт",
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        self.start_seek_transaction(target_position, request.mode, SeekCommitKind::Final, None)
    }

    /// Начинает interactive scrub на уровне контракта без запуска demux seek.
    fn begin_scrub(&mut self, generation: ScrubGeneration) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.scrub_state.begin(generation);
        self.snapshot.timeline.scrubbing = true;
        self.snapshot.timeline.stale_frame = true;
        self.snapshot.timeline.target_position = Some(self.snapshot.timeline.current_position);
        self.snapshot.timeline.preview_state = TimelinePreviewState::Inactive;
        Ok(())
    }

    /// Запоминает последнюю цель scrub без изменения текущей playback позиции.
    fn update_scrub(&mut self, intent: ScrubUpdateIntent) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        let accepted_intent = match self.scrub_state.accept_update(intent) {
            SessionScrubUpdateAcceptance::Accepted { intent } => intent,
            SessionScrubUpdateAcceptance::Rejected { .. } => return Ok(()),
        };

        let request = accepted_intent.request;
        let target_position = self.resolve_seek_target(request);
        self.snapshot.timeline.scrubbing = true;
        self.snapshot.timeline.stale_frame = true;
        self.snapshot.timeline.target_position = Some(target_position);
        self.snapshot.timeline.preview_state = TimelinePreviewState::Pending;
        self.pending_events
            .push(PlayerEvent::SeekRequested(request));
        Ok(())
    }

    /// Делает live preview seek для активного scrub-а без фиксации playback позиции.
    fn preview_scrub(&mut self, intent: ScrubUpdateIntent) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        let accepted_intent = match self.scrub_state.accept_update(intent) {
            SessionScrubUpdateAcceptance::Accepted { intent } => intent,
            SessionScrubUpdateAcceptance::Rejected { .. } => return Ok(()),
        };

        let request = accepted_intent.request;
        let target_position = self.resolve_seek_target(request);
        self.snapshot.timeline.scrubbing = true;
        self.snapshot.timeline.stale_frame = true;
        self.snapshot.timeline.target_position = Some(target_position);
        self.snapshot.timeline.preview_state = TimelinePreviewState::Pending;
        let _preview_invalidation = self
            .scrub_state
            .invalidate_completed_preview(accepted_intent.generation);
        self.start_seek_transaction(
            target_position,
            request.mode,
            SeekCommitKind::Preview,
            Some(accepted_intent.generation),
        )
    }

    /// Завершает scrub и применяет последнюю цель согласно выбранной политике.
    fn end_scrub(&mut self, intent: ScrubCommitIntent) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        match self.scrub_state.finish_decision(intent.generation) {
            SessionScrubFinishDecision::Current { .. } => {}
            SessionScrubFinishDecision::Rejected { .. } => return Ok(()),
        };

        if let Some(target_position) = self.snapshot.timeline.target_position {
            let seek_mode = self.seek_mode_for_scrub_commit(intent.generation);
            self.apply_scrub_commit_policy(target_position, seek_mode, intent)?;
        }
        self.snapshot.timeline.scrubbing = false;
        let _finish_decision = self.scrub_state.close_current(intent.generation);
        Ok(())
    }

    /// Возвращает `SeekMode` последнего update-а, чтобы release не терял точность команды.
    fn seek_mode_for_scrub_commit(&self, generation: ScrubGeneration) -> SeekMode {
        self.scrub_state.latest_seek_mode(generation)
    }

    /// Применяет выбранную policy release-а через один typed `match`.
    fn apply_scrub_commit_policy(
        &mut self,
        target_position: MediaTime,
        seek_mode: crate::SeekMode,
        intent: ScrubCommitIntent,
    ) -> PlayerResult<()> {
        debug!(
            policy = ?intent.policy,
            generation = intent.generation.as_u64(),
            target_ms = target_position.as_duration().as_millis(),
            seek_mode = ?seek_mode,
            preview_state = ?self.snapshot.timeline.preview_state,
            stale_frame = self.snapshot.timeline.stale_frame,
            "Session выбирает EndScrub commit policy"
        );
        match intent.policy {
            ScrubCommitPolicy::CommitLatestTarget => {
                self.commit_latest_target_as_final(target_position, seek_mode, intent.generation)
            }
            ScrubCommitPolicy::CommitVisiblePreview => self
                .commit_visible_preview_or_latest_target(
                    target_position,
                    seek_mode,
                    intent.generation,
                ),
            ScrubCommitPolicy::CommitLatestTargetWithVisiblePreviewFallback => self
                .commit_latest_target_with_visible_preview_fallback(
                    target_position,
                    seek_mode,
                    intent.generation,
                ),
        }
    }

    /// Коммитит latest target, переиспользуя уже запущенный preview decode path.
    fn commit_latest_target_as_final(
        &mut self,
        target_position: MediaTime,
        seek_mode: crate::SeekMode,
        generation: ScrubGeneration,
    ) -> PlayerResult<()> {
        if self.complete_ready_preview_seek_as_final(target_position, generation) {
            return Ok(());
        }

        if self.promote_active_preview_seek_to_final(target_position, generation) {
            return Ok(());
        }

        self.start_seek_transaction(
            target_position,
            seek_mode,
            SeekCommitKind::Final,
            Some(generation),
        )
    }

    /// CommitVisiblePreview предпочитает уже показанный кадр, но без него закрывает latest target.
    fn commit_visible_preview_or_latest_target(
        &mut self,
        target_position: MediaTime,
        seek_mode: crate::SeekMode,
        generation: ScrubGeneration,
    ) -> PlayerResult<()> {
        if self.commit_visible_preview_as_final(generation) {
            return Ok(());
        }

        self.commit_latest_target_as_final(target_position, seek_mode, generation)
    }

    /// Hybrid оставляет текущий frame stale-feedback-ом и финально доезжает до latest target.
    fn commit_latest_target_with_visible_preview_fallback(
        &mut self,
        target_position: MediaTime,
        seek_mode: crate::SeekMode,
        generation: ScrubGeneration,
    ) -> PlayerResult<()> {
        self.commit_latest_target_as_final(target_position, seek_mode, generation)
    }

    /// CommitVisiblePreview фиксирует именно frame, который уже был реально показан.
    fn commit_visible_preview_as_final(&mut self, generation: ScrubGeneration) -> bool {
        if !self.snapshot.timeline.scrubbing {
            return false;
        }

        if self.seek_commit.is_some_and(|seek_commit| {
            seek_commit.kind != SeekCommitKind::Preview
                || seek_commit.scrub_generation != Some(generation)
        }) {
            return false;
        }

        let Some(visible_preview) = self.scrub_state.visible_preview_for(generation) else {
            return false;
        };

        if !self.present_frame_matches_position(visible_preview.position.as_duration()) {
            return false;
        }

        let packet_generation = self
            .seek_commit
            .map(|seek_commit| seek_commit.generation)
            .unwrap_or_else(|| self.pipeline.seek_generation());
        let actual_position = self
            .seek_commit
            .map(|seek_commit| seek_commit.actual_position)
            .unwrap_or(visible_preview.position);
        let promoted_seek_commit = SeekCommitState {
            generation: packet_generation,
            scrub_generation: Some(generation),
            target_position: visible_preview.position,
            actual_position,
            started_at: Instant::now(),
            resume_intent: PlaybackResumeIntent::Pause,
            kind: SeekCommitKind::Final,
        };
        self.complete_final_seek_commit(promoted_seek_commit);
        true
    }

    /// Активный preview latest target-а становится final transaction-ом без restart seek-а.
    fn promote_active_preview_seek_to_final(
        &mut self,
        target_position: MediaTime,
        generation: ScrubGeneration,
    ) -> bool {
        let Some(seek_commit) = self.seek_commit.as_mut() else {
            return false;
        };

        if seek_commit.kind != SeekCommitKind::Preview
            || seek_commit.target_position != target_position
            || seek_commit.scrub_generation != Some(generation)
        {
            return false;
        }

        debug!(
            generation = generation.as_u64(),
            target_ms = target_position.as_duration().as_millis(),
            packet_generation = seek_commit.generation,
            "Active preview promoted to final without demux restart"
        );
        seek_commit.kind = SeekCommitKind::Final;
        seek_commit.resume_intent = PlaybackResumeIntent::Pause;
        seek_commit.started_at = Instant::now();
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.seeking = true;
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame()
            && !self.present_frame_covers_target(target_position.as_duration());
        self.snapshot.timeline.target_position = Some(target_position);
        self.snapshot.timeline.preview_state = TimelinePreviewState::Inactive;
        self.drop_queued_video_frames_before_target(target_position.as_duration());
        true
    }

    /// Завершённый preview с уже показанным target frame коммитится сразу.
    fn complete_ready_preview_seek_as_final(
        &mut self,
        target_position: MediaTime,
        generation: ScrubGeneration,
    ) -> bool {
        if !self.completed_preview_can_commit_as_final(target_position, generation) {
            return false;
        }

        let promoted_seek_commit = SeekCommitState {
            generation: self.pipeline.seek_generation(),
            scrub_generation: Some(generation),
            target_position,
            actual_position: target_position,
            started_at: Instant::now(),
            resume_intent: PlaybackResumeIntent::Pause,
            kind: SeekCommitKind::Final,
        };
        self.complete_final_seek_commit(promoted_seek_commit);
        true
    }

    /// Проверяет, что preview действительно дошёл до target frame и не является stale UI state.
    fn completed_preview_can_commit_as_final(
        &self,
        target_position: MediaTime,
        generation: ScrubGeneration,
    ) -> bool {
        self.seek_commit.is_none()
            && self.snapshot.timeline.scrubbing
            && self.snapshot.timeline.target_position == Some(target_position)
            && self.scrub_state.completed_preview_for(generation)
            && !self.snapshot.timeline.seeking
            && !self.snapshot.timeline.stale_frame
            && self.snapshot.timeline.preview_state == TimelinePreviewState::Ready
            && self.present_frame_covers_target(target_position.as_duration())
    }

    /// Проверяет, что текущий video frame уже соответствует target; audio-only media проходит.
    fn present_frame_covers_target(&self, target_position: Duration) -> bool {
        if !self.pipeline.has_selected_video_track() {
            return true;
        }

        self.pipeline.present_video_frame_covers(target_position)
    }

    /// Проверяет, что текущий present frame ровно тот preview-кадр, который хотим зафиксировать.
    fn present_frame_matches_position(&self, position: Duration) -> bool {
        self.pipeline.has_selected_video_track()
            && self.pipeline.present_video_frame_matches(position)
    }

    /// Убирает pre-target кадры из future queue при переходе preview -> final.
    fn drop_queued_video_frames_before_target(&mut self, target_position: Duration) {
        let mut retained_frames = std::collections::VecDeque::new();

        while let Some(frame) = self.pipeline.pop_queued_video_frame_front() {
            if frame.pts < target_position {
                self.release_video_texture(frame.texture_handle);
            } else {
                retained_frames.push_back(frame);
            }
        }

        for frame in retained_frames {
            self.pipeline.enqueue_queued_video_frame(frame);
        }
    }

    /// Сбрасывает video decoder перед seek и делает flush явной fail-fast границей.
    fn reset_video_decoder_for_seek(&self) -> Result<(), PlayerError> {
        self.pipeline
            .flush_video_decoder_thread()
            .map_err(player_error_from_decoder_flush_error)
    }

    /// Завершает seek transaction до demux seek, если decoder не подтвердил flush.
    fn fail_seek_transaction_on_decoder_flush(
        &mut self,
        target_position: MediaTime,
        commit_kind: SeekCommitKind,
        error: PlayerError,
    ) {
        self.seek_commit = None;
        self.seek_eof_fallback_video_position = None;
        self.clear_seek_preroll_fallback_frame();
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.stale_frame = true;
        self.snapshot.timeline.target_position = match commit_kind {
            SeekCommitKind::Final => None,
            SeekCommitKind::Preview => Some(target_position),
        };
        self.snapshot.timeline.preview_state = match commit_kind {
            SeekCommitKind::Final => TimelinePreviewState::Inactive,
            SeekCommitKind::Preview => TimelinePreviewState::Failed,
        };
        if commit_kind == SeekCommitKind::Final {
            self.scrub_state.clear();
            self.snapshot.timeline.scrubbing = false;
        }
        self.set_playback_state(PlaybackState::Paused);
        self.record_recoverable_error(error);
    }

    /// Выполняет синхронную часть seek transaction и оставляет commit gates на tick.
    fn start_seek_transaction(
        &mut self,
        target_position: MediaTime,
        seek_mode: crate::SeekMode,
        commit_kind: SeekCommitKind,
        scrub_generation: Option<ScrubGeneration>,
    ) -> PlayerResult<()> {
        if !self.snapshot.timeline.seekable {
            let reason = self
                .snapshot
                .timeline
                .not_seekable_reason
                .unwrap_or(TimelineNotSeekableReason::UnknownTimeline);
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                format!("Seek невозможен: timeline не seekable ({reason:?})"),
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        if !self.pipeline.has_demuxer() {
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Seek невозможен: media pipeline ещё не открыт",
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        let target_duration = target_position.as_duration();
        let demux_seek_request = match demux_seek_request_for_transaction(
            commit_kind,
            self.pipeline.has_selected_video_track(),
            target_duration,
            seek_mode,
        ) {
            Ok(request) => request,
            Err(error) => {
                if commit_kind == SeekCommitKind::Preview {
                    self.snapshot.timeline.target_position = Some(target_position);
                    self.snapshot.timeline.preview_state = TimelinePreviewState::Failed;
                    self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame();
                }
                self.record_recoverable_error(player_error_from_seek_demux_request_error(error));
                return Ok(());
            }
        };

        let resume_intent = match commit_kind {
            SeekCommitKind::Final => {
                self.scrub_state.prepare_final_commit();
                self.snapshot.timeline.preview_state = TimelinePreviewState::Inactive;
                PlaybackResumeIntent::from_playback_state(self.playback_state())
            }
            SeekCommitKind::Preview => {
                if let Some(generation) = scrub_generation {
                    let _preview_invalidation =
                        self.scrub_state.invalidate_completed_preview(generation);
                }
                self.snapshot.timeline.preview_state = TimelinePreviewState::Pending;
                PlaybackResumeIntent::Pause
            }
        };

        self.pause_audio_output_for_seek();
        if let Err(error) = self.reset_video_decoder_for_seek() {
            self.fail_seek_transaction_on_decoder_flush(target_position, commit_kind, error);
            return Ok(());
        }

        self.set_playback_state(PlaybackState::Seeking);
        let generation = self.pipeline.begin_seek_generation();
        let has_video = self.pipeline.has_selected_video_track();

        self.pipeline.clear_pending_packets_for_seek();
        self.pipeline.reset_decoder_state_for_seek(has_video);
        self.seek_eof_fallback_video_position = None;
        self.clear_seek_preroll_fallback_frame();
        self.clear_queued_video_frames();
        self.pipeline.reset_clocks_for_seek(target_duration);
        self.snapshot.timeline.target_position = Some(target_position);
        self.snapshot.timeline.seeking = true;
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame();
        self.snapshot.timeline.preview_state = match commit_kind {
            SeekCommitKind::Final => TimelinePreviewState::Inactive,
            SeekCommitKind::Preview => TimelinePreviewState::Pending,
        };

        if let Some(Err(error)) = self.pipeline.reset_audio_decoder() {
            let player_error = PlayerError::new(
                PlayerErrorKind::RuntimeError,
                format!("Audio decoder reset failed during seek: {error}"),
            );
            self.record_recoverable_error(player_error);
        }

        if let Some(clear_result) = self.pipeline.clear_audio_output_for_seek(generation) {
            match clear_result {
                Ok(ack_generation) => {
                    self.pipeline.mark_audio_buffer_clear_ack(ack_generation);
                }
                Err(error) => {
                    let player_error = PlayerError::new(
                        PlayerErrorKind::AudioDeviceUnavailable,
                        format!("Audio buffer clear failed during seek: {error}"),
                    );
                    self.record_recoverable_error(player_error);
                }
            }
        } else {
            self.pipeline.mark_audio_buffer_clear_ack(generation);
            self.pipeline.reset_audio_clock();
        }

        let seek_result = {
            debug!(
                kind = ?commit_kind,
                target_ms = target_duration.as_millis(),
                demux_mode = ?demux_seek_request.mode,
                generation,
                scrub_generation = ?scrub_generation.map(ScrubGeneration::as_u64),
                "Starting demux seek transaction"
            );
            let Some(seek_result) = self.pipeline.seek_demuxer(demux_seek_request) else {
                return Ok(());
            };
            seek_result
        };

        match seek_result {
            Ok(result) => {
                debug!(
                    kind = ?commit_kind,
                    target_ms = target_duration.as_millis(),
                    actual_ms = result.actual_position.as_duration().as_millis(),
                    generation,
                    "Demux seek transaction accepted"
                );
                self.seek_commit = Some(SeekCommitState {
                    generation,
                    scrub_generation,
                    target_position,
                    actual_position: result.actual_position,
                    started_at: Instant::now(),
                    resume_intent,
                    kind: commit_kind,
                });
                Ok(())
            }
            Err(error) => {
                self.seek_commit = None;
                self.snapshot.timeline.seeking = false;
                self.snapshot.timeline.stale_frame = match commit_kind {
                    SeekCommitKind::Final => false,
                    SeekCommitKind::Preview => self.pipeline.has_present_video_frame(),
                };
                self.set_playback_state(PlaybackState::Paused);
                self.snapshot.timeline.target_position = match commit_kind {
                    SeekCommitKind::Final => None,
                    SeekCommitKind::Preview => Some(target_position),
                };
                self.snapshot.timeline.preview_state = match commit_kind {
                    SeekCommitKind::Final => TimelinePreviewState::Inactive,
                    SeekCommitKind::Preview => TimelinePreviewState::Failed,
                };
                let player_error = player_error_from_demux_seek_error(error);
                self.record_recoverable_error(player_error);
                Ok(())
            }
        }
    }

    /// Останавливает audio stream для seek, не меняя high-level playback state.
    fn pause_audio_output_for_seek(&mut self) {
        if let Some(Err(error)) = self.pipeline.pause_audio_output() {
            warn!(error = %error, "Не удалось остановить audio перед seek");
            self.set_runtime_error(format!("Audio pause before seek error: {error}"));
        }
    }

    /// Останавливает текущий media и сбрасывает timeline без завершения session.
    fn stop(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.reset_media_state();
        self.set_playback_state(PlaybackState::Stopped);
        Ok(())
    }

    /// Валидирует и устанавливает громкость.
    fn set_volume(&mut self, volume: f32) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
            let error = PlayerError::new(
                PlayerErrorKind::InvalidCommand,
                format!("volume must be finite and within 0.0..=1.0, got {volume}"),
            );
            self.record_recoverable_error(error.clone());
            return Err(error);
        }

        self.snapshot.volume = volume;
        self.snapshot.muted = volume <= f32::EPSILON;
        let _output_was_present = self.pipeline.set_audio_output_volume(volume);
        Ok(())
    }

    /// Выбирает video track.
    fn select_video_track(&mut self, track_id: TrackId) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.pipeline
            .select_video_track_preserving_active_requirement(track_id);
        self.snapshot.selected_tracks.video_track = Some(track_id);
        self.pending_events
            .push(PlayerEvent::VideoTrackSelected(track_id));
        Ok(())
    }

    /// Выбирает audio track.
    fn select_audio_track(&mut self, track_id: TrackId) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.pipeline.select_audio_track(track_id);
        self.snapshot.selected_tracks.audio_track = Some(track_id);
        self.pending_events
            .push(PlayerEvent::AudioTrackSelected(track_id));
        Ok(())
    }

    /// Выбирает subtitle track или отключает субтитры.
    fn select_subtitle_track(&mut self, track_id: Option<TrackId>) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.snapshot.selected_tracks.subtitle_track = track_id;
        self.pending_events
            .push(PlayerEvent::SubtitleTrackSelected(track_id));
        Ok(())
    }

    /// Фиксирует выбор качества как событие для будущего source/service слоя.
    fn select_quality(&mut self, selection: QualitySelection) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.pending_events
            .push(PlayerEvent::QualitySelectionChanged(selection));
        Ok(())
    }

    /// Запрашивает reload config без чтения файлов внутри player-core.
    fn reload_config(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.pending_events.push(PlayerEvent::ConfigReloadRequested);
        Ok(())
    }

    /// Переводит session в stopped state.
    fn shutdown(&mut self) -> PlayerResult<()> {
        self.shutdown_requested = true;
        self.pending_events.push(PlayerEvent::ShutdownRequested);
        self.set_playback_state(PlaybackState::Stopped);
        Ok(())
    }

    /// Запрещает команды после shutdown, кроме самого idempotent shutdown.
    fn ensure_not_shutdown(&mut self) -> PlayerResult<()> {
        if !self.shutdown_requested {
            return Ok(());
        }

        let error = PlayerError::new(
            PlayerErrorKind::InvalidCommand,
            "player session is already shut down",
        );
        self.record_recoverable_error(error.clone());
        Err(error)
    }

    /// Обновляет playback state и публикует событие только при реальном изменении.
    fn set_playback_state(&mut self, playback_state: PlaybackState) {
        let previous_state = self.playback_state();
        self.draining_after_eof = playback_state == PlaybackState::Draining;
        self.snapshot.playback_state = playback_state;

        if previous_state == playback_state {
            return;
        }

        self.pending_events
            .push(PlayerEvent::PlaybackStateChanged(playback_state));
    }

    /// Сохраняет recoverable error в snapshot и event queue.
    fn record_recoverable_error(&mut self, error: PlayerError) {
        self.snapshot.last_error = Some(error.clone());
        self.pending_events
            .push(PlayerEvent::RecoverableError(error));
    }

    /// Публикует редкое явное изменение позиции, например seek.
    fn publish_position_changed(&mut self, position: Duration) {
        self.update_current_position(position);
        self.pending_events
            .push(PlayerEvent::PositionChanged(position));
    }

    /// Разрешает seek target в абсолютную media-позицию без изменения runtime seek policy.
    fn resolve_seek_target(&self, request: SeekRequest) -> MediaTime {
        let target_position = request
            .target
            .resolve(self.snapshot.timeline.current_position);

        self.snapshot
            .timeline
            .seekable_range
            .map(|range| target_position.clamp_to(range))
            .unwrap_or(target_position)
    }

    /// Синхронно обновляет legacy `Duration` и typed timeline duration.
    fn set_snapshot_duration(&mut self, duration: Option<Duration>) {
        self.snapshot
            .set_timeline_duration(duration.map(MediaDuration::from_duration));
    }

    /// Применяет seekability demuxer/source stack-а к player timeline.
    fn apply_demux_seekability(&mut self, seekability: DemuxSeekability) {
        match seekability {
            DemuxSeekability::Seekable => {}
            DemuxSeekability::NotSeekable { reason } => {
                self.snapshot.timeline.seekable = false;
                self.snapshot.timeline.seekable_range = None;
                self.snapshot.timeline.not_seekable_reason = Some(reason);
            }
        }
    }

    /// Сохраняет runtime error как user-facing ошибку.
    fn set_runtime_error(&mut self, message: String) {
        let error = PlayerError::new(PlayerErrorKind::RuntimeError, message);
        self.snapshot.last_error = Some(error.clone());
        self.pending_events
            .push(PlayerEvent::RecoverableError(error));
    }

    /// Очищает последнюю ошибку после успешного media action.
    fn clear_error(&mut self) {
        self.snapshot.last_error = None;
    }

    /// Ищет первый video track, который проходит capability-based selection.
    fn select_default_video_track(
        &mut self,
        tracks: &[TrackInfo],
        missing_message: &str,
    ) -> PlayerResult<()> {
        let video_tracks = tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .collect::<Vec<_>>();

        if video_tracks.is_empty() {
            info!("{missing_message}");
            return Ok(());
        }

        let mut last_rejection = None;
        for track in video_tracks {
            let Some(requirement) = video_requirement_from_track(track) else {
                last_rejection = Some(PlayerError::new(
                    PlayerErrorKind::UnsupportedVideoCodec,
                    format!(
                        "Video codec `{}` не поддерживается текущей capability model",
                        track.codec_id
                    ),
                ));
                continue;
            };

            match self.validate_video_decode_requirement(&requirement) {
                Ok(()) => {
                    self.activate_video_track(track, requirement);
                    return Ok(());
                }
                Err(error) => {
                    if self.can_defer_packet_refinement(&requirement) {
                        info!(
                            track_id = %track.id,
                            requirement = %requirement.describe(),
                            "Video track выбран до bitstream refinement; strict capability check будет повторён перед decode"
                        );
                        self.activate_video_track(track, requirement);
                        return Ok(());
                    }

                    last_rejection = Some(error);
                }
            }
        }

        Err(last_rejection.unwrap_or_else(|| {
            PlayerError::new(PlayerErrorKind::UnsupportedVideoCodec, missing_message)
        }))
    }

    /// Активирует video track после обычной проверки или разрешённого deferred refinement.
    fn activate_video_track(&mut self, track: &TrackInfo, requirement: VideoDecodeRequirement) {
        self.pipeline.select_video_track(track.id, requirement);
        self.snapshot.selected_tracks.video_track = Some(track.id);
        log_selected_video_track_metadata(track, self.pipeline.active_video_requirement());
    }

    /// Разрешает отложить codec validation до первого packet header-а, если container неполный.
    fn can_defer_packet_refinement(&self, requirement: &VideoDecodeRequirement) -> bool {
        if !video_requirement_needs_packet_refinement(requirement) {
            return false;
        }

        self.capabilities.as_ref().is_some_and(|capabilities| {
            matches!(
                capabilities.check_video_requirement(requirement),
                Err(ref unsupported_requirement)
                    if unsupported_requirement_can_be_refined_by_packet_probe(
                        unsupported_requirement
                    )
            )
        })
    }

    /// Проверяет video stream requirement по последнему capability report.
    pub(crate) fn validate_video_decode_requirement(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> PlayerResult<()> {
        let Some(capabilities) = &self.capabilities else {
            return Ok(());
        };

        match capabilities.check_video_requirement(requirement) {
            Ok(_) => Ok(()),
            Err(error) => Err(player_error_from_unsupported_requirement(error)),
        }
    }

    /// Уточняет active video requirement после bitstream probe.
    pub(crate) fn refine_active_video_requirement(
        &mut self,
        requirement: VideoDecodeRequirement,
    ) -> PlayerResult<()> {
        self.validate_video_decode_requirement(&requirement)?;
        self.pipeline.set_active_video_requirement(requirement);
        Ok(())
    }

    /// Возвращает codec текущего video track по `TrackId`.
    pub(crate) fn video_codec_for_track(&self, track_id: TrackId) -> Option<VideoCodec> {
        self.pipeline
            .tracks()
            .iter()
            .find(|track| track.id == track_id && track.kind == TrackKind::Video)
            .and_then(|track| VideoCodec::from_container_codec_id(&track.codec_id))
    }

    /// Возвращает container metadata source для active track refinement.
    pub(crate) fn video_metadata_source_for_track(
        &self,
        track_id: TrackId,
    ) -> Option<VideoMetadataSource> {
        self.pipeline
            .tracks()
            .iter()
            .find(|track| track.id == track_id && track.kind == TrackKind::Video)
            .and_then(video_metadata_source_from_track)
    }

    /// Выбирает audio track и откладывает decoder/output init до первого packet-а.
    fn init_audio_pipeline(&mut self, tracks: &[TrackInfo]) {
        let init_spec = match audio_decoder_init_spec_from_tracks(tracks) {
            Ok(Some(init_spec)) => init_spec,
            Ok(None) => {
                info!("Audio track не найден — playback без звука");
                return;
            }
            Err(error) => {
                warn!(error = %error, "Audio track rejected during lazy decoder planning");
                self.record_recoverable_error(error);
                return;
            }
        };

        info!(
            track_id = %init_spec.track_id,
            codec_id = %init_spec.codec_id,
            sample_rate = ?init_spec.initial_sample_rate,
            channels = ?init_spec.initial_channels,
            "Audio track выбран; decoder/output будут созданы лениво"
        );

        let decoder_config = audio::AudioDecoderConfig::from_track_metadata(
            init_spec.track_id.get(),
            init_spec.codec_id.clone(),
            init_spec.initial_sample_rate,
            init_spec.initial_channels,
        )
        .with_codec_private(init_spec.codec_private.clone());

        self.pipeline
            .install_deferred_audio_decoder_config(decoder_config);

        self.pipeline.select_audio_track(init_spec.track_id);
        self.snapshot.selected_tracks.audio_track = Some(init_spec.track_id);

        info!(
            track_id = %init_spec.track_id,
            "Audio pipeline подготовлен к lazy init"
        );
    }

    /// Собирает codec/render-neutral queue depths для diagnostics aggregator.
    fn diagnostic_queue_depths(&self) -> PipelineQueueDepthSnapshot {
        let decoder_send_queue_depth = self
            .pipeline
            .video_decoder_packet_queue_depth()
            .unwrap_or(self.pipeline.pending_video_packet_len());
        let decoder_resource_snapshot = self.pipeline.video_decoder_resource_snapshot();

        PipelineQueueDepthSnapshot {
            pending_audio_packets: self.pipeline.pending_audio_packet_len(),
            pending_video_packets: self.pipeline.pending_video_packet_len(),
            present_queue_depth: self.pipeline.video_present_queue_len(),
            decoder_send_queue_depth,
            decoder_in_flight_packets: self.pipeline.video_decode_in_flight_packets(),
            decoder_ready_queue_depth: None,
            active_render_leases: self.pipeline.active_render_lease_count(),
            deferred_render_releases: self.pipeline.deferred_render_release_count(),
            texture_slots: decoder_resource_snapshot.map(|texture_stats| {
                TextureSlotPressureSnapshot {
                    capacity: texture_stats.capacity,
                    slots: texture_stats.slots,
                    in_use: texture_stats.in_use,
                    free_surfaces: texture_stats.free_surfaces,
                    waiting_gpu_completion: texture_stats.waiting_gpu_completion,
                    waiting_decoder_reuse: texture_stats.waiting_decoder_reuse,
                    import_failures: texture_stats.import_failures,
                    imports_created: texture_stats.imports_created,
                    imports_reused: texture_stats.imports_reused,
                    imports_replaced: texture_stats.imports_replaced,
                }
            }),
            decoder_control_channel: self.pipeline.video_decoder_control_channel_pressure(),
        }
    }
}

/// Чистая политика audio gate-а для seek commit-а.
fn classify_seek_audio_gate(
    seek_commit: SeekCommitState,
    audio_runtime_state: AudioSeekRuntimeState,
    audio_buffer_clear_generation: u64,
    audio_buffer_level_ms: Option<f64>,
    resume_audio_min_buffer_ms: f64,
) -> SeekAudioGateStatus {
    if audio_runtime_state == AudioSeekRuntimeState::NoSelectedAudio {
        return SeekAudioGateStatus::Ready;
    }

    if audio_buffer_clear_generation < seek_commit.generation {
        return SeekAudioGateStatus::WaitingForClear;
    }

    if seek_commit.kind == SeekCommitKind::Preview {
        return SeekAudioGateStatus::Ready;
    }

    if seek_commit.resume_intent == PlaybackResumeIntent::Pause {
        return SeekAudioGateStatus::Ready;
    }

    match audio_runtime_state {
        AudioSeekRuntimeState::NoSelectedAudio => SeekAudioGateStatus::Ready,
        AudioSeekRuntimeState::WaitingForDecoder => SeekAudioGateStatus::WaitingForDecoder,
        AudioSeekRuntimeState::WaitingForOutput => SeekAudioGateStatus::WaitingForOutput,
        AudioSeekRuntimeState::Ready => {
            if seek_audio_preroll_ready(audio_buffer_level_ms, resume_audio_min_buffer_ms) {
                SeekAudioGateStatus::Ready
            } else {
                SeekAudioGateStatus::WaitingForPreroll
            }
        }
    }
}

/// Проверяет минимальный audio buffer после seek, не считая absent output успехом.
fn seek_audio_preroll_ready(
    audio_buffer_level_ms: Option<f64>,
    resume_audio_min_buffer_ms: f64,
) -> bool {
    let Some(audio_buffer_level_ms) = audio_buffer_level_ms else {
        return false;
    };

    audio_buffer_level_ms.is_finite()
        && audio_buffer_level_ms >= normalized_seek_audio_preroll_ms(resume_audio_min_buffer_ms)
}

/// Нормализует внешний config seek-preroll-а к безопасному положительному минимуму.
fn normalized_seek_audio_preroll_ms(resume_audio_min_buffer_ms: f64) -> f64 {
    if resume_audio_min_buffer_ms.is_finite() && resume_audio_min_buffer_ms > 0.0 {
        return resume_audio_min_buffer_ms.max(MIN_SEEK_AUDIO_PREROLL_MS);
    }

    MIN_SEEK_AUDIO_PREROLL_MS
}

/// Находит первый audio track, даже если probe ещё не сообщил полный decoded spec.
fn first_audio_track(tracks: &[TrackInfo]) -> Option<&TrackInfo> {
    tracks.iter().find(|track| track.kind == TrackKind::Audio)
}

/// Создаёт чистый lazy-init plan для audio decoder-а без открытия CPAL device.
fn audio_decoder_init_spec_from_track(track: &TrackInfo) -> PlayerResult<AudioDecoderInitSpec> {
    if track.sample_rate == Some(0) {
        return Err(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!(
                "Audio track `{}` содержит нулевой sample_rate для decoder init",
                track.id
            ),
        ));
    }

    if track.channels == Some(0) {
        return Err(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!(
                "Audio track `{}` содержит нулевой channel count для decoder init",
                track.id
            ),
        ));
    }

    Ok(AudioDecoderInitSpec {
        track_id: track.id,
        codec_id: track.codec_id.clone(),
        codec_private: track.codec_private.as_ref().map(|bytes| bytes.to_vec()),
        initial_sample_rate: track.sample_rate,
        initial_channels: track.channels,
    })
}

/// Выбирает audio decoder lazy-init plan по track metadata, не создавая runtime ресурсы.
fn audio_decoder_init_spec_from_tracks(
    tracks: &[TrackInfo],
) -> PlayerResult<Option<AudioDecoderInitSpec>> {
    let Some(track) = first_audio_track(tracks) else {
        return Ok(None);
    };

    audio_decoder_init_spec_from_track(track).map(Some)
}

/// Сохраняет typed unsupported-codec ошибку factory, не меняя остальные init errors.
fn player_error_from_audio_decoder_factory_error(error: anyhow::Error) -> PlayerError {
    if matches!(
        error.downcast_ref::<audio::AudioDecoderError>(),
        Some(audio::AudioDecoderError::UnsupportedCodec { .. })
    ) {
        return PlayerError::new(
            PlayerErrorKind::UnsupportedAudioCodec,
            format!("Audio error: {error}"),
        );
    }

    PlayerError::new(
        PlayerErrorKind::RuntimeError,
        format!("Audio error: {error}"),
    )
}

/// Строит минимальное decode requirement из container track metadata.
fn video_requirement_from_track(track: &TrackInfo) -> Option<VideoDecodeRequirement> {
    let codec = VideoCodec::from_container_codec_id(&track.codec_id)?;
    let Some(container_source) = video_metadata_source_from_track(track) else {
        return Some(VideoDecodeRequirement::new(codec));
    };

    Some(resolve_video_metadata(codec, Some(container_source), None).requirement)
}

/// Проверяет, что отказ относится к metadata, которую codec packet probe может уточнить.
fn unsupported_requirement_can_be_refined_by_packet_probe(
    unsupported_requirement: &UnsupportedVideoRequirement,
) -> bool {
    let is_metadata_rejection = matches!(
        unsupported_requirement.rejections.first(),
        Some(VideoCapabilityRejection::InvalidHdrMetadata { .. })
            | Some(VideoCapabilityRejection::InsufficientStreamMetadata { .. })
    );

    codec_requirement_can_be_refined_by_packet_probe(
        &unsupported_requirement.requirement,
        is_metadata_rejection,
    )
}

/// Собирает codec-neutral resolver source из typed video metadata track-а.
fn video_metadata_source_from_track(track: &TrackInfo) -> Option<VideoMetadataSource> {
    let codec = VideoCodec::from_container_codec_id(&track.codec_id)?;
    let video = track.video.as_ref()?;
    let mut source = VideoMetadataSource::container(codec);
    source.profile = video.profile;
    source.bit_depth = video.bit_depth;
    source.chroma = video.chroma;
    source.width = video.coded_width;
    source.height = video.coded_height;
    if let Some(color) = &video.color {
        source = source.with_color(color.clone());
    }
    Some(source)
}

/// Пишет resolved video/container metadata в logs без codec logic в UI.
fn log_selected_video_track_metadata(
    track: &TrackInfo,
    active_requirement: Option<&VideoDecodeRequirement>,
) {
    let Some(video_metadata) = track.video.as_ref() else {
        return;
    };

    info!(
        track_id = %track.id,
        codec = %track.codec_id,
        width = ?video_metadata.coded_width,
        height = ?video_metadata.coded_height,
        bit_depth = ?video_metadata.bit_depth,
        chroma = ?video_metadata.chroma,
        color = ?video_metadata.color,
        requirement = ?active_requirement,
        "Video track metadata resolved from container"
    );
}

/// Переводит structured capability error в player error model.
fn player_error_from_unsupported_requirement(error: UnsupportedVideoRequirement) -> PlayerError {
    let kind = match error.rejections.first() {
        Some(VideoCapabilityRejection::UnsupportedCodec { .. }) => {
            PlayerErrorKind::UnsupportedVideoCodec
        }
        Some(VideoCapabilityRejection::UnsupportedProfile { .. }) => {
            PlayerErrorKind::UnsupportedVideoProfile
        }
        Some(VideoCapabilityRejection::UnsupportedBitDepth { .. }) => {
            PlayerErrorKind::UnsupportedVideoBitDepth
        }
        Some(VideoCapabilityRejection::UnsupportedChroma { .. }) => {
            PlayerErrorKind::UnsupportedVideoChroma
        }
        Some(VideoCapabilityRejection::UnsupportedHdrRenderer { .. }) => {
            PlayerErrorKind::UnsupportedHdrMode
        }
        Some(VideoCapabilityRejection::InvalidHdrMetadata { .. }) => {
            PlayerErrorKind::UnsupportedHdrMode
        }
        Some(VideoCapabilityRejection::P010NotRenderable { .. }) if error.requirement.hdr => {
            PlayerErrorKind::UnsupportedHdrMode
        }
        Some(VideoCapabilityRejection::NoAvailableRenderer)
        | Some(VideoCapabilityRejection::UnsupportedDeviceExportPath { .. })
        | Some(VideoCapabilityRejection::UnsupportedP010StorageLayout { .. })
        | Some(VideoCapabilityRejection::UnsupportedRenderFrameFormat { .. })
        | Some(VideoCapabilityRejection::P010NotRenderable { .. }) => {
            PlayerErrorKind::UnsupportedRenderFormat
        }
        Some(VideoCapabilityRejection::NoAvailableBackend)
        | Some(VideoCapabilityRejection::UnsupportedDecodeFormat { .. })
        | Some(VideoCapabilityRejection::InsufficientStreamMetadata { .. })
        | None => PlayerErrorKind::HardwareDecoderUnavailable,
    };

    PlayerError::new(kind, error.user_message())
}

impl Default for PlayerSession {
    /// Создаёт пустую session с default snapshot.
    fn default() -> Self {
        Self {
            snapshot: PlayerSnapshot::default(),
            pipeline: PlaybackPipeline::default(),
            diagnostics: PlaybackDiagnostics::default(),
            pending_events: Vec::new(),
            pending_autoplay: false,
            shutdown_requested: false,
            draining_after_eof: false,
            capabilities: None,
            seek_commit: None,
            scrub_state: SessionScrubState::default(),
            direct_scrub_generation_seed: ScrubGeneration::default(),
            seek_eof_fallback_video_position: None,
        }
    }
}

/// Возвращает срез audio samples, который начинается не раньше текущей media clock base.
fn trim_decoded_audio_to_clock_base(
    samples: &[f32],
    packet_pts: Duration,
    media_clock_base: Duration,
    sample_rate: u32,
    channels: u32,
) -> &[f32] {
    if packet_pts >= media_clock_base || sample_rate == 0 || channels == 0 {
        return samples;
    }

    let channel_count = channels as usize;
    let frame_count = samples.len() / channel_count;
    if frame_count == 0 {
        return &[];
    }

    let trim_duration = media_clock_base.saturating_sub(packet_pts);
    let trim_frames = duration_to_audio_frames(trim_duration, sample_rate);
    if trim_frames >= frame_count {
        return &[];
    }

    let trim_samples = trim_frames.saturating_mul(channel_count);
    &samples[trim_samples..]
}

/// Конвертирует duration в количество audio frames с округлением вниз.
fn duration_to_audio_frames(duration: Duration, sample_rate: u32) -> usize {
    let frames = duration.as_nanos().saturating_mul(u128::from(sample_rate)) / 1_000_000_000u128;

    frames.min(usize::MAX as u128) as usize
}

/// Мапит ошибку demux seek в player error без смешивания unavailable/timeout/demux.
fn player_error_from_demux_seek_error(error: anyhow::Error) -> PlayerError {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<MediaDemuxError>()
            .is_some_and(MediaDemuxError::is_seek_unavailable)
    }) {
        return PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            format!("Seek failed: {error}"),
        );
    }

    PlayerError::new(PlayerErrorKind::DemuxError, format!("Seek failed: {error}"))
}

/// Мапит ошибку выбора seek policy до mutating-части transaction-а.
fn player_error_from_seek_demux_request_error(error: SeekDemuxRequestError) -> PlayerError {
    match error {
        SeekDemuxRequestError::UnsupportedSeekMode { mode } => PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            format!("Seek mode {mode:?} пока не поддерживается текущим demux contract"),
        ),
    }
}

/// Мапит failed decoder flush в typed player error без продолжения seek transaction.
fn player_error_from_decoder_flush_error(error: anyhow::Error) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::DecoderFlushFailed,
        format!("Video decoder flush failed before seek: {error}"),
    )
}

/// Возвращает stable label для типа seek transaction-а.
fn seek_commit_kind_name(kind: SeekCommitKind) -> &'static str {
    match kind {
        SeekCommitKind::Final => "final",
        SeekCommitKind::Preview => "preview",
    }
}

/// Возвращает stable label для resume intent-а после seek.
fn playback_resume_intent_name(intent: PlaybackResumeIntent) -> &'static str {
    match intent {
        PlaybackResumeIntent::Pause => "pause",
        PlaybackResumeIntent::Play => "play",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use codec_core::{ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction};

    use super::*;
    use crate::{
        DecodeBackpressureReason, DecodeSendError, DecodeThreadError,
        DecoderControlChannelPressureSnapshot, DecoderResourceSnapshot, MediaSource,
        PendingAudioPacket, PendingVideoPacket, PlayerCommand, PlayerDecodePacket,
        PlayerTickConfig, PlayerTickContext, ScrubCommitPolicy, SeekMode, SeekTarget,
        WgpuRenderTextureProviderHandle,
    };
    use bytes::Bytes;
    use capability_core::{
        BackendCapabilities, BackendDriverInfo, BackendProbeStatus, P010StorageLayout,
        VideoExportPath,
    };
    use codec_core::{
        BitDepth, ChromaSubsampling, DecodeBackendId, SupportedVideoDecodeFormat, VideoProfile,
        Vp9Profile,
    };
    use media_core::{DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer};
    use render_core::RenderCapabilities;

    /// Собирает абсолютный scrub request с явным seek mode для проверки boundary state.
    fn session_scrub_request(seconds: u64, mode: SeekMode) -> SeekRequest {
        SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_secs(seconds)),
            mode,
        }
    }

    /// Собирает tagged scrub update для unit-тестов session-side scrub state.
    fn session_scrub_intent(
        generation: ScrubGeneration,
        seconds: u64,
        mode: SeekMode,
    ) -> ScrubUpdateIntent {
        ScrubUpdateIntent::new(generation, session_scrub_request(seconds, mode))
    }

    /// Создаёт минимальный seek commit для чистых audio-gate тестов.
    fn audio_gate_seek_commit(
        kind: SeekCommitKind,
        resume_intent: PlaybackResumeIntent,
    ) -> SeekCommitState {
        SeekCommitState {
            generation: 4,
            scrub_generation: None,
            target_position: MediaTime::from_secs(8),
            actual_position: MediaTime::from_secs(8),
            started_at: Instant::now(),
            resume_intent,
            kind,
        }
    }

    #[test]
    fn session_scrub_end_closes_active_state_without_resetting_unrelated_seek_state() {
        let generation = ScrubGeneration::default().next();
        let direct_seed = generation.next();
        let mut session = PlayerSession::new();

        session.direct_scrub_generation_seed = direct_seed;
        session.seek_eof_fallback_video_position = Some(MediaTime::from_secs(29));
        session.seek_commit = Some(SeekCommitState {
            generation: 77,
            scrub_generation: Some(generation),
            target_position: MediaTime::from_secs(17),
            actual_position: MediaTime::from_secs(16),
            started_at: Instant::now(),
            resume_intent: PlaybackResumeIntent::Pause,
            kind: SeekCommitKind::Final,
        });
        session.scrub_state.begin(generation);
        let intent = session_scrub_intent(generation, 17, SeekMode::Accurate);
        assert_eq!(
            session.scrub_state.accept_update(intent),
            SessionScrubUpdateAcceptance::Accepted { intent }
        );

        assert_eq!(
            session.scrub_state.close_current(generation),
            SessionScrubFinishDecision::Current { generation }
        );

        assert_eq!(session.scrub_state.current_generation(), None);
        assert_eq!(session.scrub_state.latest_request_for(generation), None);
        assert_eq!(session.direct_scrub_generation_seed, direct_seed);
        assert_eq!(
            session.seek_eof_fallback_video_position,
            Some(MediaTime::from_secs(29))
        );
        assert!(session.seek_commit.is_some());
    }

    /// Fake demuxer для проверки player-core transaction без реального WebM/GPU.
    struct FakeDemuxer {
        tracks: Vec<TrackInfo>,
        duration: Option<Duration>,
        packets: VecDeque<media_core::Packet>,
        seek_log: Arc<Mutex<Vec<Duration>>>,
        seek_request_log: Option<Arc<Mutex<Vec<DemuxSeekRequest>>>>,
        seekability: DemuxSeekability,
    }

    impl FakeDemuxer {
        /// Создаёт fake demuxer с явными tracks и shared seek log.
        fn new(
            tracks: Vec<TrackInfo>,
            duration: Option<Duration>,
            seek_log: Arc<Mutex<Vec<Duration>>>,
        ) -> Self {
            Self {
                tracks,
                duration,
                packets: VecDeque::new(),
                seek_log,
                seek_request_log: None,
                seekability: DemuxSeekability::Seekable,
            }
        }

        /// Задаёт seekability для сценариев с playback-only source.
        fn with_seekability(mut self, seekability: DemuxSeekability) -> Self {
            self.seekability = seekability;
            self
        }

        /// Подключает отдельный log полных demux seek request-ов.
        fn with_seek_request_log(
            mut self,
            seek_request_log: Arc<Mutex<Vec<DemuxSeekRequest>>>,
        ) -> Self {
            self.seek_request_log = Some(seek_request_log);
            self
        }
    }

    impl Demuxer for FakeDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &self.tracks
        }

        fn duration(&self) -> Option<Duration> {
            self.duration
        }

        fn seekability(&self) -> DemuxSeekability {
            self.seekability
        }

        fn next_packet(&mut self) -> anyhow::Result<Option<media_core::Packet>> {
            Ok(self.packets.pop_front())
        }

        fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            self.seek_log
                .lock()
                .expect("seek log mutex should not be poisoned")
                .push(timestamp);
            Ok(DemuxSeekResult {
                requested_position: MediaTime::from_duration(timestamp),
                actual_position: MediaTime::from_duration(timestamp),
                actual_track_timestamp: None,
            })
        }

        fn seek_with_request(
            &mut self,
            request: DemuxSeekRequest,
        ) -> anyhow::Result<DemuxSeekResult> {
            if let Some(ref seek_request_log) = self.seek_request_log {
                seek_request_log
                    .lock()
                    .expect("seek request log mutex should not be poisoned")
                    .push(request);
            }

            self.seek(request.timestamp)
        }
    }

    /// Fake decoder thread, который нужен только для проверки failed flush boundary.
    struct FailingFlushVideoDecoderThread {
        /// Текст ошибки, возвращаемый из session-level decoder reset.
        error_message: &'static str,
    }

    impl FailingFlushVideoDecoderThread {
        /// Создаёт fake decoder thread с предсказуемой flush ошибкой.
        const fn new(error_message: &'static str) -> Self {
            Self { error_message }
        }
    }

    impl video_core::VideoDecoderThreadHandle for FailingFlushVideoDecoderThread {
        type TextureViewProvider = WgpuRenderTextureProviderHandle;

        fn backend_name(&self) -> &'static str {
            "Fake failing decoder"
        }

        fn send_packet(&self, _packet: PlayerDecodePacket) -> Result<(), DecodeSendError> {
            Err(DecodeSendError::Fatal(DecodeThreadError::new(
                "fake decoder cannot accept packets",
            )))
        }

        fn release_frame(&self, _handle: video_core::FrameTextureHandle) {}

        fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
            None
        }

        fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
            None
        }

        fn try_recv_error(&self) -> Option<DecodeThreadError> {
            None
        }

        fn flush(&self) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("{}", self.error_message))
        }

        fn texture_view_provider(&self) -> WgpuRenderTextureProviderHandle {
            panic!("fake failing decoder does not provide renderer texture views")
        }

        fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
            None
        }

        fn packet_queue_depth(&self) -> usize {
            0
        }

        fn drain_completed_packet_count(&self) -> usize {
            0
        }
    }

    /// Fake audio decoder считает reset-вызовы без CPAL и codec side effects.
    struct CountingAudioDecoder {
        /// Shared counter reset-вызовов.
        reset_count: Arc<AtomicUsize>,
    }

    impl CountingAudioDecoder {
        /// Создаёт fake decoder с внешним счётчиком.
        fn new(reset_count: Arc<AtomicUsize>) -> Self {
            Self { reset_count }
        }
    }

    impl audio::AudioDecoder for CountingAudioDecoder {
        /// Decode в этом fake path-е не используется.
        fn decode(&mut self, _packet: &audio::EncodedAudioPacket<'_>) -> anyhow::Result<Vec<f32>> {
            Ok(Vec::new())
        }

        /// Отмечает reset после seek.
        fn reset(&mut self) -> anyhow::Result<()> {
            self.reset_count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        /// Возвращает стабильный sample rate для boundary contract-а.
        fn sample_rate(&self) -> u32 {
            48_000
        }

        /// Возвращает стабильный channel count для boundary contract-а.
        fn channels(&self) -> u32 {
            2
        }
    }

    /// Shared fake decoder для seek/tick тестов без production decoder и renderer resources.
    #[derive(Clone)]
    struct SharedFakeVideoDecoderThread {
        /// Очередь decoded frames, которые fake decoder отдаёт через `try_recv_frame`.
        decoded_frames: Arc<Mutex<VecDeque<video_core::DecodedFrame>>>,

        /// Очередь результатов `send_packet` для проверки decoder boundary без реального backend-а.
        send_results: Arc<Mutex<VecDeque<Result<(), DecodeSendError>>>>,

        /// Накопленные packet ack-и, которые fake decoder отдаёт одним drain-вызовом.
        completed_packet_count: Arc<Mutex<usize>>,

        /// Diagnostics events, которые fake decoder отдаёт через boundary drain.
        diagnostic_events: Arc<Mutex<VecDeque<video_core::VideoDecoderDiagnosticEvent>>>,

        /// Опциональная flush ошибка для проверки проброса seek/reset failure.
        flush_error_message: Arc<Mutex<Option<String>>>,

        /// Texture handles, которые session вернула decoder boundary через release.
        released_handles: Arc<Mutex<Vec<video_core::FrameTextureHandle>>>,

        /// Control-channel pressure snapshot, который fake отдаёт через decoder boundary.
        control_pressure: Arc<Mutex<Option<DecoderControlChannelPressureSnapshot>>>,
    }

    impl SharedFakeVideoDecoderThread {
        /// Создаёт fake decoder с пустыми output/release очередями.
        fn new() -> Self {
            Self {
                decoded_frames: Arc::new(Mutex::new(VecDeque::new())),
                send_results: Arc::new(Mutex::new(VecDeque::new())),
                completed_packet_count: Arc::new(Mutex::new(0)),
                diagnostic_events: Arc::new(Mutex::new(VecDeque::new())),
                flush_error_message: Arc::new(Mutex::new(None)),
                released_handles: Arc::new(Mutex::new(Vec::new())),
                control_pressure: Arc::new(Mutex::new(None)),
            }
        }

        /// Публикует frame так, как production decoder thread сделал бы после seek decode.
        fn push_decoded_frame(&self, frame: video_core::DecodedFrame) {
            self.decoded_frames
                .lock()
                .expect("fake decoder frame queue lock")
                .push_back(frame);
        }

        /// Добавляет предсказуемый результат отправки packet-а через fake boundary.
        fn push_send_result(&self, result: Result<(), DecodeSendError>) {
            self.send_results
                .lock()
                .expect("fake decoder send result queue lock")
                .push_back(result);
        }

        /// Публикует packet ack-и так, как production decoder сделал бы после decode/flush drain.
        fn add_completed_packet_count(&self, packet_count: usize) {
            let mut completed_packet_count = self
                .completed_packet_count
                .lock()
                .expect("fake decoder ack counter lock");
            *completed_packet_count = completed_packet_count.saturating_add(packet_count);
        }

        /// Публикует diagnostics event так, как production decoder boundary сделал бы.
        fn push_diagnostic_event(&self, event: video_core::VideoDecoderDiagnosticEvent) {
            self.diagnostic_events
                .lock()
                .expect("fake decoder diagnostics queue lock")
                .push_back(event);
        }

        /// Настраивает следующую flush ошибку без изменения decoder lifecycle.
        fn fail_flush_with(&self, error_message: &str) {
            *self
                .flush_error_message
                .lock()
                .expect("fake decoder flush error lock") = Some(error_message.to_string());
        }

        /// Возвращает release log для проверки bounded queue и ownership.
        fn released_handles(&self) -> Vec<video_core::FrameTextureHandle> {
            self.released_handles
                .lock()
                .expect("fake decoder release log lock")
                .clone()
        }

        /// Настраивает control-channel pressure snapshot для проверки pipeline boundary.
        fn set_control_pressure(&self, pressure: DecoderControlChannelPressureSnapshot) {
            *self
                .control_pressure
                .lock()
                .expect("fake decoder control pressure lock") = Some(pressure);
        }
    }

    impl video_core::VideoDecoderThreadHandle for SharedFakeVideoDecoderThread {
        type TextureViewProvider = WgpuRenderTextureProviderHandle;

        fn backend_name(&self) -> &'static str {
            "Shared fake decoder"
        }

        fn send_packet(&self, _packet: PlayerDecodePacket) -> Result<(), DecodeSendError> {
            self.send_results
                .lock()
                .expect("fake decoder send result queue lock")
                .pop_front()
                .unwrap_or(Ok(()))
        }

        fn release_frame(&self, handle: video_core::FrameTextureHandle) {
            self.released_handles
                .lock()
                .expect("fake decoder release log lock")
                .push(handle);
        }

        fn try_recv_frame(&self) -> Option<video_core::DecodedFrame> {
            self.decoded_frames
                .lock()
                .expect("fake decoder frame queue lock")
                .pop_front()
        }

        fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
            self.diagnostic_events
                .lock()
                .expect("fake decoder diagnostics queue lock")
                .pop_front()
        }

        fn try_recv_error(&self) -> Option<DecodeThreadError> {
            None
        }

        fn flush(&self) -> anyhow::Result<()> {
            if let Some(error_message) = self
                .flush_error_message
                .lock()
                .expect("fake decoder flush error lock")
                .take()
            {
                return Err(anyhow::anyhow!(error_message));
            }

            Ok(())
        }

        fn texture_view_provider(&self) -> WgpuRenderTextureProviderHandle {
            panic!("shared fake decoder does not provide renderer texture views")
        }

        fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
            None
        }

        fn decoder_control_channel_pressure(
            &self,
        ) -> Option<DecoderControlChannelPressureSnapshot> {
            *self
                .control_pressure
                .lock()
                .expect("fake decoder control pressure lock")
        }

        fn packet_queue_depth(&self) -> usize {
            0
        }

        fn drain_completed_packet_count(&self) -> usize {
            let mut completed_packet_count = self
                .completed_packet_count
                .lock()
                .expect("fake decoder ack counter lock");
            let drained_packet_count = *completed_packet_count;
            *completed_packet_count = 0;
            drained_packet_count
        }
    }

    /// Создаёт минимальный track summary для fake media.
    fn fake_track(track_id: u32, kind: TrackKind) -> TrackInfo {
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

    /// Создаёт audio track с заданным container codec id для чистых codec selection tests.
    fn fake_audio_track_with_codec(track_id: u32, codec_id: &str) -> TrackInfo {
        let mut track = fake_track(track_id, TrackKind::Audio);
        track.codec_id = codec_id.to_string();
        track
    }

    #[test]
    fn audio_decoder_init_spec_maps_opus_track_without_cpal_device() {
        let tracks = vec![
            fake_track(1, TrackKind::Video),
            fake_audio_track_with_codec(2, "A_OPUS"),
        ];

        let init_spec = audio_decoder_init_spec_from_tracks(&tracks)
            .expect("Opus audio track should be accepted")
            .expect("audio init spec should exist");

        assert_eq!(
            init_spec,
            AudioDecoderInitSpec {
                track_id: TrackId::new(2),
                codec_id: "A_OPUS".to_string(),
                codec_private: None,
                initial_sample_rate: Some(48_000),
                initial_channels: Some(2),
            }
        );
    }

    #[test]
    fn audio_decoder_init_spec_preserves_symphonia_candidate_codec_ids_without_cpal_device() {
        let codec_ids = ["A_AAC/MPEG4/LC", "A_VORBIS", "A_FLAC"];

        for codec_id in codec_ids {
            let tracks = vec![fake_audio_track_with_codec(2, codec_id)];
            let init_spec = audio_decoder_init_spec_from_tracks(&tracks)
                .expect("audio init spec should not create decoder/output resources")
                .expect("audio init spec should exist");

            assert_eq!(init_spec.codec_id, codec_id);
            assert_eq!(init_spec.codec_private, None);
            assert_eq!(init_spec.track_id, TrackId::new(2));
            assert_eq!(init_spec.initial_sample_rate, Some(48_000));
            assert_eq!(init_spec.initial_channels, Some(2));
        }
    }

    #[test]
    fn audio_decoder_init_spec_accepts_audio_track_without_probe_parameters() {
        let mut incomplete_audio_track = fake_audio_track_with_codec(2, "A_OPUS");
        incomplete_audio_track.sample_rate = None;
        incomplete_audio_track.channels = None;

        let init_spec = audio_decoder_init_spec_from_tracks(&[
            fake_track(1, TrackKind::Video),
            incomplete_audio_track,
        ])
        .expect("missing audio parameters should not be a codec error");

        let init_spec = init_spec.expect("audio track should still be selected lazily");
        assert_eq!(init_spec.track_id, TrackId::new(2));
        assert_eq!(init_spec.initial_sample_rate, None);
        assert_eq!(init_spec.initial_channels, None);
    }

    #[test]
    fn audio_decoder_init_spec_returns_none_when_audio_track_is_absent() {
        let init_spec = audio_decoder_init_spec_from_tracks(&[fake_track(1, TrackKind::Video)])
            .expect("video-only media should not be a codec error");

        assert!(init_spec.is_none());
    }

    #[test]
    fn audio_decoder_init_spec_rejects_zero_probe_parameters() {
        let mut invalid_audio_track = fake_audio_track_with_codec(2, "A_OPUS");
        invalid_audio_track.sample_rate = Some(0);

        let error = audio_decoder_init_spec_from_tracks(&[invalid_audio_track])
            .expect_err("zero sample_rate should stay a planning error");

        assert_eq!(error.kind, PlayerErrorKind::RuntimeError);
    }

    #[test]
    fn audio_decoder_init_spec_preserves_unknown_codec_for_factory_typed_error() {
        let tracks = vec![fake_audio_track_with_codec(2, "A_NOT_REAL")];

        let init_spec = audio_decoder_init_spec_from_tracks(&tracks)
            .expect("unknown audio codec should be deferred to audio factory")
            .expect("audio init spec should exist");

        assert_eq!(init_spec.codec_id, "A_NOT_REAL");
    }

    #[test]
    fn init_audio_pipeline_defers_unsupported_factory_codec_until_first_packet() {
        let mut session = PlayerSession::new();
        let tracks = vec![fake_audio_track_with_codec(2, "A_NOT_REAL")];

        session.init_audio_pipeline(&tracks);

        assert!(!session.pipeline.has_audio_decoder());
        assert!(!session.pipeline.has_audio_output());
        assert!(session.pipeline.has_deferred_audio_decoder_config());
        assert_eq!(
            session.pipeline.selected_audio_track_id(),
            Some(TrackId::new(2))
        );
        assert!(session.take_events().is_empty());

        session.process_audio_packet(TrackId::new(2), Duration::ZERO, None, None, 0, b"encoded");

        assert!(!session.pipeline.has_audio_decoder());
        assert!(!session.pipeline.has_audio_output());
        assert!(!session.pipeline.has_deferred_audio_decoder_config());
        assert!(session.pipeline.selected_audio_track_id().is_none());
        assert!(session.take_events().iter().any(|event| matches!(
            event,
            PlayerEvent::RecoverableError(error)
                if error.kind == PlayerErrorKind::UnsupportedAudioCodec
        )));
    }

    #[test]
    fn demux_track_list_update_replans_audio_runtime_without_fatal_error() {
        let mut session = PlayerSession::default();
        install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);
        let reset_count = Arc::new(AtomicUsize::new(0));
        session
            .pipeline
            .install_audio_decoder(Box::new(CountingAudioDecoder::new(Arc::clone(
                &reset_count,
            ))));
        session.pipeline.install_deferred_audio_decoder_config(
            audio::AudioDecoderConfig::from_track_metadata(2, "A_OPUS", Some(48_000), Some(2)),
        );
        let initial_generation = session.pipeline.seek_generation();

        session.handle_demux_track_list_update(DemuxTrackListUpdate::new(
            vec![fake_audio_track_with_codec(3, "A_AAC")],
            Some(Duration::from_secs(42)),
        ));

        assert_eq!(session.playback_state(), PlaybackState::Paused);
        assert_eq!(session.snapshot.duration, Some(Duration::from_secs(42)));
        assert_eq!(session.pipeline.seek_generation(), initial_generation + 1);
        assert_eq!(
            session.pipeline.selected_audio_track_id(),
            Some(TrackId::new(3))
        );
        assert!(session.pipeline.has_deferred_audio_decoder_config());
        assert!(!session.pipeline.has_audio_decoder());
        assert!(!session.pipeline.has_audio_output());
        assert!(session.snapshot.last_error.is_none());
        assert_eq!(reset_count.load(Ordering::Relaxed), 0);
    }

    /// Подключает fake demuxer без инициализации audio/video backend ресурсов.
    fn install_fake_media(
        session: &mut PlayerSession,
        tracks: Vec<TrackInfo>,
    ) -> Arc<Mutex<Vec<Duration>>> {
        install_fake_media_with_seekability(session, tracks, DemuxSeekability::Seekable)
    }

    /// Подключает fake demuxer с заданной seekability.
    fn install_fake_media_with_seekability(
        session: &mut PlayerSession,
        tracks: Vec<TrackInfo>,
        seekability: DemuxSeekability,
    ) -> Arc<Mutex<Vec<Duration>>> {
        let seek_log = Arc::new(Mutex::new(Vec::new()));
        let demuxer = FakeDemuxer::new(
            tracks.clone(),
            Some(Duration::from_secs(30)),
            Arc::clone(&seek_log),
        )
        .with_seekability(seekability);

        session
            .pipeline
            .install_opened_media(Box::new(demuxer), None, None, tracks.clone());
        if let Some(video_track_id) = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .map(|track| track.id)
        {
            session
                .pipeline
                .select_video_track_preserving_active_requirement(video_track_id);
        }
        if let Some(audio_track_id) = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .map(|track| track.id)
        {
            session.pipeline.select_audio_track(audio_track_id);
        }
        session.set_snapshot_duration(Some(Duration::from_secs(30)));
        session.apply_demux_seekability(seekability);
        session.set_playback_state(PlaybackState::Paused);

        seek_log
    }

    /// Подключает fake demuxer и возвращает log полных demux seek request-ов.
    fn install_fake_media_with_seek_request_log(
        session: &mut PlayerSession,
        tracks: Vec<TrackInfo>,
    ) -> Arc<Mutex<Vec<DemuxSeekRequest>>> {
        let seek_log = Arc::new(Mutex::new(Vec::new()));
        let seek_request_log = Arc::new(Mutex::new(Vec::new()));
        let demuxer = FakeDemuxer::new(tracks.clone(), Some(Duration::from_secs(30)), seek_log)
            .with_seek_request_log(Arc::clone(&seek_request_log));

        session
            .pipeline
            .install_opened_media(Box::new(demuxer), None, None, tracks.clone());
        if let Some(video_track_id) = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .map(|track| track.id)
        {
            session
                .pipeline
                .select_video_track_preserving_active_requirement(video_track_id);
        }
        if let Some(audio_track_id) = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .map(|track| track.id)
        {
            session.pipeline.select_audio_track(audio_track_id);
        }
        session.set_snapshot_duration(Some(Duration::from_secs(30)));
        session.apply_demux_seekability(DemuxSeekability::Seekable);
        session.set_playback_state(PlaybackState::Paused);

        seek_request_log
    }

    #[test]
    fn prepared_media_install_publishes_open_snapshot_and_events() {
        let mut session = PlayerSession::new();
        let tracks = vec![fake_track(1, TrackKind::Video)];
        let seek_log = Arc::new(Mutex::new(Vec::new()));
        let demuxer =
            FakeDemuxer::new(tracks, Some(Duration::from_secs(30)), Arc::clone(&seek_log))
                .with_seekability(DemuxSeekability::NotSeekable {
                    reason: TimelineNotSeekableReason::SourceNotSeekable,
                });
        let media_path = std::path::PathBuf::from("/tmp/sample.webm");
        let prepared_media = PreparedMedia::from_local_file(media_path.clone(), Box::new(demuxer));

        session.load_prepared_media_with_autoplay(prepared_media, false);

        assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
        assert_eq!(
            session.snapshot().source_label.as_deref(),
            Some("/tmp/sample.webm")
        );
        assert_eq!(session.snapshot().media_title.as_deref(), Some("sample"));
        assert_eq!(session.snapshot().duration, Some(Duration::from_secs(30)));
        assert_eq!(
            session.snapshot().selected_tracks.video_track,
            Some(TrackId::new(1))
        );
        assert!(!session.snapshot().timeline.seekable);
        assert_eq!(
            session.snapshot().timeline.not_seekable_reason,
            Some(TimelineNotSeekableReason::SourceNotSeekable)
        );

        let events = session.take_events();
        assert!(events.iter().any(|event| {
            matches!(
                event,
                PlayerEvent::MediaOpenRequested(request)
                    if request.source == MediaSource::LocalFile(media_path.clone())
                        && !request.autoplay
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                PlayerEvent::MediaOpened(summary)
                    if summary.source_label == "/tmp/sample.webm"
                        && summary.title.as_deref() == Some("sample")
                        && summary.duration == Some(Duration::from_secs(30))
            )
        }));
    }

    #[test]
    fn audio_only_prepared_media_opens_without_missing_video_fatal() {
        let mut session = PlayerSession::new();
        let tracks = vec![fake_audio_track_with_codec(2, "A_FLAC")];
        let seek_log = Arc::new(Mutex::new(Vec::new()));
        let demuxer =
            FakeDemuxer::new(tracks, Some(Duration::from_secs(30)), Arc::clone(&seek_log));
        let media_path = std::path::PathBuf::from("/tmp/song.flac");
        let prepared_media = PreparedMedia::from_local_file(media_path.clone(), Box::new(demuxer));

        session.load_prepared_media_with_autoplay(prepared_media, false);

        let snapshot = session.snapshot_with_frame_counters(FrameCounters::default());
        assert_eq!(snapshot.playback_state, PlaybackState::Paused);
        assert_eq!(snapshot.source_label.as_deref(), Some("/tmp/song.flac"));
        assert_eq!(snapshot.media_title.as_deref(), Some("song.flac"));
        assert_eq!(snapshot.duration, Some(Duration::from_secs(30)));
        assert_eq!(snapshot.selected_tracks.video_track, None);
        assert_eq!(snapshot.current_video_frame, None);
        assert!(snapshot.last_error.is_none());
        assert!(session.pipeline.has_demuxer());

        let events = session.take_events();
        assert!(events.iter().any(|event| {
            matches!(
                event,
                PlayerEvent::MediaOpenRequested(request)
                    if request.source == MediaSource::LocalFile(media_path.clone())
                        && !request.autoplay
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                PlayerEvent::MediaOpened(summary)
                    if summary.source_label == "/tmp/song.flac"
                        && summary.title.as_deref() == Some("song")
                        && summary.duration == Some(Duration::from_secs(30))
            )
        }));
    }

    #[test]
    fn audio_only_play_and_tick_do_not_require_video_decoder() {
        let mut session = PlayerSession::new();
        let tracks = vec![fake_track(2, TrackKind::Audio)];
        let seek_log = Arc::new(Mutex::new(Vec::new()));
        let mut demuxer = FakeDemuxer::new(
            tracks.clone(),
            Some(Duration::from_secs(30)),
            Arc::clone(&seek_log),
        );
        let audio_time_base = media_core::TimeBase::new(1, 48_000).expect("valid audio time base");
        demuxer.packets.push_back(
            media_core::Packet::new(
                TrackId::new(2),
                TrackKind::Audio,
                Duration::from_millis(10),
                None,
                false,
                Bytes::from_static(b"audio-packet"),
            )
            .with_track_timestamps(
                Some(media_core::TrackTimestamp::new(
                    TrackId::new(2),
                    480,
                    audio_time_base,
                )),
                Some(media_core::TrackTimestamp::new(
                    TrackId::new(2),
                    480,
                    audio_time_base,
                )),
            ),
        );
        session
            .pipeline
            .install_opened_media(Box::new(demuxer), None, None, tracks);
        session.pipeline.select_audio_track(TrackId::new(2));
        session.set_snapshot_duration(Some(Duration::from_secs(30)));
        session.set_playback_state(PlaybackState::Paused);

        session.dispatch_command(PlayerCommand::Play).unwrap();
        assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);

        let tick_result = session.tick(PlayerTickContext::with_config(
            Instant::now(),
            PlayerTickConfig {
                max_demux_packets_per_tick: 1,
                ..PlayerTickConfig::default()
            },
        ));

        assert!(!session.pipeline.has_active_video_decoder());
        assert!(matches!(
            session.snapshot().playback_state,
            PlaybackState::Playing | PlaybackState::Draining
        ));
        assert_eq!(tick_result.demuxed_packets.len(), 1);
        assert_eq!(tick_result.video_frames_presented, 0);
        assert!(tick_result.dropped_video_frames.is_empty());
        assert!(session.snapshot().last_error.is_none());
        assert_eq!(
            tick_result.demuxed_packets[0]
                .track_pts
                .expect("tick telemetry должен сохранить raw PTS")
                .units
                .get(),
            480
        );
        assert_eq!(
            tick_result.demuxed_packets[0]
                .track_dts
                .expect("tick telemetry должен сохранить raw DTS")
                .units
                .get(),
            480
        );
    }

    /// Создаёт decoded frame без реальных GPU resources.
    fn decoded_frame_for_tests(pts: Duration, handle: u64) -> video_core::DecodedFrame {
        video_core::DecodedFrame {
            pts,
            format: video_core::DecodedPixelFormat::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            memory_path: video_core::FrameMemoryPath::DmaBufZeroCopy,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            color: codec_core::VideoColorMetadata::sdr_bt709_limited(),
            texture_handle: video_core::FrameTextureHandle(handle),
            diagnostics: video_core::VideoFrameDiagnostics::default(),
        }
    }

    /// Создаёт минимальный decode packet для проверки worker -> decoder boundary.
    fn decode_packet_for_tests(pts: Duration) -> PlayerDecodePacket {
        PlayerDecodePacket {
            track_id: TrackId::new(1),
            pts,
            encoded_bytes: Bytes::from_static(b"vp9-test-packet"),
            keyframe: true,
            resolved_color: None,
        }
    }

    /// Создаёт tick config с маленькой present queue для seek admission тестов.
    fn seek_admission_tick_config(
        max_video_present_queue: usize,
        max_decoded_video_frames_drained_per_tick: usize,
    ) -> PlayerTickConfig {
        PlayerTickConfig {
            max_video_present_queue,
            min_video_present_queue: 1,
            target_video_present_queue: 1,
            max_decoded_video_frames_drained_per_tick,
            seek_resume_video_min_ready_frames: 1,
            ..PlayerTickConfig::default()
        }
    }

    fn capabilities_with_vp9_profile0() -> SystemCapabilities {
        let backend_id = test_decode_backend_id();

        SystemCapabilities {
            schema_version: capability_core::CURRENT_CAPABILITY_SCHEMA_VERSION,
            probed_at_unix_seconds: 1,
            video_backends: vec![BackendCapabilities {
                backend_id: backend_id.clone(),
                display_name: "Test hardware decoder".to_string(),
                status: BackendProbeStatus::Available,
                driver: BackendDriverInfo::default(),
                supported_video_decode_formats: vec![SupportedVideoDecodeFormat {
                    codec: VideoCodec::Vp9,
                    profile: VideoProfile::Vp9(Vp9Profile::Profile0),
                    bit_depth: BitDepth::Eight,
                    chroma: ChromaSubsampling::Yuv420,
                    max_width: Some(1920),
                    max_height: Some(1080),
                    max_fps: None,
                    hdr_input: false,
                    backend: backend_id,
                }],
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                export_paths: vec![VideoExportPath::DmaBuf],
                p010_storage_layouts: Vec::new(),
                diagnostics: Vec::new(),
            }],
            render_backends: vec![RenderCapabilities::wgpu_nv12(Some(4096))],
        }
    }

    fn capabilities_with_phase10_vp9_profile2_hdr() -> SystemCapabilities {
        let backend_id = test_decode_backend_id();

        SystemCapabilities {
            schema_version: capability_core::CURRENT_CAPABILITY_SCHEMA_VERSION,
            probed_at_unix_seconds: 1,
            video_backends: vec![BackendCapabilities {
                backend_id: backend_id.clone(),
                display_name: "Test hardware decoder".to_string(),
                status: BackendProbeStatus::Available,
                driver: BackendDriverInfo::default(),
                supported_video_decode_formats: vec![SupportedVideoDecodeFormat {
                    codec: VideoCodec::Vp9,
                    profile: VideoProfile::Vp9(Vp9Profile::Profile2),
                    bit_depth: BitDepth::Ten,
                    chroma: ChromaSubsampling::Yuv420,
                    max_width: Some(4096),
                    max_height: Some(4096),
                    max_fps: None,
                    hdr_input: true,
                    backend: backend_id,
                }],
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                export_paths: vec![VideoExportPath::DmaBuf],
                p010_storage_layouts: vec![P010StorageLayout::BaselineSeparateLayer],
                diagnostics: Vec::new(),
            }],
            render_backends: vec![RenderCapabilities::wgpu_p010_bt2446c(Some(4096))],
        }
    }

    fn test_decode_backend_id() -> DecodeBackendId {
        DecodeBackendId::new("test_hardware_decoder")
            .expect("test backend id must use codec-core backend id syntax")
    }

    fn bt2020_pq_limited() -> codec_core::VideoColorMetadata {
        codec_core::VideoColorMetadata::container(
            ColorRange::Limited,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            TransferFunction::Pq,
            None,
        )
    }

    #[test]
    fn playback_pipeline_decoder_boundary_absent_thread_is_noop() {
        let pipeline = PlaybackPipeline::default();

        assert!(!pipeline.has_active_video_decoder());
        assert_eq!(pipeline.video_backend_name(), "Synthetic (test)");
        assert!(pipeline.flush_video_decoder_thread().is_ok());
        assert!(pipeline.try_recv_decoded_video_frame().is_none());
        assert!(pipeline.try_recv_video_decoder_diagnostic_event().is_none());
        assert!(pipeline.try_recv_video_decoder_error().is_none());
        assert_eq!(pipeline.drain_completed_video_decode_packet_count(), 0);
        assert!(pipeline.video_decoder_control_channel_pressure().is_none());
        assert_eq!(pipeline.video_decode_in_flight_packets(), 0);
        assert!(!pipeline.can_send_video_decode_packets());
        assert!(!pipeline.can_receive_decoded_video_frames());
        assert!(
            pipeline
                .send_video_decode_packet(decode_packet_for_tests(Duration::from_millis(10)))
                .is_none()
        );
        assert!(!pipeline.release_frame_to_video_decoder(video_core::FrameTextureHandle(7)));
    }

    #[test]
    fn playback_pipeline_decoder_boundary_forwards_send_results() {
        let mut pipeline = PlaybackPipeline::default();
        let fake_decoder = SharedFakeVideoDecoderThread::new();
        let backpressure_reason = DecodeBackpressureReason::PacketQueueFull {
            queued_packets: 4,
            capacity: 4,
        };
        fake_decoder.push_send_result(Ok(()));
        fake_decoder.push_send_result(Err(DecodeSendError::Backpressure(backpressure_reason)));
        fake_decoder.push_send_result(Err(DecodeSendError::Fatal(DecodeThreadError::new(
            "fatal send failure",
        ))));
        pipeline.set_video_decoder_thread(fake_decoder);

        assert!(pipeline.has_active_video_decoder());
        assert_eq!(pipeline.video_backend_name(), "Shared fake decoder");
        assert!(pipeline.can_send_video_decode_packets());
        assert!(matches!(
            pipeline.send_video_decode_packet(decode_packet_for_tests(Duration::from_millis(1))),
            Some(Ok(()))
        ));

        match pipeline.send_video_decode_packet(decode_packet_for_tests(Duration::from_millis(2))) {
            Some(Err(DecodeSendError::Backpressure(reason))) => {
                assert_eq!(reason, backpressure_reason);
            }
            unexpected_result => {
                panic!("expected decoder backpressure, got {unexpected_result:?}");
            }
        }

        match pipeline.send_video_decode_packet(decode_packet_for_tests(Duration::from_millis(3))) {
            Some(Err(DecodeSendError::Fatal(error))) => {
                assert_eq!(error.message(), "fatal send failure");
            }
            unexpected_result => {
                panic!("expected fatal decoder send error, got {unexpected_result:?}");
            }
        }
    }

    #[test]
    fn playback_pipeline_decoder_boundary_drains_acks_without_touching_in_flight() {
        let mut pipeline = PlaybackPipeline::default();
        let fake_decoder = SharedFakeVideoDecoderThread::new();
        fake_decoder.add_completed_packet_count(2);
        pipeline.set_video_decoder_thread(fake_decoder);
        pipeline.note_video_packet_sent_to_decoder();
        pipeline.note_video_packet_sent_to_decoder();
        pipeline.note_video_packet_sent_to_decoder();

        assert_eq!(pipeline.video_decode_in_flight_packets(), 3);

        let completed_packet_count = pipeline.drain_completed_video_decode_packet_count();

        assert_eq!(completed_packet_count, 2);
        assert_eq!(pipeline.video_decode_in_flight_packets(), 3);
        assert_eq!(pipeline.drain_completed_video_decode_packet_count(), 0);

        pipeline.note_video_packets_completed_by_decoder(completed_packet_count);

        assert_eq!(pipeline.video_decode_in_flight_packets(), 1);
    }

    #[test]
    fn playback_pipeline_decoder_boundary_forwards_diagnostic_events() {
        let mut pipeline = PlaybackPipeline::default();
        let fake_decoder = SharedFakeVideoDecoderThread::new();
        let pressure = video_core::VideoFramePublishPressureDiagnostics {
            frame_publish_channel_full_count: 2,
            pending_publish_retry_count: 1,
            max_decoded_frame_publish_latency: Duration::from_millis(8),
            total_decoded_frame_publish_latency: Duration::from_millis(8),
        };
        fake_decoder.push_diagnostic_event(
            video_core::VideoDecoderDiagnosticEvent::DecodedFramePublishPressure { pressure },
        );
        pipeline.set_video_decoder_thread(fake_decoder);

        let event = pipeline
            .try_recv_video_decoder_diagnostic_event()
            .expect("fake decoder diagnostic event should cross pipeline boundary");

        assert_eq!(
            event,
            video_core::VideoDecoderDiagnosticEvent::DecodedFramePublishPressure { pressure }
        );
        assert!(pipeline.try_recv_video_decoder_diagnostic_event().is_none());
    }

    #[test]
    fn playback_pipeline_decoder_boundary_forwards_control_pressure_snapshot() {
        let mut pipeline = PlaybackPipeline::default();
        let fake_decoder = SharedFakeVideoDecoderThread::new();
        let pressure = DecoderControlChannelPressureSnapshot {
            control_channel_len: 31,
            control_channel_capacity: 32,
            control_channel_full_count: 2,
            release_control_send_fail_count: 1,
            flush_control_send_fail_count: 1,
        };
        fake_decoder.set_control_pressure(pressure);
        pipeline.set_video_decoder_thread(fake_decoder);

        assert_eq!(
            pipeline.video_decoder_control_channel_pressure(),
            Some(pressure)
        );
    }

    #[test]
    fn playback_pipeline_decoder_boundary_propagates_flush_error() {
        let mut pipeline = PlaybackPipeline::default();
        let fake_decoder = SharedFakeVideoDecoderThread::new();
        fake_decoder.fail_flush_with("flush failed at decoder boundary");
        pipeline.set_video_decoder_thread(fake_decoder);

        let flush_error = pipeline
            .flush_video_decoder_thread()
            .expect_err("fake decoder flush should fail");

        assert_eq!(flush_error.to_string(), "flush failed at decoder boundary");
    }

    #[test]
    fn playback_pipeline_decoder_boundary_releases_active_fake_frame() {
        let mut pipeline = PlaybackPipeline::default();
        let fake_decoder = SharedFakeVideoDecoderThread::new();
        let released_decoder = fake_decoder.clone();
        let texture_handle = video_core::FrameTextureHandle(42);
        pipeline.set_video_decoder_thread(fake_decoder);

        assert!(pipeline.can_receive_decoded_video_frames());
        assert!(pipeline.release_frame_to_video_decoder(texture_handle));
        assert_eq!(released_decoder.released_handles(), vec![texture_handle]);
    }

    #[test]
    fn snapshot_backend_name_uses_pipeline_decoder_boundary() {
        let mut session = PlayerSession::new();

        session
            .pipeline
            .set_video_decoder_thread(SharedFakeVideoDecoderThread::new());

        let snapshot = session.snapshot_with_frame_counters(FrameCounters::default());

        assert_eq!(
            snapshot.active_backend.name.as_deref(),
            Some("Shared fake decoder")
        );
    }

    #[test]
    fn default_session_starts_idle() {
        let session = PlayerSession::new();

        assert_eq!(session.snapshot().playback_state, PlaybackState::Idle);
        assert_eq!(session.snapshot().current_position, Duration::ZERO);
        assert_eq!(
            session.snapshot().timeline.current_position,
            MediaTime::ZERO
        );
    }

    #[test]
    fn seek_command_sets_target_and_commits_position_after_gates() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, Vec::new());
        let request = SeekRequest::absolute(MediaTime::from_millis(1_500));

        session
            .dispatch_command(PlayerCommand::Seek(request))
            .unwrap();

        assert_eq!(session.snapshot().current_position, Duration::ZERO);
        assert_eq!(
            session.snapshot().timeline.target_position,
            Some(MediaTime::from_millis(1_500))
        );
        assert!(session.snapshot().timeline.seeking);

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(
            session.snapshot().current_position,
            Duration::from_millis(1_500)
        );
        assert_eq!(
            session.snapshot().timeline.current_position,
            MediaTime::from_millis(1_500)
        );
        assert!(session.take_events().iter().any(
            |event| matches!(event, PlayerEvent::SeekRequested(accepted) if *accepted == request)
        ));
    }

    #[test]
    fn relative_seek_target_resolves_from_current_timeline_position() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, Vec::new());
        session.update_current_position(Duration::from_secs(10));
        let request = SeekRequest {
            target: SeekTarget::Relative(Duration::from_secs(5)),
            mode: crate::SeekMode::Accurate,
        };

        session
            .dispatch_command(PlayerCommand::Seek(request))
            .unwrap();
        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(session.snapshot().current_position, Duration::from_secs(15));
    }

    #[test]
    fn direct_dispatch_scrub_commands_remain_session_compatibility_path() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, Vec::new());
        let request = SeekRequest::absolute(MediaTime::from_secs(3));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        assert_eq!(
            session.scrub_state.current_generation(),
            Some(ScrubGeneration::default().next())
        );

        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        assert!(session.snapshot().timeline.scrubbing);
        assert_eq!(
            session.snapshot().timeline.target_position,
            Some(MediaTime::from_secs(3))
        );

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            })
            .unwrap();
        assert!(!session.snapshot().timeline.scrubbing);
    }

    #[test]
    fn default_timeline_release_policy_remains_commit_visible_preview() {
        assert_eq!(
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            ScrubCommitPolicy::CommitVisiblePreview
        );
    }

    #[test]
    fn default_release_after_update_commits_latest_target_without_visible_preview() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, Vec::new());
        let request = SeekRequest::absolute(MediaTime::from_secs(7));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();

        assert!(session.snapshot().timeline.scrubbing);
        assert!(session.snapshot().timeline.stale_frame);
        assert_eq!(
            session.snapshot().timeline.target_position,
            Some(MediaTime::from_secs(7))
        );

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            })
            .unwrap();

        assert!(!session.snapshot().timeline.scrubbing);
        assert_eq!(session.snapshot().current_position, Duration::ZERO);

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(session.snapshot().current_position, Duration::from_secs(7));
    }

    /// Фиксирует быстрый release сразу после UpdateScrub: без visible preview коммитится latest target.
    #[test]
    fn release_immediately_after_update_without_preview_frame_starts_final_seek() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let request = SeekRequest::absolute(MediaTime::from_secs(12));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            })
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].timestamp, Duration::from_secs(12));
        assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| (seek_commit.kind, seek_commit.target_position)),
            Some((SeekCommitKind::Final, MediaTime::from_secs(12)))
        );
        assert!(session.pipeline.present_video_frame().is_none());
        assert_eq!(session.snapshot().current_position, Duration::ZERO);
        assert!(!session.snapshot().timeline.scrubbing);
        assert!(session.snapshot().timeline.seeking);
        assert!(!session.snapshot().timeline.stale_frame);
        assert_eq!(
            session.snapshot().timeline.preview_state,
            TimelinePreviewState::Inactive
        );
    }

    #[test]
    fn preview_scrub_seek_keeps_scrub_active_without_committing_position() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media(&mut session, Vec::new());
        let request = SeekRequest::absolute(MediaTime::from_secs(7));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();

        assert_eq!(
            seek_log.lock().expect("seek log lock").as_slice(),
            &[Duration::from_secs(7)]
        );
        assert_eq!(
            session.seek_commit().map(|seek_commit| seek_commit.kind),
            Some(SeekCommitKind::Preview)
        );

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert!(session.snapshot().timeline.scrubbing);
        assert_eq!(
            session.snapshot().timeline.target_position,
            Some(MediaTime::from_secs(7))
        );
        assert_eq!(session.snapshot().current_position, Duration::ZERO);
        assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    }

    #[test]
    fn preview_timeout_without_target_frame_marks_preview_expired() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(1), 40));
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();
        let timeout_now = session
            .seek_commit()
            .expect("preview seek должен быть активен")
            .started_at
            + Duration::from_millis(101);

        session.finish_seek_commit_if_ready(
            timeout_now,
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert!(session.seek_commit().is_none());
        assert!(session.snapshot().timeline.scrubbing);
        assert!(!session.snapshot().timeline.seeking);
        assert!(session.snapshot().timeline.stale_frame);
        assert_eq!(
            session.snapshot().timeline.preview_state,
            TimelinePreviewState::Expired
        );
        assert_eq!(
            session.snapshot().timeline.target_position,
            Some(MediaTime::from_secs(8))
        );
        assert_eq!(session.snapshot().current_position, Duration::ZERO);
    }

    #[test]
    fn preview_scrub_seek_passes_target_to_demuxer() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();

        assert_eq!(
            seek_log.lock().expect("seek log lock").as_slice(),
            &[Duration::from_secs(8)]
        );
        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| seek_commit.target_position),
            Some(MediaTime::from_secs(8))
        );
        assert_eq!(
            session.seek_commit().map(|seek_commit| seek_commit.kind),
            Some(SeekCommitKind::Preview)
        );
    }

    #[test]
    fn scrub_preview_transaction_records_user_generation() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        let generation = session
            .scrub_state
            .current_generation()
            .expect("begin scrub должен выдать active generation");
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();

        assert_eq!(
            session
                .seek_commit()
                .and_then(|seek_commit| seek_commit.scrub_generation),
            Some(generation)
        );
    }

    #[test]
    fn preview_seek_keeps_pre_target_frames_for_live_feedback() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();

        assert!(!session.should_drop_decoded_frame_for_seek(Duration::from_millis(7_900)));

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            })
            .unwrap();

        assert!(session.should_drop_decoded_frame_for_seek(Duration::from_millis(7_900)));
    }

    #[test]
    fn preview_seek_completes_after_visible_pre_target_frame() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(7_900), 42));
        session.note_presented_frame_for_seek(Duration::from_millis(7_900));

        let tick_now = session
            .seek_commit()
            .expect("preview seek должен быть активен")
            .started_at
            + Duration::from_millis(1);
        session.finish_seek_commit_if_ready(
            tick_now,
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert!(session.seek_commit().is_none());
        assert!(session.snapshot().timeline.scrubbing);
        assert!(!session.snapshot().timeline.seeking);
        assert!(!session.snapshot().timeline.stale_frame);
        assert_eq!(
            session.snapshot().timeline.preview_state,
            TimelinePreviewState::Visible
        );
        assert_eq!(
            session.snapshot().timeline.target_position,
            Some(MediaTime::from_secs(8))
        );
    }

    #[test]
    fn default_release_while_preview_active_promotes_preview_without_second_seek() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(7_900), 41));

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            })
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].mode, DemuxSeekMode::Preview);
        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| (seek_commit.kind, seek_commit.target_position)),
            Some((SeekCommitKind::Final, MediaTime::from_secs(8)))
        );
        assert!(!session.snapshot().timeline.scrubbing);
        assert!(session.pipeline.video_present_queue_is_empty());
    }

    #[test]
    fn hybrid_release_finishes_when_latest_target_frame_is_presented() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let fake_decoder = SharedFakeVideoDecoderThread::new();
        session
            .pipeline
            .set_video_decoder_thread(fake_decoder.clone());
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(7_900), 42));
        session.note_presented_frame_for_seek(Duration::from_millis(7_900));

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatestTargetWithVisiblePreviewFallback,
            })
            .unwrap();
        fake_decoder.push_decoded_frame(decoded_frame_for_tests(Duration::from_secs(8), 43));

        let tick_result = session.tick(PlayerTickContext::with_config(
            Instant::now(),
            seek_admission_tick_config(2, 4),
        ));

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].mode, DemuxSeekMode::Preview);
        assert_eq!(tick_result.decoded_video_frames, 1);
        assert_eq!(tick_result.video_frames_presented, 1);
        assert!(session.seek_commit().is_none());
        assert_eq!(session.snapshot().current_position, Duration::from_secs(8));
        assert!(!session.snapshot().timeline.seeking);
        assert!(!session.snapshot().timeline.stale_frame);
        assert_eq!(
            session
                .pipeline
                .present_video_frame()
                .map(|frame| frame.pts),
            Some(Duration::from_secs(8))
        );
    }

    #[test]
    fn default_release_commits_visible_preview_without_exact_target() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(7_900), 42));
        session.note_presented_frame_for_seek(Duration::from_millis(7_900));

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            })
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].timestamp, Duration::from_secs(8));
        assert_eq!(requests[0].mode, DemuxSeekMode::Preview);
        assert!(session.seek_commit().is_none());
        assert_eq!(
            session.snapshot().current_position,
            Duration::from_millis(7_900)
        );
        assert_eq!(
            session.pipeline.media_clock_base(),
            Duration::from_millis(7_900)
        );
        assert!(!session.snapshot().timeline.scrubbing);
        assert!(!session.snapshot().timeline.seeking);
        assert!(!session.should_drop_decoded_frame_for_seek(Duration::from_millis(7_916)));
    }

    #[test]
    fn commit_visible_preview_policy_commits_visible_frame_without_exact_target() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(7_900), 42));
        session.note_presented_frame_for_seek(Duration::from_millis(7_900));

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitVisiblePreview,
            })
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].timestamp, Duration::from_secs(8));
        assert_eq!(requests[0].mode, DemuxSeekMode::Preview);
        assert!(session.seek_commit().is_none());
        assert_eq!(
            session.snapshot().current_position,
            Duration::from_millis(7_900)
        );
        assert_eq!(
            session.pipeline.media_clock_base(),
            Duration::from_millis(7_900)
        );
        assert!(!session.snapshot().timeline.scrubbing);
        assert!(!session.snapshot().timeline.seeking);
        assert!(!session.should_drop_decoded_frame_for_seek(Duration::from_millis(7_916)));
    }

    #[test]
    fn end_scrub_hybrid_marks_visible_preview_stale_until_latest_target_is_presented() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(7_900), 42));
        session.note_presented_frame_for_seek(Duration::from_millis(7_900));

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatestTargetWithVisiblePreviewFallback,
            })
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].mode, DemuxSeekMode::Preview);
        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| (seek_commit.kind, seek_commit.target_position)),
            Some((SeekCommitKind::Final, MediaTime::from_secs(8)))
        );
        assert_eq!(session.snapshot().current_position, Duration::ZERO);
        assert!(!session.snapshot().timeline.scrubbing);
        assert!(session.snapshot().timeline.seeking);
        assert!(session.snapshot().timeline.stale_frame);
        assert!(session.should_drop_decoded_frame_for_seek(Duration::from_millis(7_916)));
    }

    #[test]
    fn default_release_keeps_last_visible_preview_when_latest_target_changed() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let visible_request = SeekRequest::absolute(MediaTime::from_secs(8));
        let unpresented_request = SeekRequest::absolute(MediaTime::from_secs(9));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(visible_request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(visible_request))
            .unwrap();
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(7_900), 42));
        session.note_presented_frame_for_seek(Duration::from_millis(7_900));

        session
            .dispatch_command(PlayerCommand::UpdateScrub(unpresented_request))
            .unwrap();
        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| seek_commit.target_position),
            Some(MediaTime::from_secs(8))
        );
        assert_eq!(
            session.snapshot().timeline.target_position,
            Some(MediaTime::from_secs(9))
        );

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            })
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].timestamp, Duration::from_secs(8));
        assert_eq!(requests[0].mode, DemuxSeekMode::Preview);
        assert!(session.seek_commit().is_none());
        assert_eq!(
            session.snapshot().current_position,
            Duration::from_millis(7_900)
        );
        assert!(!session.snapshot().timeline.scrubbing);
        assert!(!session.snapshot().timeline.seeking);
    }

    #[test]
    fn commit_visible_preview_policy_keeps_last_visible_preview_when_latest_target_changed() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let visible_request = SeekRequest::absolute(MediaTime::from_secs(8));
        let unpresented_request = SeekRequest::absolute(MediaTime::from_secs(9));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(visible_request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(visible_request))
            .unwrap();
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(7_900), 42));
        session.note_presented_frame_for_seek(Duration::from_millis(7_900));

        session
            .dispatch_command(PlayerCommand::UpdateScrub(unpresented_request))
            .unwrap();
        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| seek_commit.target_position),
            Some(MediaTime::from_secs(8))
        );
        assert_eq!(
            session.snapshot().timeline.target_position,
            Some(MediaTime::from_secs(9))
        );

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitVisiblePreview,
            })
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert!(session.seek_commit().is_none());
        assert_eq!(
            session.snapshot().current_position,
            Duration::from_millis(7_900)
        );
        assert!(!session.snapshot().timeline.scrubbing);
        assert!(!session.snapshot().timeline.seeking);
    }

    /// Фиксирует, что visible preview старого scrub generation не может закрыть новый scrub.
    #[test]
    fn stale_visible_preview_from_old_generation_does_not_affect_new_scrub() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let first_generation = ScrubGeneration::default().next();
        let second_generation = first_generation.next();
        let stale_request = SeekRequest::absolute(MediaTime::from_secs(8));
        let fresh_request = SeekRequest::absolute(MediaTime::from_secs(9));

        session
            .dispatch_scrub_command(SessionScrubCommand::Begin {
                generation: first_generation,
            })
            .unwrap();
        session
            .dispatch_scrub_command(SessionScrubCommand::Update(ScrubUpdateIntent::new(
                first_generation,
                stale_request,
            )))
            .unwrap();
        session
            .dispatch_scrub_command(SessionScrubCommand::Preview(ScrubUpdateIntent::new(
                first_generation,
                stale_request,
            )))
            .unwrap();
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(7_900), 42));
        session.note_presented_frame_for_seek(Duration::from_millis(7_900));

        session
            .dispatch_scrub_command(SessionScrubCommand::Begin {
                generation: second_generation,
            })
            .unwrap();
        session
            .dispatch_scrub_command(SessionScrubCommand::Update(ScrubUpdateIntent::new(
                second_generation,
                fresh_request,
            )))
            .unwrap();
        session
            .dispatch_scrub_command(SessionScrubCommand::Preview(ScrubUpdateIntent::new(
                first_generation,
                stale_request,
            )))
            .unwrap();
        session
            .dispatch_scrub_command(SessionScrubCommand::End(ScrubCommitIntent::new(
                second_generation,
                ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            )))
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0],
            DemuxSeekRequest::preview(Duration::from_secs(8))
        );
        assert_eq!(requests[1].timestamp, Duration::from_secs(9));
        assert_eq!(requests[1].mode, DemuxSeekMode::DecodePointBefore);
        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| (seek_commit.kind, seek_commit.target_position)),
            Some((SeekCommitKind::Final, MediaTime::from_secs(9)))
        );
        assert_eq!(session.snapshot().current_position, Duration::ZERO);
        assert!(!session.snapshot().timeline.scrubbing);
        assert!(session.snapshot().timeline.seeking);
        assert!(session.snapshot().timeline.stale_frame);
    }

    #[test]
    fn end_scrub_hybrid_keeps_visible_preview_stale_and_commits_latest_target() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let visible_request = SeekRequest::absolute(MediaTime::from_secs(8));
        let latest_request = SeekRequest::absolute(MediaTime::from_secs(9));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(visible_request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(visible_request))
            .unwrap();
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(7_900), 42));
        session.note_presented_frame_for_seek(Duration::from_millis(7_900));

        session
            .dispatch_command(PlayerCommand::UpdateScrub(latest_request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::CommitLatestTargetWithVisiblePreviewFallback,
            })
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].mode, DemuxSeekMode::Preview);
        assert_eq!(requests[1].timestamp, Duration::from_secs(9));
        assert_eq!(requests[1].mode, DemuxSeekMode::DecodePointBefore);
        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| (seek_commit.kind, seek_commit.target_position)),
            Some((SeekCommitKind::Final, MediaTime::from_secs(9)))
        );
        assert_eq!(session.snapshot().current_position, Duration::ZERO);
        assert!(!session.snapshot().timeline.scrubbing);
        assert!(session.snapshot().timeline.seeking);
        assert!(session.snapshot().timeline.stale_frame);
        assert_eq!(
            session
                .pipeline
                .present_video_frame()
                .map(|frame| frame.pts),
            Some(Duration::from_millis(7_900))
        );
    }

    #[test]
    fn end_scrub_commits_ready_preview_without_second_demux_seek() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );
        let request = SeekRequest::absolute(MediaTime::from_secs(8));

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(request))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::PreviewScrub(request))
            .unwrap();
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(8), 42));
        session.note_presented_frame_for_seek(Duration::from_secs(8));
        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );
        assert_eq!(
            session.snapshot().timeline.preview_state,
            TimelinePreviewState::Ready
        );

        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            })
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(session.snapshot().current_position, Duration::from_secs(8));
        assert!(!session.snapshot().timeline.scrubbing);
        assert!(!session.snapshot().timeline.seeking);
        assert!(session.seek_commit().is_none());
    }

    #[test]
    fn keyframe_before_seek_keeps_demuxer_target_on_requested_position() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest {
                target: SeekTarget::Absolute(MediaTime::from_secs(8)),
                mode: SeekMode::KeyframeBefore,
            }))
            .unwrap();

        assert_eq!(
            seek_log.lock().expect("seek log lock").as_slice(),
            &[Duration::from_secs(8)]
        );
        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| seek_commit.target_position),
            Some(MediaTime::from_secs(8))
        );
    }

    #[test]
    fn accurate_video_seek_passes_requested_target_to_demuxer() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(8),
            )))
            .unwrap();

        assert_eq!(
            seek_log.lock().expect("seek log lock").as_slice(),
            &[Duration::from_secs(8)]
        );
        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| seek_commit.target_position),
            Some(MediaTime::from_secs(8))
        );
    }

    #[test]
    fn seek_transaction_passes_demux_request_without_runtime_index_hint() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(8),
            )))
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].timestamp, Duration::from_secs(8));
        assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
    }

    #[test]
    fn audio_only_accurate_final_seek_uses_demux_accurate_mode() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(2, TrackKind::Audio)],
        );

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(8),
            )))
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].timestamp, Duration::from_secs(8));
        assert_eq!(requests[0].mode, DemuxSeekMode::Accurate);
    }

    #[test]
    fn keyframe_before_video_seek_uses_decode_point_before_mode() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest {
                target: SeekTarget::Absolute(MediaTime::from_secs(8)),
                mode: SeekMode::KeyframeBefore,
            }))
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
    }

    #[test]
    fn keyframe_after_seek_is_rejected_before_demux_seek() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
        );

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest {
                target: SeekTarget::Absolute(MediaTime::from_secs(8)),
                mode: SeekMode::KeyframeAfter,
            }))
            .unwrap();

        assert!(
            seek_request_log
                .lock()
                .expect("seek request log lock")
                .is_empty()
        );
        assert!(!session.snapshot().timeline.seeking);
        assert_eq!(
            session
                .snapshot()
                .last_error
                .as_ref()
                .map(|error| &error.kind),
            Some(&PlayerErrorKind::SeekUnavailable)
        );
    }

    #[test]
    fn end_scrub_preserves_latest_request_seek_mode() {
        let mut session = PlayerSession::new();
        let seek_request_log = install_fake_media_with_seek_request_log(
            &mut session,
            vec![fake_track(2, TrackKind::Audio)],
        );

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest {
                target: SeekTarget::Absolute(MediaTime::from_secs(8)),
                mode: SeekMode::KeyframeBefore,
            }))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            })
            .unwrap();

        let requests = seek_request_log.lock().expect("seek request log lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].mode, DemuxSeekMode::DecodePointBefore);
    }

    #[test]
    fn not_seekable_demuxer_marks_timeline_and_blocks_seek() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media_with_seekability(
            &mut session,
            vec![fake_track(1, TrackKind::Video)],
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::SourceNotSeekable,
            },
        );

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(5),
            )))
            .unwrap();

        assert!(!session.snapshot().timeline.seekable);
        assert_eq!(
            session.snapshot().timeline.not_seekable_reason,
            Some(TimelineNotSeekableReason::SourceNotSeekable)
        );
        assert!(seek_log.lock().expect("seek log lock").is_empty());
        assert_eq!(
            session
                .snapshot()
                .last_error
                .as_ref()
                .map(|error| &error.kind),
            Some(&PlayerErrorKind::SeekUnavailable)
        );
    }

    #[test]
    fn seek_audio_gate_treats_no_selected_audio_as_ready() {
        let seek_commit = audio_gate_seek_commit(SeekCommitKind::Final, PlaybackResumeIntent::Play);

        let gate_status = classify_seek_audio_gate(
            seek_commit,
            AudioSeekRuntimeState::NoSelectedAudio,
            seek_commit.generation,
            None,
            50.0,
        );

        assert_eq!(gate_status, SeekAudioGateStatus::Ready);
    }

    #[test]
    fn seek_audio_gate_preserves_clear_decoder_output_and_preroll_blockers() {
        let seek_commit = audio_gate_seek_commit(SeekCommitKind::Final, PlaybackResumeIntent::Play);

        assert_eq!(
            classify_seek_audio_gate(
                seek_commit,
                AudioSeekRuntimeState::Ready,
                seek_commit.generation - 1,
                Some(100.0),
                50.0,
            ),
            SeekAudioGateStatus::WaitingForClear
        );
        assert_eq!(
            classify_seek_audio_gate(
                seek_commit,
                AudioSeekRuntimeState::WaitingForDecoder,
                seek_commit.generation,
                Some(100.0),
                50.0,
            ),
            SeekAudioGateStatus::WaitingForDecoder
        );
        assert_eq!(
            classify_seek_audio_gate(
                seek_commit,
                AudioSeekRuntimeState::WaitingForOutput,
                seek_commit.generation,
                Some(100.0),
                50.0,
            ),
            SeekAudioGateStatus::WaitingForOutput
        );
        assert_eq!(
            classify_seek_audio_gate(
                seek_commit,
                AudioSeekRuntimeState::Ready,
                seek_commit.generation,
                None,
                50.0,
            ),
            SeekAudioGateStatus::WaitingForPreroll
        );
    }

    #[test]
    fn final_play_seek_audio_gate_requires_minimal_preroll() {
        let seek_commit = audio_gate_seek_commit(SeekCommitKind::Final, PlaybackResumeIntent::Play);

        assert_eq!(
            classify_seek_audio_gate(
                seek_commit,
                AudioSeekRuntimeState::Ready,
                seek_commit.generation,
                Some(49.0),
                50.0,
            ),
            SeekAudioGateStatus::WaitingForPreroll
        );
        assert_eq!(
            classify_seek_audio_gate(
                seek_commit,
                AudioSeekRuntimeState::Ready,
                seek_commit.generation,
                Some(50.0),
                50.0,
            ),
            SeekAudioGateStatus::Ready
        );
    }

    #[test]
    fn preview_and_paused_seek_audio_gates_skip_runtime_preroll_after_clear() {
        let preview_commit =
            audio_gate_seek_commit(SeekCommitKind::Preview, PlaybackResumeIntent::Pause);
        let paused_commit =
            audio_gate_seek_commit(SeekCommitKind::Final, PlaybackResumeIntent::Pause);

        assert_eq!(
            classify_seek_audio_gate(
                preview_commit,
                AudioSeekRuntimeState::WaitingForDecoder,
                preview_commit.generation,
                None,
                50.0,
            ),
            SeekAudioGateStatus::Ready
        );
        assert_eq!(
            classify_seek_audio_gate(
                paused_commit,
                AudioSeekRuntimeState::WaitingForOutput,
                paused_commit.generation,
                None,
                50.0,
            ),
            SeekAudioGateStatus::Ready
        );
    }

    #[test]
    fn video_audio_seek_waits_for_audio_runtime_after_clear_ack() {
        let mut session = PlayerSession::new();
        install_fake_media(
            &mut session,
            vec![
                fake_track(1, TrackKind::Video),
                fake_track(2, TrackKind::Audio),
            ],
        );

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(5),
            )))
            .unwrap();

        let seek_commit = session
            .seek_commit()
            .expect("seek commit должен остаться открытым до audio runtime readiness");

        assert_eq!(
            session.pipeline.audio_buffer_clear_generation(),
            seek_commit.generation
        );
        assert_eq!(
            session.pipeline.audio_seek_runtime_state(),
            AudioSeekRuntimeState::WaitingForDecoder
        );
        assert_eq!(
            session.seek_audio_gate_status(seek_commit, 50.0),
            SeekAudioGateStatus::WaitingForDecoder
        );
        assert!(!session.seek_audio_gate_ready(seek_commit, 50.0));
    }

    #[test]
    fn audio_only_seek_does_not_commit_when_output_is_absent() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(5),
            )))
            .unwrap();

        let seek_commit = session
            .seek_commit()
            .expect("audio-only seek должен ждать audio runtime readiness");

        assert_eq!(
            session.seek_audio_gate_status(seek_commit, 50.0),
            SeekAudioGateStatus::WaitingForDecoder
        );
        assert!(!session.seek_audio_gate_ready(seek_commit, 50.0));
    }

    #[test]
    fn seek_transaction_clears_pending_packets_and_calls_demux_seek() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media(
            &mut session,
            vec![
                fake_track(1, TrackKind::Video),
                fake_track(2, TrackKind::Audio),
            ],
        );
        session
            .pipeline
            .enqueue_pending_audio_packet(PendingAudioPacket::new(
                TrackId::new(2),
                Duration::ZERO,
                None,
                None,
                session.pipeline.seek_generation(),
                Bytes::from_static(&[1, 2, 3]),
            ));
        session
            .pipeline
            .enqueue_pending_video_packet(PendingVideoPacket::new(
                TrackId::new(1),
                Duration::ZERO,
                session.pipeline.seek_generation(),
                Bytes::from_static(&[4, 5, 6]),
                true,
            ));
        session.pipeline.mark_video_decoder_bootstrapped();
        session.pipeline.note_video_packet_sent_to_decoder();
        session.pipeline.note_video_packet_sent_to_decoder();

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(5),
            )))
            .unwrap();

        assert!(session.pipeline.pending_audio_packet_is_empty());
        assert!(session.pipeline.pending_video_packet_is_empty());
        assert_eq!(
            *seek_log
                .lock()
                .expect("seek log mutex should not be poisoned"),
            vec![Duration::from_secs(5)]
        );
        assert_eq!(session.pipeline.seek_generation(), 1);
        assert!(session.pipeline.video_decoder_needs_keyframe());
        assert_eq!(session.pipeline.video_decode_in_flight_packets(), 0);
        assert!(session.seek_commit().is_some());
    }

    #[test]
    fn seek_transaction_resets_installed_audio_decoder() {
        let mut session = PlayerSession::new();
        let reset_count = Arc::new(AtomicUsize::new(0));
        let _seek_log = install_fake_media(
            &mut session,
            vec![
                fake_track(1, TrackKind::Video),
                fake_track(2, TrackKind::Audio),
            ],
        );

        session.pipeline.select_audio_track(TrackId::new(2));
        session
            .pipeline
            .install_audio_decoder(Box::new(CountingAudioDecoder::new(Arc::clone(
                &reset_count,
            ))));

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(5),
            )))
            .unwrap();

        assert_eq!(reset_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn active_seek_diagnostics_identifies_demux_blocker_before_target_frame() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(5),
            )))
            .unwrap();

        let diagnostics = session
            .active_seek_diagnostics(
                Instant::now() + Duration::from_millis(300),
                &PlayerTickConfig::default(),
            )
            .expect("active seek diagnostics should exist while seek commit is open");

        assert_eq!(diagnostics.kind, "final");
        assert_eq!(diagnostics.target, Duration::from_secs(5));
        assert_eq!(
            diagnostics.blocker,
            crate::SeekProgressBlocker::WaitingForDemux
        );
        assert!(!diagnostics.target_frame_presented);
        assert_eq!(diagnostics.queues.present_queue_depth, 0);
    }

    #[test]
    fn seek_flush_failure_does_not_call_demux_seek_or_advance_generation() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
        session
            .pipeline
            .set_video_decoder_thread(FailingFlushVideoDecoderThread::new("flush failed"));
        let initial_generation = session.pipeline.seek_generation();

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(5),
            )))
            .unwrap();

        assert!(
            seek_log
                .lock()
                .expect("seek log mutex should not be poisoned")
                .is_empty()
        );
        assert_eq!(session.pipeline.seek_generation(), initial_generation);
        assert!(session.seek_commit().is_none());
        assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
        assert!(!session.snapshot().timeline.seeking);
        assert!(session.snapshot().timeline.stale_frame);
        assert_eq!(session.snapshot().timeline.target_position, None);
        assert!(matches!(
            session
                .snapshot()
                .last_error
                .as_ref()
                .map(|error| &error.kind),
            Some(PlayerErrorKind::DecoderFlushFailed)
        ));
    }

    #[test]
    fn seek_flush_failure_clears_existing_seek_commit() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(3),
            )))
            .unwrap();
        let generation_after_first_seek = session.pipeline.seek_generation();
        assert!(session.seek_commit().is_some());

        session
            .pipeline
            .set_video_decoder_thread(FailingFlushVideoDecoderThread::new("flush failed"));
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(8),
            )))
            .unwrap();

        assert_eq!(
            *seek_log
                .lock()
                .expect("seek log mutex should not be poisoned"),
            vec![Duration::from_secs(3)]
        );
        assert_eq!(
            session.pipeline.seek_generation(),
            generation_after_first_seek
        );
        assert!(session.seek_commit().is_none());
        assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
        assert!(!session.snapshot().timeline.seeking);
        assert!(session.snapshot().timeline.stale_frame);
        assert!(matches!(
            session
                .snapshot()
                .last_error
                .as_ref()
                .map(|error| &error.kind),
            Some(PlayerErrorKind::DecoderFlushFailed)
        ));
    }

    #[test]
    fn commit_timeout_pauses_and_reports_recoverable_seek_error() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(5),
            )))
            .unwrap();
        let timeout_now = Instant::now() + Duration::from_secs(11);

        session.finish_seek_commit_if_ready(
            timeout_now,
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
        assert!(matches!(
            session
                .snapshot()
                .last_error
                .as_ref()
                .map(|error| &error.kind),
            Some(PlayerErrorKind::SeekTimeout)
        ));
    }

    #[test]
    fn paused_before_scrub_stays_paused_after_commit() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, Vec::new());

        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            })
            .unwrap();

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
        assert!(!session.snapshot().timeline.seeking);
    }

    #[test]
    fn playing_before_scrub_resumes_after_gates() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, Vec::new());

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session.dispatch_command(PlayerCommand::Pause).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            })
            .unwrap();
        session.dispatch_command(PlayerCommand::Play).unwrap();

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
        assert!(!session.snapshot().timeline.seeking);
    }

    #[test]
    fn explicit_scrub_resume_intent_survives_temporary_pause() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, Vec::new());

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.dispatch_command(PlayerCommand::BeginScrub).unwrap();
        session.dispatch_command(PlayerCommand::Pause).unwrap();
        session
            .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        session
            .dispatch_command(PlayerCommand::EndScrub {
                policy: ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
            })
            .unwrap();

        assert_eq!(
            session
                .seek_commit()
                .map(|seek_commit| seek_commit.resume_intent),
            Some(PlaybackResumeIntent::Pause)
        );
        assert!(session.override_active_seek_resume_intent(PlaybackResumeIntent::Play));

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
        assert!(!session.snapshot().timeline.seeking);
    }

    #[test]
    fn no_audio_media_seek_resumes_after_target_video_frame() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        let target_frame = decoded_frame_for_tests(Duration::from_secs(6), 42);
        session.pipeline.set_present_video_frame(target_frame);
        session.note_presented_frame_for_seek(Duration::from_secs(6));

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            1,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
        assert!(!session.snapshot().timeline.seeking);
    }

    #[test]
    fn active_seek_drains_target_frame_when_present_queue_is_full() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
        let fake_decoder = SharedFakeVideoDecoderThread::new();
        session
            .pipeline
            .set_video_decoder_thread(fake_decoder.clone());

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_secs(5), 5));
        fake_decoder.push_decoded_frame(decoded_frame_for_tests(Duration::from_secs(6), 6));

        let tick_result = session.tick(PlayerTickContext::with_config(
            Instant::now(),
            seek_admission_tick_config(1, 4),
        ));

        assert_eq!(tick_result.decoded_video_frames, 1);
        assert_eq!(tick_result.video_frames_presented, 1);
        assert!(session.seek_commit().is_none());
        assert_eq!(
            session
                .pipeline
                .present_video_frame()
                .map(|frame| frame.pts),
            Some(Duration::from_secs(6))
        );
        assert!(session.pipeline.video_present_queue_is_empty());
        assert!(!session.snapshot().timeline.stale_frame);
        assert!(
            !tick_result.pipeline_pauses.iter().any(|pause| {
                pause.reason == crate::PipelinePauseReason::WaitingForPresentQueue
            })
        );
        assert!(
            fake_decoder
                .released_handles()
                .contains(&video_core::FrameTextureHandle(5))
        );
    }

    #[test]
    fn active_seek_long_preroll_drain_keeps_present_queue_bounded() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
        let fake_decoder = SharedFakeVideoDecoderThread::new();
        session
            .pipeline
            .set_video_decoder_thread(fake_decoder.clone());

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(2),
            )))
            .unwrap();

        for frame_index in 0..32u64 {
            fake_decoder.push_decoded_frame(decoded_frame_for_tests(
                Duration::from_millis(frame_index * 50),
                100 + frame_index,
            ));
        }
        fake_decoder.push_decoded_frame(decoded_frame_for_tests(Duration::from_secs(2), 200));

        let tick_result = session.tick(PlayerTickContext::with_config(
            Instant::now(),
            seek_admission_tick_config(2, 64),
        ));

        assert_eq!(tick_result.decoded_video_frames, 33);
        assert!(session.seek_commit().is_none());
        assert!(session.pipeline.video_present_queue_len() <= 2);
        assert_eq!(
            session
                .pipeline
                .present_video_frame()
                .map(|frame| frame.pts),
            Some(Duration::from_secs(2))
        );
        assert!(!session.pipeline.has_seek_preroll_fallback_video_frame());
    }

    #[test]
    fn paused_video_seek_tick_presents_target_frame_and_stays_paused() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
        let fake_decoder = SharedFakeVideoDecoderThread::new();
        session
            .pipeline
            .set_video_decoder_thread(fake_decoder.clone());
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(1), 1));

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        fake_decoder.push_decoded_frame(decoded_frame_for_tests(Duration::from_secs(6), 6));

        let tick_result = session.tick(PlayerTickContext::with_config(
            Instant::now(),
            seek_admission_tick_config(2, 4),
        ));

        assert_eq!(tick_result.video_frames_presented, 1);
        assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
        assert_eq!(
            session
                .pipeline
                .present_video_frame()
                .map(|frame| frame.pts),
            Some(Duration::from_secs(6))
        );
        assert!(!session.snapshot().timeline.stale_frame);
        assert!(
            fake_decoder
                .released_handles()
                .contains(&video_core::FrameTextureHandle(1))
        );
    }

    #[test]
    fn final_seek_with_frozen_audio_clock_waits_for_audio_runtime_after_target_frame() {
        let mut session = PlayerSession::new();
        install_fake_media(
            &mut session,
            vec![
                fake_track(1, TrackKind::Video),
                fake_track(2, TrackKind::Audio),
            ],
        );
        let fake_decoder = SharedFakeVideoDecoderThread::new();
        session
            .pipeline
            .set_video_decoder_thread(fake_decoder.clone());
        session
            .pipeline
            .install_audio_clock(Arc::new(audio::AudioClock::new(48_000, 2)));

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        fake_decoder.push_decoded_frame(decoded_frame_for_tests(Duration::from_millis(6_016), 16));

        let tick_result = session.tick(PlayerTickContext::with_config(
            Instant::now(),
            seek_admission_tick_config(2, 4),
        ));

        assert_eq!(tick_result.video_frames_presented, 1);
        assert_eq!(
            session
                .pipeline
                .present_video_frame()
                .map(|frame| frame.pts),
            Some(Duration::from_millis(6_016))
        );
        let seek_commit = session
            .seek_commit()
            .expect("video+audio final seek должен ждать audio runtime после target frame");
        assert_eq!(
            session.seek_audio_gate_status(seek_commit, 50.0),
            SeekAudioGateStatus::WaitingForDecoder
        );
        assert_eq!(session.snapshot().playback_state, PlaybackState::Draining);
        assert!(session.snapshot().timeline.seeking);
        assert!(!session.snapshot().timeline.stale_frame);
    }

    #[test]
    fn no_audio_seek_scheduler_uses_target_before_position_commit() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
        session.update_current_position(Duration::from_secs(5));

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(24),
            )))
            .unwrap();

        assert_eq!(session.snapshot().current_position, Duration::from_secs(5));
        assert_eq!(
            session.seek_presentation_clock_override(),
            Some(Duration::from_secs(24))
        );

        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_secs(24), 42));

        let tick_config = PlayerTickConfig {
            seek_resume_video_min_ready_frames: 1,
            ..PlayerTickConfig::default()
        };
        let tick_result = session.tick(PlayerTickContext::with_config(Instant::now(), tick_config));

        assert_eq!(tick_result.video_frames_presented, 1);
        assert_eq!(
            session
                .pipeline
                .present_video_frame()
                .map(|frame| frame.pts),
            Some(Duration::from_secs(24))
        );
        assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
        assert_eq!(session.snapshot().current_position, Duration::from_secs(24));
        assert!(!session.snapshot().timeline.seeking);
    }

    #[test]
    fn eof_during_seek_schedules_bounded_progress_instead_of_idle_sleep() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        session.enter_eof_drain();

        let plan = session.worker_wakeup_plan(
            Instant::now(),
            &PlayerTickConfig::default(),
            Duration::from_millis(2),
            Duration::from_millis(250),
        );

        assert_eq!(plan.reason, crate::WorkerWakeupReason::SeekOrPreroll);
        assert_eq!(plan.delay, Some(Duration::from_millis(250)));
    }

    #[test]
    fn final_seek_near_eof_presents_current_generation_preroll_fallback() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(30),
            )))
            .unwrap();

        session.enter_eof_drain();
        session.replace_seek_preroll_fallback_frame(decoded_frame_for_tests(
            Duration::from_millis(29_950),
            77,
        ));

        let tick_result = session.tick(PlayerTickContext::new(Instant::now()));

        assert_eq!(tick_result.video_frames_presented, 1);
        assert_eq!(
            session
                .pipeline
                .present_video_frame()
                .map(|frame| frame.pts),
            Some(Duration::from_millis(29_950))
        );
        assert!(!session.snapshot().timeline.stale_frame);
        assert!(session.seek_commit().is_none());
        assert_eq!(session.playback_state(), PlaybackState::Playing);
    }

    #[test]
    fn playing_seek_waits_for_configured_video_preroll_before_resume() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(6), 42));
        session.note_presented_frame_for_seek(Duration::from_secs(6));

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            3,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Seeking);
        assert!(session.snapshot().timeline.seeking);

        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(6_016), 43));
        session
            .pipeline
            .enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(6_033), 44));

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            3,
        );

        assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
        assert!(!session.snapshot().timeline.seeking);
    }

    #[test]
    fn playing_video_seek_with_audio_waits_for_audio_runtime_after_target_frame() {
        let mut session = PlayerSession::new();
        install_fake_media(
            &mut session,
            vec![
                fake_track(1, TrackKind::Video),
                fake_track(2, TrackKind::Audio),
            ],
        );

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();
        session
            .pipeline
            .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(6), 42));
        session.note_presented_frame_for_seek(Duration::from_secs(6));

        session.finish_seek_commit_if_ready(
            Instant::now(),
            Duration::from_secs(10),
            Duration::from_millis(100),
            50.0,
            3,
        );

        let seek_commit = session
            .seek_commit()
            .expect("video+audio final seek должен остаться открытым без audio runtime");
        assert_eq!(
            session.seek_audio_gate_status(seek_commit, 50.0),
            SeekAudioGateStatus::WaitingForDecoder
        );
        assert_eq!(session.snapshot().playback_state, PlaybackState::Seeking);
        assert!(session.snapshot().timeline.seeking);
    }

    #[test]
    fn seek_generation_drops_pre_roll_frames_before_target() {
        let mut session = PlayerSession::new();
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .unwrap();

        assert!(session.should_drop_decoded_frame_for_seek(Duration::from_millis(5_999)));
        assert!(!session.should_drop_decoded_frame_for_seek(Duration::from_secs(6)));
    }

    #[test]
    fn play_pause_commands_update_snapshot_and_events() {
        let mut session = PlayerSession::new();

        session.dispatch_command(PlayerCommand::Play).unwrap();
        session.dispatch_command(PlayerCommand::Pause).unwrap();

        let events = session.take_events();
        assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
        assert!(events.contains(&PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)));
        assert!(events.contains(&PlayerEvent::PlaybackStateChanged(PlaybackState::Paused)));
    }

    #[test]
    fn invalid_volume_is_reported_and_preserves_previous_value() {
        let mut session = PlayerSession::new();

        let result = session.dispatch_command(PlayerCommand::SetVolume(1.5));

        assert!(result.is_err());
        assert_eq!(session.snapshot().volume, 1.0);
        assert!(session.snapshot().last_error.is_some());
        assert!(
            session
                .take_events()
                .iter()
                .any(|event| matches!(event, PlayerEvent::RecoverableError(_)))
        );
    }

    #[test]
    fn autoplay_open_media_enters_buffering_before_play() {
        let mut session = PlayerSession::new();
        let request = MediaOpenRequest::new(MediaSource::ExternalLabel("sample".into()), true);

        session
            .dispatch_command(PlayerCommand::OpenMedia(request))
            .unwrap();
        assert_eq!(session.snapshot().playback_state, PlaybackState::Opening);

        session
            .mark_media_opened(MediaSummary {
                title: Some("Sample".into()),
                source_label: "sample".into(),
                duration: Some(Duration::from_secs(10)),
            })
            .unwrap();

        assert_eq!(session.snapshot().playback_state, PlaybackState::Buffering);
        assert!(session.is_demuxing_active());
        assert!(session.can_present_video());
        assert!(
            session
                .finish_autoplay_preroll_if_ready(50.0)
                .expect("preroll finish should not fail")
        );
        assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
        assert_eq!(session.snapshot().duration, Some(Duration::from_secs(10)));
    }

    #[test]
    fn old_generation_render_release_does_not_touch_current_generation() {
        let mut session = PlayerSession::new();
        let old_generation = session.pipeline.render_generation();
        let old_handle = video_core::FrameTextureHandle(5);

        assert!(session.register_render_lease(old_generation, old_handle));
        session.release_video_texture(old_handle);
        session.reset_media_state();
        let new_generation = session.pipeline.render_generation();

        session.release_render_lease(old_generation, old_handle);

        assert!(new_generation > old_generation);
        assert_eq!(session.render_lease_count(), 0);
        assert_eq!(session.deferred_video_texture_release_count(), 0);
    }

    #[test]
    fn eof_drain_state_is_visible_in_snapshot() {
        let mut session = PlayerSession::new();

        session.enter_eof_drain();

        assert_eq!(session.playback_state(), PlaybackState::Draining);
        assert!(session.can_present_video());
    }

    #[test]
    fn eof_drain_keeps_demuxer_open_for_seek() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media(
            &mut session,
            vec![
                fake_track(1, TrackKind::Video),
                fake_track(2, TrackKind::Audio),
            ],
        );

        session.enter_eof_drain();

        assert!(session.pipeline.has_demuxer());
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(5),
            )))
            .unwrap();

        assert_eq!(
            seek_log.lock().expect("seek log lock").as_slice(),
            &[Duration::from_secs(5)]
        );
        assert_eq!(session.playback_state(), PlaybackState::Seeking);
        assert!(!session.draining_after_eof);
        assert!(session.snapshot().last_error.is_none());
    }

    #[test]
    fn play_from_eof_drain_rewinds_to_start_and_resumes_playback() {
        let mut session = PlayerSession::new();
        let seek_log = install_fake_media(
            &mut session,
            vec![
                fake_track(1, TrackKind::Video),
                fake_track(2, TrackKind::Audio),
            ],
        );

        session.update_current_position(Duration::from_secs(30));
        session.enter_eof_drain();

        session.dispatch_command(PlayerCommand::Play).unwrap();

        let seek_commit = session
            .seek_commit()
            .expect("play after EOF should start a seek transaction");
        assert_eq!(
            seek_log.lock().expect("seek log lock").as_slice(),
            &[Duration::ZERO]
        );
        assert_eq!(seek_commit.target_position, MediaTime::ZERO);
        assert_eq!(seek_commit.resume_intent, PlaybackResumeIntent::Play);
        assert_eq!(session.playback_state(), PlaybackState::Seeking);
        assert!(!session.draining_after_eof);
        assert!(session.snapshot().last_error.is_none());
    }

    #[test]
    fn capability_report_updates_snapshot_and_event_queue() {
        let mut session = PlayerSession::new();

        session.set_system_capabilities(capabilities_with_vp9_profile0());

        assert!(session.snapshot().capability_summary.is_some());
        assert!(
            session
                .take_events()
                .iter()
                .any(|event| matches!(event, PlayerEvent::CapabilityScanCompleted(_)))
        );
    }

    #[test]
    fn unsupported_profile_returns_player_error_before_decode() {
        let mut session = PlayerSession::new();
        session.set_system_capabilities(capabilities_with_vp9_profile0());
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2));

        let error = session
            .validate_video_decode_requirement(&requirement)
            .expect_err("VP9 profile2 must be rejected by profile0-only capabilities");

        assert_eq!(error.kind, PlayerErrorKind::UnsupportedVideoProfile);
        assert!(error.message.contains("profile VP9 Profile 2"));
        assert!(!session.can_defer_packet_refinement(&requirement));
    }

    #[test]
    fn active_video_requirement_refinement_preserves_selection_and_rejects_before_mutation() {
        let mut session = PlayerSession::new();
        session.set_system_capabilities(capabilities_with_vp9_profile0());
        let video_track_id = TrackId::new(1);
        let initial_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);
        let supported_requirement = initial_requirement
            .clone()
            .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0));
        let rejected_requirement = initial_requirement
            .clone()
            .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2));

        session
            .pipeline
            .select_video_track(video_track_id, initial_requirement);

        session
            .refine_active_video_requirement(supported_requirement.clone())
            .expect("profile0 requirement должен пройти fake capabilities");

        assert_eq!(
            session.pipeline.selected_video_track_id(),
            Some(video_track_id)
        );
        assert_eq!(
            session.pipeline.active_video_requirement(),
            Some(&supported_requirement)
        );

        let error = session
            .refine_active_video_requirement(rejected_requirement)
            .expect_err("profile2 должен быть отвергнут до изменения active requirement");

        assert_eq!(error.kind, PlayerErrorKind::UnsupportedVideoProfile);
        assert_eq!(
            session.pipeline.selected_video_track_id(),
            Some(video_track_id)
        );
        assert_eq!(
            session.pipeline.active_video_requirement(),
            Some(&supported_requirement)
        );
    }

    #[test]
    fn incomplete_vp9_hdr_container_metadata_waits_for_packet_refinement() {
        let mut session = PlayerSession::new();
        session.set_system_capabilities(capabilities_with_phase10_vp9_profile2_hdr());
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_resolution(3840, 2160)
            .with_color(bt2020_pq_limited());

        let error = session
            .validate_video_decode_requirement(&requirement)
            .expect_err("container-only VP9 HDR metadata is not strict enough yet");

        assert_eq!(error.kind, PlayerErrorKind::UnsupportedHdrMode);
        assert!(video_requirement_needs_packet_refinement(&requirement));
        assert!(session.can_defer_packet_refinement(&requirement));
    }
}
