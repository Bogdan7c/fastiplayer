//! Read-only вычисления commit gates активной seek-транзакции.
use super::PlayerSession;
use super::audio_runtime::SeekAudioGateStatus;
use crate::seek_state::{PlaybackResumeIntent, SeekCommitState};
use crate::{
    PipelineQueueDepthSnapshot, PlayerTickConfig, SeekBootstrapDiagnosticsSnapshot,
    SeekProgressBlocker,
};
use std::time::{Duration, Instant};
#[derive(Debug, Clone, Copy)]
pub(super) struct SeekProgressGateSnapshot {
    pub(super) target_frame_presented: bool,
    pub(super) video_gate_ready: bool,
    pub(super) audio_gate_status: SeekAudioGateStatus,
    pub(super) ready_video_frames: usize,
    pub(super) required_video_frames: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SeekCommitGateDecision {
    Waiting,
    Ready,
}
impl SeekCommitGateDecision {
    pub(super) const fn allows_commit(self) -> bool {
        matches!(self, Self::Ready)
    }
}
impl PlayerSession {
    /// Разрешает commit только после готовности target video и выбранного audio runtime.
    pub(super) fn seek_commit_gate_decision(
        &self,
        seek_commit: SeekCommitState,
        resume_audio_min_buffer_ms: f64,
        resume_video_min_ready_frames: usize,
    ) -> SeekCommitGateDecision {
        if !self.seek_video_gate_ready(seek_commit, resume_video_min_ready_frames) {
            return SeekCommitGateDecision::Waiting;
        }

        if self
            .seek_audio_gate_status(seek_commit, resume_audio_min_buffer_ms)
            .is_ready()
        {
            return SeekCommitGateDecision::Ready;
        }

        SeekCommitGateDecision::Waiting
    }

    /// Берёт текущий blocker из active seek diagnostics до очистки timeout-состояния.
    pub(super) fn seek_timeout_blocker_from_active_diagnostics(
        &self,
        now: Instant,
        tick_config: &PlayerTickConfig,
    ) -> SeekProgressBlocker {
        self.active_seek_diagnostics(now, tick_config)
            .map(|diagnostics| diagnostics.blocker)
            .unwrap_or(SeekProgressBlocker::Unknown)
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
    pub(super) fn seek_presented_frame_ready(&self, seek_commit: SeekCommitState) -> bool {
        if self.final_seek_visible_frame_ready(seek_commit) {
            return true;
        }

        let target_position = seek_commit.landing_frame_min_position();
        self.current_seek_landing_frame_position(seek_commit)
            .is_some_and(|frame_position| frame_position >= target_position)
    }

    /// Проверяет, что обычный final seek уже показал свежий frame текущего generation-а.
    pub(super) fn final_seek_visible_frame_ready(&self, seek_commit: SeekCommitState) -> bool {
        self.current_seek_landing_frame_position(seek_commit)
            .is_some()
    }

    /// Проверяет текущий present frame как landing point активного seek-а.
    pub(super) fn current_seek_landing_frame_position(
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
    pub(super) fn seek_landing_frame_matches_active_commit(
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
    pub(super) fn seek_ready_video_frame_count(&self, seek_commit: SeekCommitState) -> usize {
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
    pub(super) fn seek_eof_video_decoder_drained_for_fallback(&self) -> bool {
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
}
