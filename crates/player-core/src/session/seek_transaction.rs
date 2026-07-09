use std::time::{Duration, Instant};

use frame_server_core::{
    CancelScrubReason, CancelledOutcome, FinishScrubPolicy, LiveScrubDiagnostics,
    ScrubDriverOutcome, ScrubEvent, ScrubExactnessPolicy, ScrubGeneration, ScrubGenerationToken,
    ScrubTarget, ScrubTargetUpdate, ScrubTrackSelection, ValidatedFrameServerConfig,
};
use media_core::{
    MediaDemuxError, MediaDuration, MediaTime, TimeBase, TimelineNotSeekableReason,
    TimelinePreviewState, TrackKind, TrackTimestamp,
};
use tracing::{debug, trace, warn};

use crate::seek_state::{
    AccuratePrerollDemuxEventKind, FinalSeekCommitPosition, PlaybackResumeIntent, SeekCommitState,
    SeekDemuxRequestError, SeekLandingRoute, demux_seek_request_for_transaction,
};
use crate::{
    ActiveSeekDiagnosticsSnapshot, PipelineQueueDepthSnapshot, PlaybackState, PlayerError,
    PlayerErrorKind, PlayerEvent, PlayerResult, SeekAudioResumeInfo,
    SeekBootstrapDiagnosticsSnapshot, SeekCommitInfo, SeekMode, SeekProgressBlocker, SeekRequest,
    SeekTargetFramePresentation, VideoPrerollOutputFloor, VideoPrerollOutputFloorClear,
    VideoPrerollOutputFloorResult,
};

use super::audio_runtime::SeekAudioGateStatus;
use super::prepared_seek::{
    SEEK_LANDING_BACKEND_REVISION_UNTRACKED, SEEK_LANDING_FIRST_SCRUB_GENERATION,
    SEEK_LANDING_SOURCE_REVISION_UNTRACKED, seek_landing_generation_token,
};
use super::scrub_driver::{
    PlayerScrubTransactionDriver, default_scrub_execution_policy, scrub_update_guards_for_owner,
};
use super::{PlayerSession, PlayerTickConfig};

/// Максимальный шаг вперёд, при котором live scrub продолжает текущий decode-проход
/// вместо нового cold seek на keyframe-before.
///
/// Движение вперёд в пределах этого окна почти всегда дешевле продолжить с текущей
/// позиции декодера (прокат едет только вперёд, без скачка назад на keyframe).
/// Большой прыжок вперёд декодировал бы все промежуточные кадры и стал бы
/// патологически дорогим, поэтому он идёт обычным cold-маршрутом (аналог капа
/// forward extension из hover Сессии 3).
const LIVE_SCRUB_FORWARD_EXTENSION_MAX: Duration = Duration::from_secs(3);

/// Read-only срез gate-состояния, нужный только для диагностики active seek-а.
#[derive(Debug, Clone, Copy)]
pub(super) struct SeekProgressGateSnapshot {
    /// Target frame уже стал текущим non-stale present frame.
    pub(super) target_frame_presented: bool,

    /// Video gate больше не блокирует commit.
    pub(super) video_gate_ready: bool,

    /// Typed status audio gate-а с причиной возможного ожидания.
    pub(super) audio_gate_status: SeekAudioGateStatus,

    /// Сколько video frames уже готовы для post-seek resume.
    pub(super) ready_video_frames: usize,

    /// Сколько video frames требуется текущей resume policy.
    pub(super) required_video_frames: usize,
}

/// Side effects выхода из lightweight scrub после восстановления public state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimpleScrubExitMode {
    /// Только вернуть state; следующая external command сама решит audio/clock route.
    RestoreStateOnly,

    /// Вернуть state и продолжить playback, если scrub начинался из `Playing`.
    ResumeConfirmedPlayback,
}

/// Решение commit gate policy без потери причины soft fallback-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeekCommitGateDecision {
    /// Gates ещё не готовы, seek transaction остаётся открытым.
    Waiting,

    /// Video и audio gates штатно готовы.
    Ready,

    /// Video gate готов, а audio gate превысил разрешённый soft timeout.
    AudioSoftFallback {
        /// Последний typed status audio gate-а перед fallback commit-ом.
        audio_gate_status: SeekAudioGateStatus,
    },
}

impl SeekCommitGateDecision {
    /// Возвращает `true`, если seek transaction можно закрывать сейчас.
    const fn allows_commit(self) -> bool {
        matches!(self, Self::Ready | Self::AudioSoftFallback { .. })
    }

    /// Возвращает audio blocker, если commit идёт через soft fallback.
    const fn audio_soft_fallback_status(self) -> Option<SeekAudioGateStatus> {
        match self {
            Self::AudioSoftFallback { audio_gate_status } => Some(audio_gate_status),
            Self::Waiting | Self::Ready => None,
        }
    }
}

fn initial_scrub_generation_before_target(
    target_scrub_generation: ScrubGeneration,
) -> PlayerResult<ScrubGeneration> {
    let Some(initial_generation) = target_scrub_generation.get().checked_sub(1) else {
        return Err(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            "SeekLanding target scrub generation cannot be zero",
        ));
    };

    Ok(ScrubGeneration::new(initial_generation))
}

/// Мапит fatal outcome decoder output-floor boundary в player runtime error.
fn player_error_from_preroll_output_floor_error(
    operation: &str,
    error: crate::DecodeThreadError,
) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::RuntimeError,
        format!("Video decoder preroll output floor {operation} failed: {error}"),
    )
}

impl PlayerSession {
    /// Проверяет, есть ли активный seek commit для scheduler/gate логики.
    #[must_use]
    pub(crate) const fn has_active_seek_commit(&self) -> bool {
        self.seek_runtime.has_active_commit()
    }

