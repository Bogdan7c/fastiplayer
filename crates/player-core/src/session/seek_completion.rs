use std::time::{Duration, Instant};

use media_core::{TimelinePreviewState, TimelineRange, TrackKind};
use tracing::{debug, info, warn};

use crate::seek_state::{FinalSeekCommitPosition, PlaybackResumeIntent, SeekCommitState};
use crate::{
    PlaybackState, PlayerError, PlayerErrorKind, PlayerEvent, SeekAudioResumeInfo, SeekCommitInfo,
    SeekProgressBlocker,
};

use super::PlayerSession;

impl PlayerSession {
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

        if seek_commit.presents_from_actual_position() {
            return FinalSeekCommitPosition::AuthoritativeActual {
                position: seek_commit.actual_position.as_duration(),
            };
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
        if seek_commit.drops_decode_preroll_before_target()
            && !seek_commit.presents_from_actual_position()
        {
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
        if seek_commit.presents_from_actual_position() {
            return None;
        }
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
        let presented_pre_target_frames = self
            .seek_runtime
            .seek_commit_presentation_evidence(seek_commit.generation);
        let available_audio_track_count = self
            .pipeline
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Audio)
            .count();

        if seek_commit.resume_intent == PlaybackResumeIntent::Play
            && let Err(error) =
                self.resume_audio_output_before_seek_commit(seek_commit, playback_position)
        {
            self.fail_final_seek_commit_after_audio_resume_error(seek_commit, error);
            return;
        }

        let committed_at = Instant::now();
        let public_to_commit_ms = committed_at
            .saturating_duration_since(seek_commit.public_accepted_at)
            .as_millis();
        let receipt_to_commit_ms = committed_at
            .saturating_duration_since(seek_commit.started_at)
            .as_millis();

        info!(
            kind = "seek_acceptance",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            committed_ms = playback_position.as_millis(),
            commit_position_policy = commit_position.policy_name(),
            generation = seek_commit.generation,
            pipeline_generation = self.pipeline.seek_generation(),
            media_instance_id = self.snapshot.media_instance_id.map(|identity| identity.get()),
            selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
            available_audio_track_count,
            presented_pre_target_frames,
            seek_elapsed_ms = public_to_commit_ms,
            public_to_commit_ms,
            receipt_to_commit_ms,
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
        self.complete_pending_seek_receipts(playback_position.into(), seek_commit.generation);
        self.push_player_event(PlayerEvent::SeekCommitted(SeekCommitInfo {
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
                let observed_at = Instant::now();
                let audio_now = self.audio_clock_now();
                self.pipeline
                    .reset_audio_clock_sample(audio_now, observed_at);
                self.set_playback_state(PlaybackState::Playing);
                self.anchor_monotonic_media_clock_if_needed(observed_at);
                self.seek_runtime.arm_post_commit_position_progress(
                    seek_commit,
                    playback_position,
                    observed_at,
                );
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

    /// Запускает выбранное audio до public commit-а; media без audio проходит без output-а.
    fn resume_audio_output_before_seek_commit(
        &mut self,
        seek_commit: SeekCommitState,
        playback_position: Duration,
    ) -> Result<(), PlayerError> {
        if !self.pipeline.has_selected_audio_track() {
            return Ok(());
        }

        let Some(play_result) = self.play_audio_output_with_resume_event() else {
            return Err(PlayerError::new(
                PlayerErrorKind::AudioDeviceUnavailable,
                "Selected audio output отсутствует перед final seek commit",
            ));
        };
        play_result.map_err(|error| {
            PlayerError::new(
                PlayerErrorKind::AudioDeviceUnavailable,
                format!("Audio play after seek error: {error}"),
            )
        })?;

        let accepted_at = Instant::now();
        info!(
            kind = "seek_acceptance",
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            playback_position_ms = playback_position.as_millis(),
            generation = seek_commit.generation,
            media_instance_id = self.snapshot.media_instance_id.map(|identity| identity.get()),
            selected_audio_track_id = ?self.pipeline.selected_audio_track_id(),
            audio_ready = true,
            audio_buffer_level_ms = self.audio_buffer_level_ms(),
            seek_elapsed_ms = accepted_at
                .saturating_duration_since(seek_commit.public_accepted_at)
                .as_millis(),
            public_to_audio_ms = accepted_at
                .saturating_duration_since(seek_commit.public_accepted_at)
                .as_millis(),
            receipt_to_audio_ms = accepted_at
                .saturating_duration_since(seek_commit.started_at)
                .as_millis(),
            accepted_after_ms = accepted_at
                .saturating_duration_since(seek_commit.started_at)
                .as_millis(),
            "Audio play accepted before final seek commit"
        );
        self.push_player_event(PlayerEvent::AudioResumedAfterSeek(SeekAudioResumeInfo {
            target_position: seek_commit.target_position.as_duration(),
            playback_position,
        }));
        Ok(())
    }

    /// Закрывает final seek без position/commit success, если audio device не стартовал.
    fn fail_final_seek_commit_after_audio_resume_error(
        &mut self,
        seek_commit: SeekCommitState,
        error: PlayerError,
    ) {
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
        self.snapshot.timeline.preview_state = TimelinePreviewState::Failed;
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame();
        self.pause_audio_output_for_seek();
        self.set_playback_state(PlaybackState::Paused);

        warn!(
            error = %error,
            target_ms = seek_commit.target_position.as_duration().as_millis(),
            actual_ms = seek_commit.actual_position.as_duration().as_millis(),
            generation = seek_commit.generation,
            "Final seek commit отклонён: audio output не возобновился"
        );
        self.fail_pending_seek_receipts(error.clone());
        self.record_recoverable_error(error);
    }

    /// Прерывает seek transaction по timeout как recoverable error и оставляет media paused.
    pub(super) fn fail_seek_commit_on_timeout(
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
        self.fail_pending_seek_receipts(error.clone());
        self.record_recoverable_error(error);
    }

    /// Прерывает pending live seek, когда authoritative DVR window уже не содержит target.
    pub(super) fn fail_dynamic_seek_target_expired(
        &mut self,
        seek_commit: SeekCommitState,
        available_range: Option<TimelineRange>,
    ) {
        if let Err(error) = self.clear_active_seek_decoder_output_floor("dynamic seek expiry") {
            self.mark_fatal_error(error);
            return;
        }

        self.expire_pending_exact_timeline_seek(available_range);
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
        self.snapshot.timeline.preview_state = TimelinePreviewState::Failed;
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame();
        self.pause_audio_output_for_seek();
        self.set_playback_state(PlaybackState::Paused);

        let error = PlayerError::new(
            PlayerErrorKind::SeekTargetExpired,
            format!(
                "Live seek target {} ms expired outside latest DVR range {:?}",
                seek_commit.target_position.as_duration().as_millis(),
                available_range
            ),
        );
        self.fail_pending_seek_receipts(error.clone());
        self.record_recoverable_error(error);
    }
}
