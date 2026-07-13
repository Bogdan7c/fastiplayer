use std::time::{Duration, Instant};

use frame_server_core::{ScrubFrameTiming, ScrubRequestKind};
use media_core::MediaTime;
use tracing::{debug, trace, warn};

#[cfg(test)]
use crate::SeekRequest;
use crate::seek_state::{PlaybackResumeIntent, SeekCommitState, VisibleScrubPreview};
use crate::{
    PlayerError, PlayerErrorKind, PlayerEvent, SeekTargetFramePresentation,
    VideoPrerollOutputFloor, VideoPrerollOutputFloorClear, VideoPrerollOutputFloorResult,
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

    /// Запоминает live-scrub preview только в момент, когда scheduler уже сделал frame текущим.
    fn note_visible_live_scrub_preview_for_release(
        &mut self,
        seek_commit: SeekCommitState,
        frame_pts: Duration,
        frame_generation: u64,
    ) {
        if frame_generation != seek_commit.generation {
            return;
        }

        let Some(context) = self.active_seek_landing_context(seek_commit) else {
            return;
        };
        if context.request_kind() != ScrubRequestKind::LiveScrub {
            return;
        }

        let Some(frame_identity) = self.current_present_frame_identity() else {
            return;
        };
        if frame_identity.decoded_generation() != frame_generation
            || frame_identity.pts() != frame_pts
        {
            return;
        }

        let media_time = MediaTime::from_duration(frame_pts);
        let pts = self.seek_landing_target_pts(context.track_selection().video_track, media_time);
        self.seek_runtime
            .note_visible_scrub_preview(VisibleScrubPreview {
                context,
                timing: ScrubFrameTiming::new(media_time, pts),
                frame_identity,
            });
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
        self.note_visible_live_scrub_preview_for_release(
            seek_commit,
            frame_pts,
            present_frame_generation,
        );
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
            self.push_player_event(PlayerEvent::SeekTargetFramePresented(
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
    pub(crate) fn set_simple_scrub_state_for_tests(
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

pub(super) fn playback_resume_intent_name(intent: PlaybackResumeIntent) -> &'static str {
    match intent {
        PlaybackResumeIntent::Pause => "pause",
        PlaybackResumeIntent::Play => "play",
    }
}