    /// Возвращает активный seek commit для scheduler/gate логики.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn seek_commit(&self) -> Option<SeekCommitState> {
        self.seek_runtime.active_commit()
    }

    /// Собирает подробный snapshot активного seek-а для throttled stall logs.
    #[must_use]
    pub(crate) fn active_seek_diagnostics(
        &self,
        now: Instant,
        tick_config: &PlayerTickConfig,
    ) -> Option<ActiveSeekDiagnosticsSnapshot> {
        let seek_commit = self.seek_runtime.active_commit()?;
        let target_position = seek_commit.target_position.as_duration();
        let queues = self.diagnostic_queue_depths();
        let required_video_frames = self.required_seek_resume_video_ready_frames(
            seek_commit,
            tick_config.effective_seek_resume_video_min_ready_frames(),
        );
        let ready_video_frames = self.seek_ready_video_frame_count(seek_commit);
        let target_frame_presented = self.seek_presented_frame_ready(seek_commit);
        let video_gate_ready = self.seek_video_gate_ready(seek_commit, required_video_frames);
        let audio_gate_status =
            self.seek_audio_gate_status(seek_commit, tick_config.seek_resume_audio_min_buffer_ms);
        let audio_gate_ready = audio_gate_status.is_ready();
        let diagnostics_snapshot = self.diagnostics_snapshot_with_queues(queues);
        let gate_snapshot = SeekProgressGateSnapshot {
            target_frame_presented,
            video_gate_ready,
            audio_gate_status,
            ready_video_frames,
            required_video_frames,
        };
        let blocker = self.seek_progress_blocker(
            tick_config,
            queues,
            gate_snapshot,
            diagnostics_snapshot.seek_bootstrap,
        );

        Some(ActiveSeekDiagnosticsSnapshot {
            kind: "seek",
            generation: seek_commit.generation,
            pipeline_generation: self.pipeline.seek_generation(),
            selected_video_track_id: self.pipeline.selected_video_track_id(),
            selected_audio_track_id: self.pipeline.selected_audio_track_id(),
            age: now.saturating_duration_since(seek_commit.started_at),
            target: target_position,
            actual: seek_commit.actual_position.as_duration(),
            resume_intent: playback_resume_intent_name(seek_commit.resume_intent),
            seek_mode: seek_commit.seek_mode,
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
            draining_after_eof: self.is_eof_draining(),
            stale_frame: self.snapshot.timeline.stale_frame,
            stale_generation_discards: diagnostics_snapshot.drops.stale_generation,
            seek_bootstrap: diagnostics_snapshot.seek_bootstrap,
            last_pause_reason: diagnostics_snapshot.pauses.last.map(|pause| pause.reason),
            accurate_preroll: self
                .seek_runtime
                .accurate_preroll_snapshot(seek_commit.drops_decode_preroll_before_target()),
            queues,
        })
    }

    /// Пишет compact marker для первых demux packets активного seek-а.
    pub(crate) fn note_demux_packet_for_seek_trace(
        &mut self,
        packet: &media_core::Packet,
        packet_generation: u64,
    ) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        let elapsed = seek_commit.started_at.elapsed();
        let target_position = seek_commit.target_position.as_duration();
        let selected_video_packet = packet.kind == media_core::TrackKind::Video
            && self.pipeline.selected_video_track_id() == Some(packet.track_id);
        if seek_commit.drops_decode_preroll_before_target() {
            self.seek_runtime.record_accurate_preroll_demux_packet(
                packet.kind,
                selected_video_packet && packet.pts >= target_position,
                elapsed,
            );
        }

        let Some(trace_decision) = self.seek_runtime.record_post_seek_packet(packet.kind) else {
            return;
        };

        if trace_decision.first_video_packet {
            debug!(
                kind = "seek",
                target_ms = seek_commit.target_position.as_duration().as_millis(),
                actual_ms = seek_commit.actual_position.as_duration().as_millis(),
                active_seek_generation = seek_commit.generation,
                packet_generation,
                pipeline_generation = self.pipeline.seek_generation(),
                selected_video_track_id = ?self.pipeline.selected_video_track_id(),
                selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
                packet_index = trace_decision.packet_index,
                packet_track_id = %packet.track_id,
                packet_pts_ms = packet.pts.as_millis(),
                packet_dts_ms = ?packet.dts.map(|dts| dts.as_millis()),
                packet_duration_ms = ?packet.duration.map(|duration| duration.as_millis()),
                packet_keyframe = ?packet.keyframe,
                elapsed_ms = elapsed.as_millis(),
                "First post-seek video packet observed"
            );
            return;
        }

        debug!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            active_seek_generation = seek_commit.generation,
            packet_generation,
            pipeline_generation = self.pipeline.seek_generation(),
            selected_video_track_id = ?self.pipeline.selected_video_track_id(),
            selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
            packet_index = trace_decision.packet_index,
            packet_track_id = %packet.track_id,
            packet_kind = ?packet.kind,
            packet_pts_ms = packet.pts.as_millis(),
            packet_dts_ms = ?packet.dts.map(|dts| dts.as_millis()),
            packet_duration_ms = ?packet.duration.map(|duration| duration.as_millis()),
            packet_keyframe = ?packet.keyframe,
            elapsed_ms = elapsed.as_millis(),
            "Post-seek demux packet observed"
        );
    }

    /// Учитывает EOF marker demuxer-а для active Accurate seek diagnostics.
    pub(crate) fn note_demux_eof_for_seek_preroll_diagnostics(&mut self) {
        self.note_demux_event_for_seek_preroll_diagnostics(
            AccuratePrerollDemuxEventKind::EndOfStream,
        );
    }

    /// Учитывает TracksChanged marker demuxer-а для active Accurate seek diagnostics.
    pub(crate) fn note_demux_tracks_changed_for_seek_preroll_diagnostics(&mut self) {
        self.note_demux_event_for_seek_preroll_diagnostics(
            AccuratePrerollDemuxEventKind::TracksChanged,
        );
    }

    /// Учитывает fatal demux read error для active Accurate seek diagnostics.
    pub(crate) fn note_demux_error_for_seek_preroll_diagnostics(&mut self) {
        self.note_demux_event_for_seek_preroll_diagnostics(AccuratePrerollDemuxEventKind::Error);
    }

    /// Записывает demux lifecycle/error event только для Accurate skip semantics.
    fn note_demux_event_for_seek_preroll_diagnostics(
        &mut self,
        event_kind: AccuratePrerollDemuxEventKind,
    ) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        if !seek_commit.drops_decode_preroll_before_target() {
            return;
        }

        self.seek_runtime
            .record_accurate_preroll_demux_event(event_kind);
    }

    /// Пишет marker первого decoded frame-а после accepted seek.
    pub(crate) fn note_decoded_video_frame_for_seek_trace(
        &mut self,
        frame_pts: Duration,
        frame_generation: u64,
    ) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        let elapsed = seek_commit.started_at.elapsed();
        if seek_commit.drops_decode_preroll_before_target() {
            self.seek_runtime.record_accurate_preroll_decoded_frame(
                frame_pts >= seek_commit.target_position.as_duration(),
                elapsed,
            );
        }

        if !self.seek_runtime.record_first_decoded_frame() {
            return;
        }

        debug!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            active_seek_generation = seek_commit.generation,
            pipeline_generation = self.pipeline.seek_generation(),
            frame_pts_ms = frame_pts.as_millis(),
            frame_generation,
            elapsed_ms = elapsed.as_millis(),
            "First post-seek decoded frame observed"
        );
    }

    /// Пишет marker первого decoded frame-а, который дошёл до presentation queue.
    pub(crate) fn note_queued_video_frame_for_seek_trace(
        &mut self,
        frame_pts: Duration,
        frame_generation: u64,
    ) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        let elapsed = seek_commit.started_at.elapsed();
        if seek_commit.drops_decode_preroll_before_target() {
            self.seek_runtime.record_accurate_preroll_queued_frame(
                frame_pts >= seek_commit.target_position.as_duration(),
                elapsed,
            );
        }

        if !self.seek_runtime.record_first_queued_frame() {
            return;
        }

        debug!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            active_seek_generation = seek_commit.generation,
            pipeline_generation = self.pipeline.seek_generation(),
            frame_pts_ms = frame_pts.as_millis(),
            frame_generation,
            present_queue_depth = self.pipeline.video_present_queue_len(),
            elapsed_ms = elapsed.as_millis(),
            "First post-seek queued frame observed"
        );
    }

    /// Учитывает demuxed audio packet, отброшенный как Accurate preroll.
    pub(crate) fn note_skipped_audio_preroll_packet_for_seek_diagnostics(&mut self) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        if seek_commit.drops_decode_preroll_before_target() {
            self.seek_runtime.record_skipped_audio_preroll_packet();
        }
    }

    /// Учитывает pre-target video packet, отправленный decoder-у во время Accurate seek-а.
    pub(crate) fn note_video_preroll_packet_sent_for_seek_diagnostics(&mut self) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        if seek_commit.drops_decode_preroll_before_target() {
            self.seek_runtime.record_video_preroll_packet_sent();
        }
    }

    /// Учитывает target-or-after video packet, отправленный до первого landing frame.
    pub(crate) fn note_target_or_after_video_packet_sent_for_seek_diagnostics(&mut self) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        if seek_commit.drops_decode_preroll_before_target()
            && !self.seek_presented_frame_ready(seek_commit)
        {
            self.seek_runtime.record_target_or_after_video_packet_sent();
        }
    }

    /// Учитывает decoded pre-target frame, который не дошёл до обычного scheduler-а.
    pub(crate) fn note_decoded_pre_target_frame_dropped_for_seek_diagnostics(&mut self) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        if seek_commit.drops_decode_preroll_before_target() {
            self.seek_runtime.record_decoded_pre_target_frame_dropped();
        }
    }

    /// Учитывает frame, который backend подавил ниже decoder-side Accurate output-floor.
    pub(crate) fn note_suppressed_preroll_frame_for_seek_diagnostics(
        &mut self,
        pts: Duration,
        generation: u64,
        floor_pts: Duration,
    ) -> bool {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return false;
        };

        let matches_active_accurate_seek = seek_commit.generation == generation
            && seek_commit.drops_decode_preroll_before_target()
            && pts < seek_commit.target_position.as_duration();
        if !matches_active_accurate_seek {
            return false;
        }

        self.seek_runtime.record_decoded_pre_target_frame_dropped();
        trace!(
            pts_ms = pts.as_millis(),
            generation,
            floor_ms = floor_pts.as_millis(),
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            "Accurate seek preroll frame suppressed by decoder output floor"
        );
        true
    }

    /// Учитывает decoder/video admission backpressure во время Accurate fast-preroll-а.
    pub(crate) fn note_decoder_backpressure_for_seek_preroll_diagnostics(&mut self) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        if seek_commit.drops_decode_preroll_before_target()
            && !self.seek_presented_frame_ready(seek_commit)
        {
            self.seek_runtime.record_decoder_backpressure_pause();
        }
    }

    /// Возвращает seek-policy clock base как временный clock, пока audio clock недоступен.
    ///
    /// Accurate seek не должен вести runtime clock от раннего `actual_position`, иначе длинный
    /// GOP превращает preroll в видимое/слышимое воспроизведение до пользовательской цели.
    #[must_use]
    pub(crate) fn seek_presentation_clock_override(&self) -> Option<Duration> {
        if self.pipeline.has_audio_clock() {
            return None;
        }

        self.seek_runtime
            .active_commit()
            .map(|seek_commit| self.accepted_seek_clock_base(seek_commit))
    }

    /// Выбирает clock base для runtime после accepted demux seek-а.
    ///
    /// Decode-safe `actual_position` может быть на несколько секунд раньше target-а. Декодер
    /// всё ещё получает этот preroll для accurate seek-а, но audio trim и scheduler сравнивают
    /// данные с user target. Explicit keyframe-before seek сохраняет actual как runtime anchor.
    fn accepted_seek_clock_base(&self, seek_commit: SeekCommitState) -> Duration {
        seek_commit.runtime_clock_base()
    }

    /// Перепривязывает clocks после accepted seek, когда demuxer уже сообщил actual point.
    pub(super) fn reanchor_clocks_after_seek_accept(&mut self, seek_commit: SeekCommitState) {
        let clock_base = self.accepted_seek_clock_base(seek_commit);
        self.pipeline
            .reanchor_media_clock_for_seek(clock_base, Instant::now());

        debug!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            clock_base_ms = clock_base.as_millis(),
            generation = seek_commit.generation,
            pipeline_generation = self.pipeline.seek_generation(),
            "Seek media clock перепривязан после accepted demux point"
        );
    }

    /// Проверяет, нужно ли выбросить decoded frame как pre-roll до seek target.
    ///
    /// `actual_position` нужен decoder-у как безопасный старт после flush, но frame до
    /// user target не должен открывать video gate и попадать в обычный playback.
    #[must_use]
    pub(crate) fn should_drop_decoded_frame_for_seek(&self, frame_pts: Duration) -> bool {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return false;
        };

        self.pipeline.has_selected_video_track()
            && seek_commit.drops_decode_preroll_before_target()
            && !self.active_seek_presents_preroll_progressively()
            && frame_pts < seek_commit.target_position.as_duration()
    }

    /// Проверяет, показывает ли активный seek preroll-кадры «прокатом» (live scrub).
    ///
    /// Прокат включён только для LiveScrub route и только пока exact landing frame
    /// ещё не показан: каждый декодированный pre-target кадр становится видимым
    /// (latest-wins), как «живая перемотка» в монтажных программах. Commit-гейты
    /// это не ослабляет: landing frame по-прежнему обязан быть target-or-after.
    /// После показа landing frame прокат выключается, чтобы decode-ahead для
    /// resume не уводил картинку дальше цели, пока пользователь держит позицию.
    #[must_use]
    pub(crate) fn active_seek_presents_preroll_progressively(&self) -> bool {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return false;
        };

        self.pipeline.has_selected_video_track()
            && seek_commit.drops_decode_preroll_before_target()
            && self.seek_runtime.active_seek_landing_is_live_scrub()
            && !self.seek_presented_frame_ready(seek_commit)
    }

    /// Проверяет, является ли video packet частью accurate seek preroll до user target.
    ///
    /// Такой packet нужен decoder-у для восстановления reference chain после flush-а, но
    /// обычный audio-clock decode-ahead не должен задавать ему playback-темп.
    #[must_use]
    pub(crate) fn should_fast_decode_video_packet_for_seek_preroll(
        &self,
        packet_pts: Duration,
    ) -> bool {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return false;
        };

        self.pipeline.has_selected_video_track()
            && seek_commit.drops_decode_preroll_before_target()
            && !self.seek_presented_frame_ready(seek_commit)
            && packet_pts < seek_commit.target_position.as_duration()
    }

    /// Проверяет, должен ли active Accurate seek временно обойти audio-clock decode-ahead.
    ///
    /// До первого target-or-after landing frame decoder-у могут понадобиться не только
    /// pre-target reference packets, но и несколько packets после target-а для DPB/reorder
    /// output. Этот intent не обходит texture capacity, packet-channel backpressure,
    /// generation checks или codec bootstrap; он снимает только audio-clock pacing.
    #[must_use]
    pub(crate) fn should_bypass_audio_clock_decode_ahead_for_active_seek(&self) -> bool {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return false;
        };

        self.pipeline.has_selected_video_track()
            && seek_commit.drops_decode_preroll_before_target()
            && !self.seek_presented_frame_ready(seek_commit)
    }

    /// Проверяет, может ли pre-target Accurate packet обойти только texture-capacity gate.
    #[must_use]
    pub(crate) fn decoder_output_floor_applies_to_seek_preroll_packet(
        &self,
        packet_pts: Duration,
        packet_generation: u64,
    ) -> bool {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return false;
        };

        self.pipeline.has_selected_video_track()
            && seek_commit.generation == packet_generation
            && seek_commit.drops_decode_preroll_before_target()
            && !self.seek_presented_frame_ready(seek_commit)
            && packet_pts < seek_commit.target_position.as_duration()
            && self
                .seek_runtime
                .decoder_output_floor_applied_for_generation(packet_generation)
    }

    /// Проверяет, должен ли tick включить fast-preroll режим для accurate seek-а.
    ///
    /// Режим активен только пока video gate ещё не набрал target frame и нужный
    /// bounded resume-preroll. `KeyframeBefore` сюда не попадает: он сохраняет
    /// demux actual/keyframe как runtime landing point.
    #[must_use]
    pub(crate) fn active_accurate_seek_needs_fast_video_preroll(
        &self,
        resume_video_min_ready_frames: usize,
    ) -> bool {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return false;
        };

        self.pipeline.has_selected_video_track()
            && seek_commit.drops_decode_preroll_before_target()
            && !self.seek_video_gate_ready(seek_commit, resume_video_min_ready_frames)
    }

    /// Проверяет, можно ли пропустить demuxed audio packet, который целиком лежит до target.
    ///
    /// Частично пересекающий target packet оставляем audio runtime-у: там PCM уже обрезается
    /// по `media_clock_base`, чтобы не проиграть старый preroll внутри packet-а.
    #[must_use]
    pub(crate) fn should_drop_demuxed_audio_packet_for_seek(
        &self,
        packet_pts: Duration,
        packet_duration: Option<Duration>,
    ) -> bool {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return false;
        };

        if !self.pipeline.has_selected_audio_track() {
            return false;
        }

        if !seek_commit.drops_decode_preroll_before_target() {
            return false;
        }

        let target_position = seek_commit.target_position.as_duration();
        let Some(packet_duration) = packet_duration else {
            return false;
        };

        packet_pts.saturating_add(packet_duration) <= target_position
    }

    /// Возвращает target активного final seek-а, если такой transition сейчас идёт.
    #[must_use]
    pub(crate) fn active_final_seek_target(&self) -> Option<Duration> {
        self.seek_runtime
            .active_commit()
            .map(|seek_commit| seek_commit.target_position.as_duration())
    }

    /// Проверяет, может ли active seek обойти обычное A/V scheduler window для queued frame-а.
    ///
    /// Accurate seek форсирует только frame на user target-е или позже. Explicit
    /// keyframe-before seek сохраняет actual/keyframe frame как разрешённую landing point.
    #[must_use]
    pub(crate) fn active_seek_frame_ready_for_scheduler(
        &self,
        frame_pts: Duration,
        frame_generation: u64,
    ) -> bool {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return false;
        };

        self.final_seek_frame_ready_for_scheduler(seek_commit, frame_pts, frame_generation)
    }

    /// Проверяет, может ли final seek показать queued frame в обход обычного scheduler window.
    fn final_seek_frame_ready_for_scheduler(
        &self,
        seek_commit: SeekCommitState,
        frame_pts: Duration,
        frame_generation: u64,
    ) -> bool {
        self.seek_landing_frame_matches_active_commit(
            seek_commit,
            frame_pts,
            frame_generation,
            false,
        ) && !self.final_seek_visible_frame_ready(seek_commit)
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
            self.release_video_texture(frame.resource_handle);
        }
    }

    /// Пробует включить decoder-side output floor для Accurate seek preroll.
    pub(super) fn apply_decoder_output_floor_for_seek(
        &mut self,
        seek_commit: SeekCommitState,
    ) -> Result<(), PlayerError> {
        self.seek_runtime.clear_decoder_output_floor();
        if !self.pipeline.has_selected_video_track()
            || !seek_commit.drops_decode_preroll_before_target()
        {
            return Ok(());
        }

        // Прокат live scrub: pre-target кадры должны выйти из декодера и стать
        // видимыми, поэтому decoder-side floor не ставим. Подавление кадров и
        // texture-capacity bypass остаются политикой one-shot Accurate seek-а.
        if self.active_seek_presents_preroll_progressively() {
            debug!(
                kind = "seek",
                generation = seek_commit.generation,
                target_ms = seek_commit.target_position.as_duration().as_millis(),
                "Live scrub прокат: decoder output floor пропущен"
            );
            return Ok(());
        }

        let floor_pts = seek_commit.target_position.as_duration();
        let floor = VideoPrerollOutputFloor {
            generation: seek_commit.generation,
            floor_pts,
            retain_latest_before_floor: true,
        };

        match self.pipeline.set_video_decoder_preroll_output_floor(floor) {
            VideoPrerollOutputFloorResult::Applied | VideoPrerollOutputFloorResult::Unchanged => {
                self.seek_runtime
                    .mark_decoder_output_floor_applied(seek_commit.generation, floor_pts);
                debug!(
                    kind = "seek",
                    generation = seek_commit.generation,
                    target_ms = floor_pts.as_millis(),
                    retain_latest_before_floor = true,
                    "Accurate seek decoder output floor applied"
                );
                Ok(())
            }
            VideoPrerollOutputFloorResult::AbsentDecoder => {
                debug!(
                    kind = "seek",
                    generation = seek_commit.generation,
                    target_ms = floor_pts.as_millis(),
                    "Accurate seek decoder output floor skipped: decoder absent"
                );
                Ok(())
            }
            VideoPrerollOutputFloorResult::Unsupported => {
                debug!(
                    kind = "seek",
                    generation = seek_commit.generation,
                    target_ms = floor_pts.as_millis(),
                    "Accurate seek decoder output floor unsupported; using player-side drop path"
                );
                Ok(())
            }
            VideoPrerollOutputFloorResult::Backpressure(reason) => {
                debug!(
                    kind = "seek",
                    generation = seek_commit.generation,
                    target_ms = floor_pts.as_millis(),
                    reason = %reason,
                    "Accurate seek decoder output floor deferred by control-channel backpressure"
                );
                Ok(())
            }
            VideoPrerollOutputFloorResult::Cleared => {
                debug!(
                    kind = "seek",
                    generation = seek_commit.generation,
                    target_ms = floor_pts.as_millis(),
                    "Accurate seek decoder output floor set returned unexpected Cleared"
                );
                Ok(())
            }
            VideoPrerollOutputFloorResult::Fatal(error) => {
                Err(player_error_from_preroll_output_floor_error("set", error))
            }
        }
    }

    /// Очищает active decoder-side floor, если session знает о подтверждённом floor.
    pub(super) fn clear_active_seek_decoder_output_floor(
        &mut self,
        reason: &'static str,
    ) -> Result<(), PlayerError> {
        let Some(floor) = self.seek_runtime.decoder_output_floor() else {
            return Ok(());
        };

        let clear = VideoPrerollOutputFloorClear::MatchingGeneration(floor.generation);
        let result = self
            .pipeline
            .clear_video_decoder_preroll_output_floor(clear);
        match result {
            VideoPrerollOutputFloorResult::Cleared
            | VideoPrerollOutputFloorResult::Unchanged
            | VideoPrerollOutputFloorResult::AbsentDecoder
            | VideoPrerollOutputFloorResult::Unsupported => {
                debug!(
                    kind = "seek",
                    generation = floor.generation,
                    floor_ms = floor.floor_pts.as_millis(),
                    clear_reason = reason,
                    clear_result = "cleared_or_noop",
                    "Accurate seek decoder output floor cleared"
                );
                self.seek_runtime.clear_decoder_output_floor();
                Ok(())
            }
            VideoPrerollOutputFloorResult::Backpressure(backpressure_reason) => {
                warn!(
                    kind = "seek",
                    generation = floor.generation,
                    floor_ms = floor.floor_pts.as_millis(),
                    clear_reason = reason,
                    reason = %backpressure_reason,
                    "Accurate seek decoder output floor clear hit control-channel backpressure"
                );
                self.seek_runtime.clear_decoder_output_floor();
                Ok(())
            }
            VideoPrerollOutputFloorResult::Applied => {
                warn!(
                    kind = "seek",
                    generation = floor.generation,
                    floor_ms = floor.floor_pts.as_millis(),
                    clear_reason = reason,
                    "Accurate seek decoder output floor clear returned unexpected Applied"
                );
                self.seek_runtime.clear_decoder_output_floor();
                Ok(())
            }
            VideoPrerollOutputFloorResult::Fatal(error) => {
                self.seek_runtime.clear_decoder_output_floor();
                Err(player_error_from_preroll_output_floor_error("clear", error))
            }
        }
    }

    /// Отмечает, что final seek near EOF показал свежий fallback frame текущего transition-а.
    pub(crate) fn note_presented_seek_eof_fallback_frame(&mut self, frame_pts: Duration) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        let first_post_seek_presented_frame =
            self.seek_runtime.record_first_presented_frame(frame_pts);
        if seek_commit.drops_decode_preroll_before_target() {
            self.seek_runtime.record_accurate_preroll_presented_frame(
                frame_pts >= seek_commit.target_position.as_duration(),
                seek_commit.started_at.elapsed(),
            );
        }
        let present_frame_generation = self
            .pipeline
            .present_video_frame()
            .map(|frame| frame.generation);
        if first_post_seek_presented_frame {
            debug!(
                kind = "seek",
                target_ms = seek_commit.target_position.as_duration().as_millis(),
                actual_ms = seek_commit.actual_position.as_duration().as_millis(),
                active_seek_generation = seek_commit.generation,
                pipeline_generation = self.pipeline.seek_generation(),
                frame_pts_ms = frame_pts.as_millis(),
                frame_generation = ?present_frame_generation,
                eof_fallback = true,
                stale_frame = self.snapshot.timeline.stale_frame,
                "First post-seek presented frame observed"
            );
        }

        self.seek_runtime
            .set_eof_fallback_video_position(MediaTime::from_duration(frame_pts));
        self.snapshot.timeline.stale_frame = false;
    }

    /// Отмечает, что свежий video frame текущего seek-а уже стал текущим кадром presentation.
    pub(crate) fn note_presented_frame_for_seek(&mut self, frame_pts: Duration) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        let present_frame_generation = self
            .pipeline
            .present_video_frame()
            .filter(|frame| frame.pts == frame_pts)
            .map(|frame| frame.generation);
        let Some(present_frame_generation) = present_frame_generation else {
            return;
        };
        // Только что показанный frame уже заменил старый кадр; stale flag очищаем только после guard.
        if !self.seek_landing_frame_matches_active_commit(
            seek_commit,
            frame_pts,
            present_frame_generation,
            false,
        ) {
            // Прокат live scrub: показанный pre-target кадр текущего generation-а —
            // это живая картинка и валидный EOF fallback, а не устаревший кадр.
            // Landing-гейт он всё равно не открывает (guard выше уже отказал).
            if present_frame_generation == seek_commit.generation
                && frame_pts < seek_commit.target_position.as_duration()
                && self.active_seek_presents_preroll_progressively()
            {
                self.seek_runtime
                    .set_eof_fallback_video_position(MediaTime::from_duration(frame_pts));
                self.snapshot.timeline.stale_frame = false;
            }
            debug!(
                kind = "seek",
                target_ms = seek_commit.target_position.as_duration().as_millis(),
                actual_ms = seek_commit.actual_position.as_duration().as_millis(),
                active_seek_generation = seek_commit.generation,
                pipeline_generation = self.pipeline.seek_generation(),
                frame_pts_ms = frame_pts.as_millis(),
                frame_generation = present_frame_generation,
                stale_frame = self.snapshot.timeline.stale_frame,
                "Presented frame rejected as seek landing frame"
            );
            return;
        }

        let first_post_seek_presented_frame =
            self.seek_runtime.record_first_presented_frame(frame_pts);
        if seek_commit.drops_decode_preroll_before_target() {
            self.seek_runtime.record_accurate_preroll_presented_frame(
                frame_pts >= seek_commit.target_position.as_duration(),
                seek_commit.started_at.elapsed(),
            );
        }
        if first_post_seek_presented_frame {
            debug!(
                kind = "seek",
                target_ms = seek_commit.target_position.as_duration().as_millis(),
                actual_ms = seek_commit.actual_position.as_duration().as_millis(),
                active_seek_generation = seek_commit.generation,
                pipeline_generation = self.pipeline.seek_generation(),
                frame_pts_ms = frame_pts.as_millis(),
                frame_generation = present_frame_generation,
                stale_frame = self.snapshot.timeline.stale_frame,
                elapsed_ms = seek_commit.started_at.elapsed().as_millis(),
                "First post-seek presented frame observed"
            );
        }

        trace!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            frame_pts_ms = frame_pts.as_millis(),
            generation = seek_commit.generation,
            "Seek transaction увидел presented frame"
        );

        self.seek_runtime.clear_eof_fallback_video_position();
        self.snapshot.timeline.stale_frame = false;
        if first_post_seek_presented_frame {
            self.pending_events
                .push(PlayerEvent::SeekTargetFramePresented(
                    SeekTargetFramePresentation {
                        target_position: seek_commit.target_position.as_duration(),
                        frame_pts,
                    },
                ));
        }
    }

    /// Завершает seek commit, когда video/audio gates готовы, или применяет timeout.
    ///
    /// Timeout защищает только реально ожидающие transition-ы: если gates уже
    /// готовы на этом tick-е, commit должен победить даже после истечения budget.
    #[cfg(test)]
    pub(super) fn finish_seek_commit_if_ready_for_tests(
        &mut self,
        now: Instant,
        commit_timeout: Duration,
        resume_audio_min_buffer_ms: f64,
        resume_audio_gate_timeout: Duration,
        resume_video_min_ready_frames: usize,
    ) {
        let tick_config = PlayerTickConfig {
            seek_commit_timeout: commit_timeout,
            seek_resume_audio_min_buffer_ms: resume_audio_min_buffer_ms,
            seek_resume_audio_gate_timeout: resume_audio_gate_timeout,
            seek_resume_video_min_ready_frames: resume_video_min_ready_frames,
            ..PlayerTickConfig::default()
        };

        self.finish_seek_commit_if_ready(now, &tick_config);
    }

    /// Завершает seek commit по фактическому tick config, чтобы diagnostics и gates
    /// видели один и тот же набор scheduler/backpressure knobs.
    pub(crate) fn finish_seek_commit_if_ready(
        &mut self,
        now: Instant,
        tick_config: &PlayerTickConfig,
    ) {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return;
        };

        if !self.seek_runtime.active_seek_landing_commit_allowed() {
            return;
        }

        let resume_video_min_ready_frames =
            tick_config.effective_seek_resume_video_min_ready_frames();
        let gate_decision = self.seek_commit_gate_decision(
            seek_commit,
            now,
            tick_config.seek_resume_audio_min_buffer_ms,
            tick_config.seek_resume_audio_gate_timeout,
            resume_video_min_ready_frames,
        );
        if gate_decision.allows_commit() {
            if let Some(audio_gate_status) = gate_decision.audio_soft_fallback_status() {
                warn!(
                    target_ms = seek_commit.target_position.as_duration().as_millis(),
                    actual_ms = seek_commit.actual_position.as_duration().as_millis(),
                    generation = seek_commit.generation,
                    audio_gate_status = ?audio_gate_status,
                    audio_gate_timeout_ms = tick_config.seek_resume_audio_gate_timeout.as_millis(),
                    "Final seek commit продолжен через audio gate soft fallback"
                );
                self.record_seek_audio_soft_fallback(
                    seek_commit,
                    audio_gate_status,
                    tick_config.seek_resume_audio_gate_timeout,
                );
            }

            self.complete_seek_commit(seek_commit);
            return;
        }

        if now.saturating_duration_since(seek_commit.started_at) < tick_config.seek_commit_timeout {
            return;
        }

        let timeout_blocker = self.seek_timeout_blocker_from_active_diagnostics(now, tick_config);
        self.fail_seek_commit_on_timeout(seek_commit, timeout_blocker);
    }

    /// Публикует recoverable диагностику, когда final seek отпущен без готового audio gate-а.
    fn record_seek_audio_soft_fallback(
        &mut self,
        seek_commit: SeekCommitState,
        audio_gate_status: SeekAudioGateStatus,
        resume_audio_gate_timeout: Duration,
    ) {
        let blocker = audio_gate_status
            .blocker()
            .unwrap_or(SeekProgressBlocker::WaitingForAudioPreroll);
        let error = PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!(
                "Final seek resumed without ready audio after {} ms: target={} ms, blocker={}",
                resume_audio_gate_timeout.as_millis(),
                seek_commit.target_position.as_duration().as_millis(),
                blocker.metric_name()
            ),
        );

        self.record_recoverable_error(error);
    }

    /// Проверяет video/audio gates и возвращает причину, если commit разрешён soft fallback-ом.
    fn seek_commit_gate_decision(
        &self,
        seek_commit: SeekCommitState,
        now: Instant,
        resume_audio_min_buffer_ms: f64,
        resume_audio_gate_timeout: Duration,
        resume_video_min_ready_frames: usize,
    ) -> SeekCommitGateDecision {
        if !self.seek_video_gate_ready(seek_commit, resume_video_min_ready_frames) {
            return SeekCommitGateDecision::Waiting;
        }

        let audio_gate_status =
            self.seek_audio_gate_status(seek_commit, resume_audio_min_buffer_ms);
        if audio_gate_status.is_ready() {
            return SeekCommitGateDecision::Ready;
        }

        if self.seek_audio_gate_soft_fallback_ready(
            seek_commit,
            now,
            audio_gate_status,
            resume_audio_gate_timeout,
        ) {
            return SeekCommitGateDecision::AudioSoftFallback { audio_gate_status };
        }

        SeekCommitGateDecision::Waiting
    }

    /// Берёт текущий blocker из active seek diagnostics до очистки timeout-состояния.
    fn seek_timeout_blocker_from_active_diagnostics(
        &self,
        now: Instant,
        tick_config: &PlayerTickConfig,
    ) -> SeekProgressBlocker {
        self.active_seek_diagnostics(now, tick_config)
            .map(|diagnostics| diagnostics.blocker)
            .unwrap_or(SeekProgressBlocker::Unknown)
    }

    /// Возвращает `true`, если audio gate можно отпустить без бесконечного удержания seek-а.
    fn seek_audio_gate_soft_fallback_ready(
        &self,
        seek_commit: SeekCommitState,
        now: Instant,
        audio_gate_status: SeekAudioGateStatus,
        resume_audio_gate_timeout: Duration,
    ) -> bool {
        if seek_commit.resume_intent != PlaybackResumeIntent::Play {
            return false;
        }

        if !self.pipeline.has_selected_audio_track() {
            return false;
        }

        if !self.pipeline.has_selected_video_track() {
            return false;
        }

        if self.active_prepared_seek_landing_matches_commit(seek_commit) {
            return false;
        }

        if !audio_gate_status.can_soft_fallback() {
            return false;
        }

        now.saturating_duration_since(seek_commit.started_at) >= resume_audio_gate_timeout
    }

    /// Video gate готов, когда текущая seek policy увидела нужный frame.
    pub(super) fn seek_video_gate_ready(
        &self,
        seek_commit: SeekCommitState,
        resume_video_min_ready_frames: usize,
    ) -> bool {
        if !self.pipeline.has_selected_video_track() {
            return true;
        }

        if self.prepared_seek_video_runway_commit_ready(seek_commit) {
            return true;
        }

        let target_position = seek_commit.target_position.as_duration();
        let landing_frame_presented = self.seek_presented_frame_ready(seek_commit);
        let eof_fallback_presented =
            self.seek_eof_fallback_video_ready(seek_commit, target_position);

        if eof_fallback_presented {
            return true;
        }

        if !landing_frame_presented {
            return false;
        }

        let required_ready_frames = self
            .required_seek_resume_video_ready_frames(seek_commit, resume_video_min_ready_frames);

        self.seek_ready_video_frame_count(seek_commit) >= required_ready_frames
    }

    /// Проверяет, что текущая seek policy уже получила non-stale present frame.
    fn seek_presented_frame_ready(&self, seek_commit: SeekCommitState) -> bool {
        if self.final_seek_visible_frame_ready(seek_commit) {
            return true;
        }

        let target_position = seek_commit.target_position.as_duration();
        self.current_seek_landing_frame_position(seek_commit)
            .is_some_and(|frame_position| frame_position >= target_position)
    }

    /// Проверяет, что обычный final seek уже показал свежий frame текущего generation-а.
    fn final_seek_visible_frame_ready(&self, seek_commit: SeekCommitState) -> bool {
        self.current_seek_landing_frame_position(seek_commit)
            .is_some()
    }

    /// Проверяет текущий present frame как landing point активного seek-а.
    fn current_seek_landing_frame_position(
        &self,
        seek_commit: SeekCommitState,
    ) -> Option<Duration> {
        let present_frame = self.pipeline.present_video_frame()?;
        self.seek_landing_frame_matches_active_commit(
            seek_commit,
            present_frame.pts,
            present_frame.generation,
            self.snapshot.timeline.stale_frame,
        )
        .then_some(present_frame.pts)
    }

    /// Проверяет player-side invariant для frame-а, который может закрыть seek commit.
    ///
    /// `timeline_stale` относится к read-only проверкам уже текущего present frame-а.
    fn seek_landing_frame_matches_active_commit(
        &self,
        seek_commit: SeekCommitState,
        frame_pts: Duration,
        frame_generation: u64,
        timeline_stale: bool,
    ) -> bool {
        let landing_min_position = seek_commit.landing_frame_min_position();

        self.pipeline.has_selected_video_track()
            && frame_generation == seek_commit.generation
            && !timeline_stale
            && frame_pts >= landing_min_position
    }

    /// Классифицирует текущую причину, по которой active seek ещё не закрыл gates.
    pub(super) fn seek_progress_blocker(
        &self,
        tick_config: &PlayerTickConfig,
        queues: PipelineQueueDepthSnapshot,
        gate_snapshot: SeekProgressGateSnapshot,
        seek_bootstrap: SeekBootstrapDiagnosticsSnapshot,
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
            return self.video_target_frame_blocker(queues, seek_bootstrap);
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
    pub(super) fn video_target_frame_blocker(
        &self,
        queues: PipelineQueueDepthSnapshot,
        seek_bootstrap: SeekBootstrapDiagnosticsSnapshot,
    ) -> SeekProgressBlocker {
        let waiting_for_decode_start_after_drops = self.pipeline.video_decoder_needs_keyframe()
            && seek_bootstrap.dropped_until_keyframe > 0;

        if waiting_for_decode_start_after_drops {
            return SeekProgressBlocker::WaitingForPostFlushKeyframe;
        }

        if queues.decoder_send_queue_depth > 0 || queues.decoder_in_flight_packets > 0 {
            return SeekProgressBlocker::WaitingForDecoderOutput;
        }

        if queues.pending_video_packets > 0 {
            return SeekProgressBlocker::WaitingForDecoderInput;
        }

        if self
            .pipeline
            .front_queued_video_frame()
            .is_some_and(|frame| {
                self.active_seek_frame_ready_for_scheduler(frame.pts, frame.generation)
            })
        {
            return SeekProgressBlocker::ReadyForScheduler;
        }

        if !self.pipeline.video_present_queue_is_empty() {
            return SeekProgressBlocker::WaitingForScheduler;
        }

        if self.is_demuxing_active() && !self.is_eof_draining() {
            return SeekProgressBlocker::WaitingForDemux;
        }

        if self.pipeline.has_present_video_frame() {
            return SeekProgressBlocker::WaitingForScheduler;
        }

        SeekProgressBlocker::WaitingForVideoTargetFrame
    }

    /// Возвращает требуемый video preroll для конкретного seek transaction-а.
    pub(super) fn required_seek_resume_video_ready_frames(
        &self,
        seek_commit: SeekCommitState,
        resume_video_min_ready_frames: usize,
    ) -> usize {
        match seek_commit.resume_intent {
            PlaybackResumeIntent::Play if self.pipeline.has_selected_audio_track() => 1,
            PlaybackResumeIntent::Play => resume_video_min_ready_frames.max(1),
            _ => 1,
        }
    }

    /// Считает current frame и уже декодированные future frames для seek resume.
    ///
    /// Resume budget использует тот же landing-frame guard, что и commit: frame текущего
    /// generation-а должен быть на user target-е или позже. Decode-safe preroll до target-а
    /// нужен только decoder-у и не считается готовым playback кадром.
    fn seek_ready_video_frame_count(&self, seek_commit: SeekCommitState) -> usize {
        let current_frame_ready = self.seek_presented_frame_ready(seek_commit);
        let queued_ready_frames = self
            .pipeline
            .queued_video_frames()
            .filter(|frame| {
                self.seek_landing_frame_matches_active_commit(
                    seek_commit,
                    frame.pts,
                    frame.generation,
                    false,
                )
            })
            .count();

        usize::from(current_frame_ready) + queued_ready_frames
    }

    /// Проверяет, что video decoder больше НЕ может выдать target-or-after кадр текущего seek-а.
    ///
    /// EOF fallback (показ последнего pre-target кадра как committed position)
    /// допустим только когда точный target кадр уже физически недостижим: нет pending video
    /// packets, нет in-flight packets и decoder thread не держит свою packet queue. Иначе target
    /// кадр ещё может прийти из EOF-drain-а, и коммитить seek по pre-target кадру нельзя.
    ///
    /// Инвариант продублирован здесь, чтобы commit gate не зависел только от presenter-а,
    /// который выставляет `eof_fallback_video_position`.
    fn seek_eof_video_decoder_drained_for_fallback(&self) -> bool {
        self.pipeline.pending_video_packet_is_empty()
            && self.pipeline.video_decode_in_flight_packets() == 0
            && self
                .pipeline
                .video_decoder_packet_queue_depth()
                .is_none_or(|packet_queue_depth| packet_queue_depth == 0)
    }

    /// EOF fallback готов только если показан свежий frame текущего final seek transition-а.
    fn seek_eof_fallback_video_ready(
        &self,
        _seek_commit: SeekCommitState,
        target_position: Duration,
    ) -> bool {
        if !self.is_eof_draining() {
            return false;
        }

        // Target кадр ещё может прийти из EOF-drain-а — тогда коммитим по нему, а не по fallback-у.
        if !self.seek_eof_video_decoder_drained_for_fallback() {
            return false;
        }

        let Some(fallback_position) = self.seek_runtime.eof_fallback_video_position() else {
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
    /// Paused final seek не включает audio stream, поэтому после clear ack
    /// он не ждёт decoder/output. Final resume в `Playing` ждёт selected audio path:
    /// unsupported/disabled audio должен быть явно снят с selection policy-слоем.
    #[cfg(test)]
    pub(super) fn seek_audio_gate_ready(
        &self,
        seek_commit: SeekCommitState,
        resume_audio_min_buffer_ms: f64,
    ) -> bool {
        self.seek_audio_gate_status(seek_commit, resume_audio_min_buffer_ms)
            .is_ready()
    }

    /// Успешно закрывает seek transaction и применяет сохранённый resume intent.
    pub(super) fn complete_seek_commit(&mut self, seek_commit: SeekCommitState) {
        self.complete_final_seek_commit(seek_commit);
    }

    /// Выбирает committed position без смешивания requested target и EOF fallback frame-а.
    fn final_seek_commit_position(&self, seek_commit: SeekCommitState) -> FinalSeekCommitPosition {
        if let Some(position) = self.final_seek_eof_fallback_commit_position(seek_commit) {
            return FinalSeekCommitPosition::EofFallbackFrame { position };
        }

        if let Some(position) = self.final_seek_presented_frame_commit_position(seek_commit) {
            return FinalSeekCommitPosition::PresentedFrame { position };
        }

        FinalSeekCommitPosition::Target {
            position: seek_commit.target_position.as_duration(),
        }
    }

    /// Explicit keyframe-before seek фиксирует реально показанный frame от demux actual.
    fn final_seek_presented_frame_commit_position(
        &self,
        seek_commit: SeekCommitState,
    ) -> Option<Duration> {
        if seek_commit.drops_decode_preroll_before_target() {
            return None;
        }

        if self.snapshot.timeline.stale_frame {
            return None;
        }

        if !self.pipeline.has_selected_video_track() {
            return None;
        }

        let landing_min_position = seek_commit.landing_frame_min_position();
        self.current_seek_landing_frame_position(seek_commit)
            .filter(|frame_position| *frame_position >= landing_min_position)
    }

    /// EOF fallback считается committed position только если этот frame реально сейчас показан.
    fn final_seek_eof_fallback_commit_position(
        &self,
        seek_commit: SeekCommitState,
    ) -> Option<Duration> {
        if !self.is_eof_draining() {
            return None;
        }

        // Симметрично gate-у: пока decoder может выдать target кадр, не коммитим по fallback-у.
        if !self.seek_eof_video_decoder_drained_for_fallback() {
            return None;
        }

        if !self.pipeline.has_selected_video_track() || self.snapshot.timeline.stale_frame {
            return None;
        }

        let fallback_position = self
            .seek_runtime
            .eof_fallback_video_position()?
            .as_duration();
        if fallback_position >= seek_commit.target_position.as_duration() {
            return None;
        }

        self.pipeline
            .present_video_frame_matches(fallback_position)
            .then_some(fallback_position)
    }

    /// Закрывает финальный seek и публикует новую playback позицию.
    fn complete_final_seek_commit(&mut self, seek_commit: SeekCommitState) {
        if let Err(error) = self.clear_active_seek_decoder_output_floor("seek commit") {
            self.mark_fatal_error(error);
            return;
        }

        let commit_position = self.final_seek_commit_position(seek_commit);
        let playback_position = commit_position.position();

        debug!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            committed_ms = playback_position.as_millis(),
            commit_position_policy = commit_position.policy_name(),
            generation = seek_commit.generation,
            pipeline_generation = self.pipeline.seek_generation(),
            resume_intent = ?seek_commit.resume_intent,
            "Final seek commit завершён"
        );
        self.seek_runtime.clear_active_commit();
        self.clear_prepared_seek_landing_with_diagnostics();
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_seek_landing();
        self.seek_runtime.clear_simple_scrub();
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.stale_frame = false;
        self.snapshot.timeline.preview_state = TimelinePreviewState::Inactive;
        self.pipeline.set_media_clock_base(playback_position);
        self.pipeline.clear_monotonic_media_clock();
        self.publish_position_changed(playback_position);
        self.pending_events
            .push(PlayerEvent::SeekCommitted(SeekCommitInfo {
                target_position: seek_commit.target_position.as_duration(),
                actual_position: seek_commit.actual_position.as_duration(),
                resume_intent: seek_commit.resume_intent,
            }));

        match seek_commit.resume_intent {
            PlaybackResumeIntent::Pause => {
                self.pause_audio_output_for_seek();
                self.set_playback_state(PlaybackState::Paused);
            }
            PlaybackResumeIntent::Play => {
                self.resume_audio_output_after_seek(playback_position);
                let observed_at = Instant::now();
                let audio_now = self.audio_clock_now();
                self.pipeline
                    .reset_audio_clock_sample(audio_now, observed_at);
                self.set_playback_state(PlaybackState::Playing);
                self.anchor_monotonic_media_clock_if_needed(observed_at);
            }
        }

        debug!(
            kind = "seek",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            committed_ms = playback_position.as_millis(),
            commit_position_policy = commit_position.policy_name(),
            generation = seek_commit.generation,
            pipeline_generation = self.pipeline.seek_generation(),
            resume_intent = ?seek_commit.resume_intent,
            playback_state = ?self.snapshot.playback_state,
            "Final seek resume intent applied"
        );
    }

    /// Запускает audio output после seek и различает success/error/absent output.
    fn resume_audio_output_after_seek(&mut self, target_position: Duration) {
        let Some(play_result) = self.pipeline.play_audio_output() else {
            return;
        };

        match play_result {
            Ok(()) => {
                self.pending_events
                    .push(PlayerEvent::AudioResumedAfterSeek(SeekAudioResumeInfo {
                        target_position,
                    }));
            }
            Err(error) => {
                warn!(error = %error, "Не удалось запустить audio после seek");
                let player_error = PlayerError::new(
                    PlayerErrorKind::AudioDeviceUnavailable,
                    format!("Audio play after seek error: {error}"),
                );
                self.record_recoverable_error(player_error);
            }
        }
    }

    /// Прерывает seek transaction по timeout как recoverable error и оставляет media paused.
    fn fail_seek_commit_on_timeout(
        &mut self,
        seek_commit: SeekCommitState,
        timeout_blocker: SeekProgressBlocker,
    ) {
        if self.active_prepared_seek_landing_matches_commit(seek_commit) {
            let audio_gate_status = self.seek_audio_gate_status(seek_commit, 0.0);
            self.fail_prepared_seek_landing_audio_resume_on_timeout(
                seek_commit,
                audio_gate_status,
                Instant::now(),
            );
            return;
        }

        self.fail_final_seek_commit_on_timeout(seek_commit, timeout_blocker);
    }

    /// Прерывает финальный seek transaction по timeout как recoverable error.
    pub(super) fn fail_final_seek_commit_on_timeout(
        &mut self,
        seek_commit: SeekCommitState,
        timeout_blocker: SeekProgressBlocker,
    ) {
        if let Err(error) = self.clear_active_seek_decoder_output_floor("seek timeout") {
            self.mark_fatal_error(error);
            return;
        }

        self.seek_runtime.clear_active_commit();
        self.clear_prepared_seek_landing_with_diagnostics();
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_simple_scrub();
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.preview_state = TimelinePreviewState::Failed;
        // После timeout старый present frame остаётся на экране, но уже не принадлежит
        // закрытому final seek-у. Поэтому fresh можно считать только frame, который
        // действительно покрывает target завершённой transaction.
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame()
            && !self.present_frame_covers_target(seek_commit.target_position.as_duration());
        self.pause_audio_output_for_seek();
        self.set_playback_state(PlaybackState::Paused);

        warn!(
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            generation = seek_commit.generation,
            blocker = timeout_blocker.metric_name(),
            blocker_kind = ?timeout_blocker,
            "Final seek commit остановлен по timeout"
        );

        let error = PlayerError::new(
            PlayerErrorKind::SeekTimeout,
            format!(
                "Seek commit timeout after target={} ms, actual demux={} ms, blocker={}",
                seek_commit.target_position.as_duration().as_millis(),
                seek_commit.actual_position.as_duration().as_millis(),
                timeout_blocker.metric_name()
            ),
        );
        self.record_recoverable_error(error);
    }

    /// Запускает one-shot SeekLanding route для обычной public seek-команды.
    pub(super) fn seek(&mut self, request: SeekRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        let replacing_active_seek_landing = self.seek_runtime.seek_landing_active();
        let resume_intent = self
            .seek_runtime
            .active_seek_landing_resume_intent()
            .unwrap_or_else(|| PlaybackResumeIntent::from_playback_state(self.playback_state()));
        if !replacing_active_seek_landing {
            self.cancel_active_scrub_for_external_command(CancelScrubReason::UserCancelled);
        }
        self.start_one_shot_seek_landing_from_request(request, resume_intent)
    }

    /// Единая S17A точка входа для user-visible seek producers.
    ///
    /// Video route идёт через reused-decoder scrub driver; audio-only route остаётся
    /// single seek transaction без второго video decode-а.
    fn start_one_shot_seek_landing_from_request(
        &mut self,
        request: SeekRequest,
        resume_intent: PlaybackResumeIntent,
    ) -> PlayerResult<()> {
        let target_position = self.resolve_seek_target(request);
        self.pending_events
            .push(PlayerEvent::SeekRequested(request));
        self.seek_runtime.clear_simple_scrub();
        self.snapshot.timeline.scrubbing = false;

        if !self.pipeline.has_demuxer() {
            let error = PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "Seek невозможен: media pipeline ещё не открыт",
            );
            self.record_recoverable_error(error);
            return Ok(());
        }

        if !self.pipeline.has_selected_video_track() {
            return self.start_audio_only_seek_landing_transaction(
                target_position,
                request.mode,
                resume_intent,
            );
        }

        self.start_reused_decoder_scrub_landing_transaction(
            target_position,
            request.mode,
            resume_intent,
            SeekLandingRoute::OneShot,
            None,
            self.frame_server_config,
            FinishScrubPolicy::CommitVisiblePreview,
        )
    }

    /// Выполняет no-video/audio-only SeekLanding через существующую single seek transaction.
    ///
    /// S17A exact landing frame semantics применимы к video route. Для audio-only
    /// нет landing кадра и нет риска второго video decode-а, поэтому используем
    /// текущие typed commit gates без отдельного public command route-а.
    fn start_audio_only_seek_landing_transaction(
        &mut self,
        target_position: MediaTime,
        seek_mode: SeekMode,
        resume_intent: PlaybackResumeIntent,
    ) -> PlayerResult<()> {
        self.start_seek_transaction(target_position, seek_mode, resume_intent)
    }

    /// Запускает S17A SeekLanding через `frame-server-core` scrub driver поверх
    /// текущего playback decoder-а.
    fn start_reused_decoder_scrub_landing_transaction(
        &mut self,
        target_position: MediaTime,
        seek_mode: SeekMode,
        resume_intent: PlaybackResumeIntent,
        route: SeekLandingRoute,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
        config: ValidatedFrameServerConfig,
        finish_policy: FinishScrubPolicy,
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

        let Some(video_track_id) = self.pipeline.selected_video_track_id() else {
            return self.start_audio_only_seek_landing_transaction(
                target_position,
                seek_mode,
                resume_intent,
            );
        };

        if route.is_live_scrub()
            && self.try_extend_active_live_scrub_landing_forward(
                target_position,
                live_scrub_diagnostics,
            )
        {
            return Ok(());
        }

        if let Err(error) =
            demux_seek_request_for_transaction(true, target_position.as_duration(), seek_mode)
        {
            self.record_recoverable_error(player_error_from_seek_demux_request_error(error));
            return Ok(());
        }
        let replacement_generation = self.supersede_active_seek_landing_for_new_target()?;
        let replacing_existing_seek_landing = replacement_generation.is_some();
        let generation_token = match replacement_generation {
            Some(generation) => generation,
            None => {
                let playback_generation = self
                    .pipeline
                    .seek_generation()
                    .checked_add(1)
                    .ok_or_else(|| {
                        PlayerError::new(
                            PlayerErrorKind::RuntimeError,
                            "Seek generation overflow would break SeekLanding stale-frame guards",
                        )
                    })?;
                seek_landing_generation_token(
                    playback_generation,
                    ScrubGeneration::new(SEEK_LANDING_FIRST_SCRUB_GENERATION),
                )
            }
        };
        let generation = generation_token.playback_generation.get();
        let target_scrub_generation = generation_token.scrub_generation;

        self.pause_audio_output_for_seek();
        self.seek_runtime.begin_seek_landing_request(
            generation_token,
            seek_mode,
            resume_intent,
            route,
        );
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.pipeline
            .reset_clocks_for_seek(target_position.as_duration());

        let target = ScrubTarget::new(
            target_position,
            self.seek_landing_target_pts(video_track_id, target_position),
        );
        let track_selection = match self.pipeline.selected_audio_track_id() {
            Some(audio_track_id) => ScrubTrackSelection::with_audio(video_track_id, audio_track_id),
            None => ScrubTrackSelection::video_only(video_track_id),
        };
        self.confirm_prepared_seek_landing_unavailable(
            target_position,
            seek_mode,
            resume_intent,
            generation_token,
            track_selection,
            target,
            route.request_kind(),
            route.commit_allowed(),
            live_scrub_diagnostics,
        )?;

        self.enter_seek_landing_public_scrubbing(target_position);

        let update = ScrubTargetUpdate::new(
            scrub_update_guards_for_owner(
                SEEK_LANDING_SOURCE_REVISION_UNTRACKED,
                SEEK_LANDING_BACKEND_REVISION_UNTRACKED,
                generation,
            ),
            track_selection,
            target,
            ScrubExactnessPolicy::ExactFrame,
            route.request_kind(),
            default_scrub_execution_policy(config, finish_policy),
        );
        let initial_scrub_generation =
            initial_scrub_generation_before_target(target_scrub_generation)?;
        let mut driver = PlayerScrubTransactionDriver::new(config, initial_scrub_generation);
        let run = driver.submit_target_update(self, update);
        for event in run.events {
            self.push_scrub_event_with_live_diagnostics(event, live_scrub_diagnostics);
        }

        if self.seek_landing_decode_active() {
            return Ok(());
        }

        if replacing_existing_seek_landing {
            self.invalidate_in_flight_scrub_outputs_after_exit("seek landing replacement failed");
        }
        self.seek_runtime.clear_active_commit();
        self.clear_prepared_seek_landing_with_diagnostics();
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_seek_landing();
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.stale_frame = false;
        self.snapshot.timeline.preview_state = TimelinePreviewState::Failed;
        self.set_playback_state(PlaybackState::Paused);
        if self.snapshot.last_error.is_none() {
            self.record_recoverable_error(PlayerError::new(
                PlayerErrorKind::SeekUnavailable,
                "SeekLanding не смог стартовать reused-decoder scrub route",
            ));
        }
        Ok(())
    }

    /// Продолжает активный live scrub проход к более поздней цели без нового cold seek.
    ///
    /// Реюз позиции декодера: движение вперёд в пределах капа не делает flush
    /// декодера и demux seek на keyframe-before — уже идущий decode-проход просто
    /// доезжает до новой цели, а прокат показывает кадры по пути. Generation,
    /// pending video packets и очередь кадров остаются валидными. Audio runtime
    /// сбрасывается под новый target: уже декодированный PCM от старой цели
    /// очищается, clock base перепривязывается, и trace начинается заново, чтобы
    /// landing новой цели снова опубликовал `SeekTargetFramePresented`.
    ///
    /// Возвращает `true`, если расширение применено и cold route не нужен.
    fn try_extend_active_live_scrub_landing_forward(
        &mut self,
        target_position: MediaTime,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) -> bool {
        let Some(seek_commit) = self.seek_runtime.active_commit() else {
            return false;
        };

        // Расширять можно только активный live scrub decode-проход до EndScrub:
        // после запрошенного commit-а целью владеет release-логика.
        if !self.seek_runtime.active_seek_landing_is_live_scrub()
            || !self.seek_runtime.seek_landing_decode_active()
            || self.seek_runtime.active_seek_landing_commit_allowed()
        {
            return false;
        }

        // Accurate-политика проката обязана быть активной, иначе окажемся в
        // расширении неподдерживаемого маршрута.
        if !seek_commit.drops_decode_preroll_before_target() {
            return false;
        }

        // EOF drain: demuxer уже не читает — новая цель обязана пройти cold route,
        // который заново seek-ает demuxer и выходит из drain-а.
        if self.is_eof_draining() {
            return false;
        }

        let current_target = seek_commit.target_position.as_duration();
        let new_target = target_position.as_duration();
        if new_target < current_target {
            return false;
        }
        if new_target.saturating_sub(current_target) > LIVE_SCRUB_FORWARD_EXTENSION_MAX {
            return false;
        }

        // Audio под новый target: буфер от старой цели очищается, decoder
        // сбрасывается, clock base перепривязывается. Video decoder не трогаем.
        self.reset_audio_runtime_for_seek_landing(seek_commit.generation);
        self.pipeline.reset_clocks_for_seek(new_target);

        self.seek_runtime.set_active_commit(SeekCommitState {
            target_position,
            started_at: Instant::now(),
            ..seek_commit
        });

        // Trace заново: landing новой цели снова должен эмитить
        // `SeekTargetFramePresented`, иначе UI completion-gate ждёт fallback budget.
        self.seek_runtime.clear_trace();
        self.seek_runtime.begin_trace(seek_commit.generation);
        self.seek_runtime
            .update_active_live_scrub_diagnostics(live_scrub_diagnostics);

        self.enter_seek_landing_public_scrubbing(target_position);

        debug!(
            kind = "seek",
            old_target_ms = current_target.as_millis(),
            new_target_ms = new_target.as_millis(),
            generation = seek_commit.generation,
            "Live scrub: цель расширена вперёд без нового cold seek"
        );
        true
    }

    /// Переводит только public state/snapshot в pending SeekLanding.
    ///
    /// Lifecycle шаги seek generation, queues, clocks и promoted ownership остаются
    /// у вызывающего route-а, чтобы prepared instant commit не публиковал Scrubbing.
    pub(super) fn enter_seek_landing_public_scrubbing(&mut self, target_position: MediaTime) {
        self.set_playback_state(PlaybackState::Scrubbing);
        self.snapshot.timeline.target_position = Some(target_position);
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = true;
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame();
        self.snapshot.timeline.preview_state = TimelinePreviewState::Pending;
    }

    /// Строит target PTS в timebase выбранного video track-а для neutral scrub events.
    pub(super) fn seek_landing_target_pts(
        &self,
        video_track_id: crate::TrackId,
        target_position: MediaTime,
    ) -> TrackTimestamp {
        let time_base = self
            .pipeline
            .tracks()
            .iter()
            .find(|track| track.id == video_track_id && track.kind == TrackKind::Video)
            .and_then(|track| track.time_base)
            .unwrap_or_else(|| TimeBase::new(1, 1_000).expect("valid millisecond timebase"));
        let units = time_base.duration_to_track_units_saturating(MediaDuration::from_duration(
            target_position.as_duration(),
        ));
        TrackTimestamp::new(video_track_id, units.get(), time_base)
    }

    /// Начинает timeline scrub gesture на уровне public state без немедленного commit-а.
    pub(super) fn begin_scrub(
        &mut self,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        let confirmed_playback_state = self.playback_state_for_new_simple_scrub();
        self.enter_simple_scrub_public_state();
        self.seek_runtime
            .begin_simple_scrub(confirmed_playback_state, live_scrub_diagnostics);
        self.snapshot.timeline.scrubbing = true;
        self.snapshot.timeline.stale_frame = false;
        self.snapshot.timeline.target_position = Some(self.snapshot.timeline.current_position);
        Ok(())
    }

    /// Запоминает последнюю цель scrub без изменения текущей playback позиции.
    pub(super) fn update_scrub(&mut self, request: SeekRequest) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.store_simple_scrub_request(request, None);
        Ok(())
    }

    /// Сохраняет preview request и запускает live reused-decoder route для timeline drag.
    pub(super) fn preview_scrub(
        &mut self,
        request: SeekRequest,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        let target_position = self.resolve_seek_target(request);
        let confirmed_playback_state = self
            .seek_runtime
            .simple_scrub_confirmed_playback_state()
            .unwrap_or_else(|| self.playback_state_for_new_simple_scrub());
        let resume_intent = PlaybackResumeIntent::from_playback_state(confirmed_playback_state);
        self.store_simple_scrub_request(request, live_scrub_diagnostics);
        let live_scrub_diagnostics =
            live_scrub_diagnostics.or_else(|| self.seek_runtime.simple_scrub_live_diagnostics());

        if !self.pipeline.has_demuxer() || !self.pipeline.has_selected_video_track() {
            return Ok(());
        }

        self.start_reused_decoder_scrub_landing_transaction(
            target_position,
            request.mode,
            resume_intent,
            SeekLandingRoute::live_scrub_preview(live_scrub_diagnostics),
            live_scrub_diagnostics,
            self.frame_server_config,
            FinishScrubPolicy::CommitVisiblePreview,
        )?;
        Ok(())
    }

    /// Запоминает latest scrub target и переводит timeline в public scrubbing state.
    fn store_simple_scrub_request(
        &mut self,
        request: SeekRequest,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) {
        let target_position = self.resolve_seek_target(request);
        let confirmed_playback_state = self.playback_state_for_new_simple_scrub();
        self.enter_simple_scrub_public_state();
        self.seek_runtime.store_simple_scrub_request(
            request,
            confirmed_playback_state,
            live_scrub_diagnostics,
        );
        self.snapshot.timeline.scrubbing = true;
        self.snapshot.timeline.target_position = Some(target_position);
        if !self.snapshot.timeline.seeking {
            self.snapshot.timeline.stale_frame = false;
        }
    }

    /// Завершает compatibility scrub и передаёт latest target в единый SeekLanding route.
    pub(super) fn end_scrub(
        &mut self,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.seek_runtime
            .update_active_live_scrub_diagnostics(live_scrub_diagnostics);
        if !self.seek_runtime.simple_scrub_active() {
            self.finish_simple_scrub_without_seek(None, SimpleScrubExitMode::RestoreStateOnly);
            return Ok(());
        }

        let finished_scrub = self
            .seek_runtime
            .finish_active_simple_scrub()
            .expect("simple scrub active должен вернуть finished state");
        let confirmed_playback_state = finished_scrub.confirmed_playback_state();
        let latest_request = finished_scrub.latest_request();
        let live_scrub_diagnostics =
            live_scrub_diagnostics.or_else(|| finished_scrub.live_scrub_diagnostics());

        if self.seek_runtime.active_seek_landing_is_live_scrub() {
            self.seek_runtime
                .update_active_live_scrub_diagnostics(live_scrub_diagnostics);
            self.seek_runtime.request_live_scrub_commit(Instant::now());
            debug!(
                kind = "seek",
                target_ms = latest_request
                    .map(|request| self.resolve_seek_target(request).as_duration().as_millis()),
                "EndScrub: live scrub commit запрошен"
            );
            self.snapshot.timeline.scrubbing = true;
            if let Some(request) = latest_request {
                self.snapshot.timeline.target_position = Some(self.resolve_seek_target(request));
            }
            return Ok(());
        }

        self.invalidate_in_flight_scrub_outputs_after_exit("end scrub");
        if latest_request.is_none() {
            self.finish_simple_scrub_without_seek(
                Some(confirmed_playback_state),
                SimpleScrubExitMode::ResumeConfirmedPlayback,
            );
            return Ok(());
        }

        self.finish_simple_scrub_without_seek(None, SimpleScrubExitMode::RestoreStateOnly);
        let request = latest_request.expect("checked latest_request is_some");
        let resume_intent = PlaybackResumeIntent::from_playback_state(confirmed_playback_state);
        self.start_one_shot_seek_landing_from_request(request, resume_intent)?;
        Ok(())
    }

    /// Сбрасывает только lightweight scrub-флаги, не трогая текущий active seek.
    fn finish_simple_scrub_without_seek(
        &mut self,
        confirmed_playback_state: Option<PlaybackState>,
        exit_mode: SimpleScrubExitMode,
    ) {
        self.seek_runtime.clear_simple_scrub();
        self.snapshot.timeline.scrubbing = false;
        if !self.snapshot.timeline.seeking {
            self.snapshot.timeline.target_position = None;
            self.snapshot.timeline.stale_frame = false;
        }
        if let Some(playback_state) = confirmed_playback_state {
            self.set_playback_state(playback_state);
            if exit_mode == SimpleScrubExitMode::ResumeConfirmedPlayback
                && playback_state == PlaybackState::Playing
            {
                self.resume_audio_and_clock_after_simple_scrub();
            }
        }
    }

    /// Заменяет active SeekLanding новым target-ом без user-cancel semantics.
    fn supersede_active_seek_landing_for_new_target(
        &mut self,
    ) -> PlayerResult<Option<ScrubGenerationToken>> {
        if !self.seek_runtime.seek_landing_active() {
            return Ok(None);
        }

        let Some(next_scrub_generation) = self
            .seek_runtime
            .next_seek_landing_scrub_generation_after_supersede()
        else {
            return Err(PlayerError::new(
                PlayerErrorKind::RuntimeError,
                "Nested scrub generation overflow would break SeekLanding replacement guards",
            ));
        };
        let Some(playback_generation) = self.seek_runtime.seek_landing_playback_generation() else {
            return Err(PlayerError::new(
                PlayerErrorKind::RuntimeError,
                "Active SeekLanding is missing scrub playback generation",
            ));
        };
        let next_generation = ScrubGenerationToken::new(playback_generation, next_scrub_generation);
        let release_prepared_ownership_for_cancel =
            !self.seek_runtime.active_seek_landing_is_live_scrub();
        let active_live_scrub_diagnostics =
            self.seek_runtime.active_seek_landing_live_diagnostics();
        let active_context = self
            .seek_runtime
            .active_commit()
            .and_then(|seek_commit| self.active_seek_landing_context(seek_commit));

        debug!(
            target_position = ?self.snapshot.timeline.target_position,
            next_scrub_generation = next_scrub_generation.get(),
            "Superseding active SeekLanding with a new target"
        );

        if let Err(error) = self.clear_active_seek_decoder_output_floor("seek landing superseded") {
            self.mark_fatal_error(error.clone());
            return Err(error);
        }

        if let Some(context) = active_context {
            self.push_scrub_event_with_live_diagnostics(
                ScrubEvent::from_driver_outcome(ScrubDriverOutcome::Cancelled(CancelledOutcome {
                    context,
                    reason: CancelScrubReason::SupersededByNewTarget,
                })),
                active_live_scrub_diagnostics,
            );
        }

        let release_context = release_prepared_ownership_for_cancel
            .then_some(active_context)
            .flatten();
        let _release_outcome = self.release_prepared_seek_landing_for_cancel(
            CancelScrubReason::SupersededByNewTarget,
            release_context,
        );
        self.seek_runtime.clear_active_commit();
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_seek_landing();
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.pipeline.clear_pending_packets_for_seek();
        self.clear_queued_video_frames();

        Ok(Some(next_generation))
    }

    /// Закрывает live scrub перед внешней командой без commit-а latest target-а.
    pub(super) fn cancel_active_scrub_for_external_command(&mut self, reason: CancelScrubReason) {
        if self.seek_runtime.seek_landing_active() {
            self.cancel_active_seek_landing_for_external_command(reason);
        }

        let Some(finished_scrub) = self.seek_runtime.finish_active_simple_scrub() else {
            return;
        };
        self.invalidate_in_flight_scrub_outputs_after_exit("external command cancel");
        let confirmed_playback_state = finished_scrub.confirmed_playback_state();
        let latest_request = finished_scrub.latest_request();
        debug!(
            reason = ?reason,
            latest_request = ?latest_request,
            confirmed_playback_state = ?confirmed_playback_state,
            "Cancelling active scrub before external command"
        );
        self.finish_simple_scrub_without_seek(
            Some(confirmed_playback_state),
            SimpleScrubExitMode::RestoreStateOnly,
        );
    }

    /// Закрывает active S17A SeekLanding без commit-а uncommitted landing target-а.
    fn cancel_active_seek_landing_for_external_command(&mut self, reason: CancelScrubReason) {
        let resume_intent = self
            .seek_runtime
            .active_seek_landing_resume_intent()
            .unwrap_or(PlaybackResumeIntent::Pause);
        debug!(
            reason = ?reason,
            resume_intent = ?resume_intent,
            target_position = ?self.snapshot.timeline.target_position,
            "Cancelling active SeekLanding before external command"
        );
        let active_context = self
            .seek_runtime
            .active_commit()
            .and_then(|seek_commit| self.active_seek_landing_context(seek_commit));
        if let Err(error) = self.clear_active_seek_decoder_output_floor("seek landing cancel") {
            self.mark_fatal_error(error);
            return;
        }

        self.invalidate_in_flight_scrub_outputs_after_exit("seek landing external command cancel");
        self.seek_runtime.clear_active_commit();
        let _release_outcome =
            self.release_prepared_seek_landing_for_cancel(reason, active_context);
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_seek_landing();
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.pipeline.clear_pending_packets_for_seek();
        self.clear_queued_video_frames();
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame();
        self.snapshot.timeline.preview_state = TimelinePreviewState::Inactive;

        match resume_intent {
            PlaybackResumeIntent::Pause => {
                self.pause_audio_output_for_seek();
                self.set_playback_state(PlaybackState::Paused);
            }
            PlaybackResumeIntent::Play => {
                self.set_playback_state(PlaybackState::Playing);
            }
        }
    }

    /// Делает все in-flight scrub packets/frames/readiness старым playback generation.
    pub(super) fn invalidate_in_flight_scrub_outputs_after_exit(
        &mut self,
        exit_reason: &'static str,
    ) {
        let generation = self.pipeline.begin_seek_generation();
        self.clear_seek_preroll_fallback_frame();
        self.clear_queued_video_frames();
        debug!(
            exit_reason,
            generation, "Active Scrubbing exit advanced playback generation"
        );
    }

    /// Входит в public `Scrubbing` и замораживает playback-owned audio/clock только один раз.
    fn enter_simple_scrub_public_state(&mut self) {
        if !self.seek_runtime.simple_scrub_active() {
            self.clear_monotonic_media_clock_anchor(Instant::now());
            self.pause_audio_output_for_seek();
        }
        self.set_playback_state(PlaybackState::Scrubbing);
    }

    /// Возобновляет слышимый playback, если scrub release не запускает seek/command route.
    fn resume_audio_and_clock_after_simple_scrub(&mut self) {
        if let Some(play_result) = self.pipeline.play_audio_output() {
            if let Err(error) = play_result {
                warn!(error = %error, "Не удалось запустить audio после scrub");
                self.set_runtime_error(format!("Audio play after scrub error: {error}"));
            }
            let observed_at = Instant::now();
            let audio_now = self.audio_clock_now();
            self.pipeline
                .reset_audio_clock_sample(audio_now, observed_at);
        }
        self.anchor_monotonic_media_clock_if_needed(Instant::now());
    }

    /// Возвращает state, от которого cancel-first команды должны продолжать route.
    fn playback_state_for_new_simple_scrub(&self) -> PlaybackState {
        match self.playback_state() {
            PlaybackState::Scrubbing => PlaybackState::Paused,
            playback_state => playback_state,
        }
    }

    /// Проверяет, что текущий video frame уже соответствует target; audio-only media проходит.
    pub(super) fn present_frame_covers_target(&self, target_position: Duration) -> bool {
        if !self.pipeline.has_selected_video_track() {
            return true;
        }

        self.pipeline.present_video_frame_covers(target_position)
    }

    /// Сбрасывает video decoder перед seek и делает flush явной fail-fast границей.
    pub(super) fn reset_video_decoder_for_seek(&self) -> Result<(), PlayerError> {
        self.pipeline
            .flush_video_decoder_thread()
            .map_err(player_error_from_decoder_flush_error)
    }

    /// Завершает seek transaction до demux seek, если decoder не подтвердил flush.
    fn fail_seek_transaction_on_decoder_flush(&mut self, error: PlayerError) {
        self.seek_runtime.clear_active_commit();
        self.clear_prepared_seek_landing_with_diagnostics();
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.stale_frame = true;
        self.snapshot.timeline.target_position = None;
        self.seek_runtime.clear_simple_scrub();
        self.snapshot.timeline.scrubbing = false;
        self.set_playback_state(PlaybackState::Paused);
        self.record_recoverable_error(error);
    }

    /// Выполняет синхронную часть seek transaction и оставляет commit gates на tick.
    pub(super) fn start_seek_transaction(
        &mut self,
        target_position: MediaTime,
        seek_mode: SeekMode,
        resume_intent: PlaybackResumeIntent,
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
            self.pipeline.has_selected_video_track(),
            target_duration,
            seek_mode,
        ) {
            Ok(request) => request,
            Err(error) => {
                self.record_recoverable_error(player_error_from_seek_demux_request_error(error));
                return Ok(());
            }
        };

        if let Err(error) = self.clear_active_seek_decoder_output_floor("new seek") {
            self.mark_fatal_error(error);
            return Ok(());
        }

        self.pause_audio_output_for_seek();
        if let Err(error) = self.reset_video_decoder_for_seek() {
            self.fail_seek_transaction_on_decoder_flush(error);
            return Ok(());
        }

        self.set_playback_state(PlaybackState::Seeking);
        let generation = self.pipeline.begin_seek_generation();
        let has_video = self.pipeline.has_selected_video_track();

        self.pipeline.clear_pending_packets_for_seek();
        self.pipeline.reset_decoder_state_for_seek(has_video);
        if has_video {
            self.record_video_decoder_bootstrap_started();
        }
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.clear_queued_video_frames();
        self.pipeline.reset_clocks_for_seek(target_duration);
        self.snapshot.timeline.target_position = Some(target_position);
        self.snapshot.timeline.seeking = true;
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame();

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
                kind = "seek",
                target_ms = target_duration.as_millis(),
                demux_mode = ?demux_seek_request.mode,
                generation,
                selected_video_track_id = ?self.pipeline.selected_video_track_id(),
                selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
                "Starting demux seek transaction"
            );
            let Some(seek_result) = self.pipeline.seek_demuxer(demux_seek_request) else {
                self.seek_runtime.clear_trace();
                return Ok(());
            };
            seek_result
        };

        match seek_result {
            Ok(result) => {
                debug!(
                    kind = "seek",
                    target_ms = target_duration.as_millis(),
                    actual_ms = result.actual_position.as_duration().as_millis(),
                    actual_track_timestamp = ?result.actual_track_timestamp,
                    demux_mode = ?demux_seek_request.mode,
                    generation,
                    pipeline_generation = self.pipeline.seek_generation(),
                    selected_video_track_id = ?self.pipeline.selected_video_track_id(),
                    selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
                    "Demux seek transaction accepted"
                );
                self.seek_runtime.begin_trace(generation);
                let seek_commit = SeekCommitState {
                    generation,
                    seek_mode,
                    target_position,
                    actual_position: result.actual_position,
                    started_at: Instant::now(),
                    resume_intent,
                };
                self.reanchor_clocks_after_seek_accept(seek_commit);
                self.seek_runtime.set_active_commit(seek_commit);
                if let Err(error) = self.apply_decoder_output_floor_for_seek(seek_commit) {
                    self.seek_runtime.clear_active_commit();
                    self.clear_prepared_seek_landing_with_diagnostics();
                    self.seek_runtime.clear_trace();
                    self.seek_runtime.clear_simple_scrub();
                    self.seek_runtime.clear_eof_fallback_video_position();
                    self.clear_seek_preroll_fallback_frame();
                    self.mark_fatal_error(error);
                    return Ok(());
                }
                Ok(())
            }
            Err(error) => {
                self.seek_runtime.clear_active_commit();
                self.clear_prepared_seek_landing_with_diagnostics();
                self.seek_runtime.clear_trace();
                self.seek_runtime.clear_simple_scrub();
                self.snapshot.timeline.scrubbing = false;
                self.snapshot.timeline.seeking = false;
                self.snapshot.timeline.stale_frame = false;
                self.set_playback_state(PlaybackState::Paused);
                self.snapshot.timeline.target_position = None;
                let player_error = player_error_from_demux_seek_error(error);
                self.record_recoverable_error(player_error);
                Ok(())
            }
        }
    }

    /// Останавливает audio stream для seek, не меняя high-level playback state.
    pub(super) fn pause_audio_output_for_seek(&mut self) {
        if let Some(Err(error)) = self.pipeline.pause_audio_output_and_capture_clock() {
            warn!(error = %error, "Не удалось остановить audio перед seek");
            self.set_runtime_error(format!("Audio pause before seek error: {error}"));
        }
    }

    /// Возвращает active simple scrub flag для focused tests.
    #[cfg(test)]
    pub(super) const fn simple_scrub_active_for_tests(&self) -> bool {
        self.seek_runtime.simple_scrub_active()
    }

    /// Возвращает latest simple scrub request для focused tests.
    #[cfg(test)]
    pub(super) const fn simple_scrub_latest_request_for_tests(&self) -> Option<SeekRequest> {
        self.seek_runtime.simple_scrub_latest_request()
    }

    /// Устанавливает simple scrub state напрямую только для focused boundary tests.
    #[cfg(test)]
    pub(super) fn set_simple_scrub_state_for_tests(
        &mut self,
        active: bool,
        latest_request: Option<SeekRequest>,
    ) {
        self.seek_runtime
            .set_simple_scrub_state_for_tests(active, latest_request);
    }

    /// Устанавливает active seek commit напрямую только для focused boundary tests.
    #[cfg(test)]
    pub(super) fn set_seek_commit_for_tests(&mut self, seek_commit: Option<SeekCommitState>) {
        match seek_commit {
            Some(seek_commit) => self.seek_runtime.set_active_commit(seek_commit),
            None => {
                self.seek_runtime.clear_active_commit();
                self.clear_prepared_seek_landing_with_diagnostics();
            }
        }
    }

    /// Открывает trace markers напрямую только для focused boundary tests.
    #[cfg(test)]
    pub(super) fn begin_seek_trace_for_tests(&mut self, generation: u64) {
        self.seek_runtime.begin_trace(generation);
    }

    /// Помечает decoder-side output-floor applied напрямую только для decoder I/O tests.
    #[cfg(test)]
    pub(super) fn mark_decoder_output_floor_applied_for_tests(
        &mut self,
        generation: u64,
        floor_pts: Duration,
    ) {
        self.seek_runtime
            .mark_decoder_output_floor_applied(generation, floor_pts);
    }

    /// Устанавливает EOF fallback marker напрямую только для focused boundary tests.
    #[cfg(test)]
    pub(super) fn set_seek_eof_fallback_video_position_for_tests(
        &mut self,
        position: Option<MediaTime>,
    ) {
        match position {
            Some(position) => self.seek_runtime.set_eof_fallback_video_position(position),
            None => self.seek_runtime.clear_eof_fallback_video_position(),
        }
    }

    /// Возвращает EOF fallback marker для focused boundary tests.
    #[cfg(test)]
    pub(super) const fn seek_eof_fallback_video_position_for_tests(&self) -> Option<MediaTime> {
        self.seek_runtime.eof_fallback_video_position()
    }
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

/// Возвращает stable label для resume intent-а после seek.
fn playback_resume_intent_name(intent: PlaybackResumeIntent) -> &'static str {
    match intent {
        PlaybackResumeIntent::Pause => "pause",
        PlaybackResumeIntent::Play => "play",
    }
}
