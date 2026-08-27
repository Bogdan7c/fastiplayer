use std::time::{Duration, Instant};

use frame_server_core::{CancelScrubReason, ValidatedFrameServerConfig};
use media_core::MediaTime;
use tracing::{debug, info, warn};

use crate::seek_state::PlaybackResumeIntent;
use crate::{
    MediaInstallCancellationCause, PlaybackState, PlayerError, PlayerErrorKind, PlayerEvent,
    PlayerResult, QualitySelection, TrackId,
};

use super::PlayerSession;

impl PlayerSession {
    /// Переключает playback между пользовательскими смыслами "сейчас слышно/идёт" и "пауза".
    pub fn toggle_playback(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.cancel_active_scrub_for_external_command(CancelScrubReason::UserCancelled);
        if self.playback_state().is_playback_active() {
            // EOF drain тоже active: audio tail ещё может звучать, поэтому toggle обязан ставить pause.
            self.pause()
        } else {
            self.play()
        }
    }

    /// Переводит playback в `Playing` и запускает audio output.
    pub(super) fn play(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.cancel_active_scrub_for_external_command(CancelScrubReason::UserCancelled);
        if let Some(seek_commit) = self.seek_runtime.active_commit_mut() {
            seek_commit.resume_intent = PlaybackResumeIntent::Play;
            self.set_playback_state(PlaybackState::Seeking);
            return Ok(());
        }

        if self.should_replay_from_eof_on_play() {
            return self.restart_playback_after_eof();
        }

        if let Some(resume_target) = self.expired_live_resume_target() {
            info!(
                expired_position_ms = self.current_source_position.as_millis(),
                resume_target_ms = resume_target.as_duration().as_millis(),
                "Paused live position expired; Play starts a recovery seek"
            );
            return self.seek_expired_live_position_resuming_playback(resume_target);
        }

        self.set_playback_state(PlaybackState::Playing);

        if let Some(play_result) = self.play_audio_output_with_resume_event() {
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

    /// Запускает preroll перед autoplay, не включая audio stream раньше заполнения buffer.
    pub(super) fn begin_autoplay_preroll(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.media_lifecycle.clear_pending_autoplay();
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
        self.ensure_not_shutdown()?;
        if self.snapshot.playback_state != PlaybackState::Buffering {
            return Ok(false);
        }

        if !self.autoplay_preroll_ready(audio_preroll_target_ms) {
            return Ok(false);
        }

        // Накопленный PCM сам по себе не доказывает recovery source-а. После
        // demux-owned freeze matching packet/event сначала обязан снять retry fence.
        if self.installed_demux_retry_blocks_buffering_resume() {
            return Ok(false);
        }

        let publishes_audio_resume = self.pipeline.audio_output_needs_play_request();
        if let Some(play_result) = self.pipeline.play_audio_output() {
            if let Err(error) = play_result {
                let player_error = PlayerError::new(
                    PlayerErrorKind::RuntimeError,
                    format!("Audio play after buffering error: {error}"),
                );
                let repeats_current_error =
                    self.snapshot.last_error.as_ref() == Some(&player_error);
                if !repeats_current_error {
                    warn!(error = %error, "Не удалось запустить audio после preroll");
                    self.record_recoverable_error(player_error);
                }
                return Ok(false);
            }

            let observed_at = Instant::now();
            let audio_now = self.audio_clock_now();
            self.pipeline
                .reset_audio_clock_sample(audio_now, observed_at);
        }

        // Сначала backend play и clock anchors, затем observable success state/events.
        self.anchor_monotonic_media_clock_if_needed(Instant::now());
        self.set_playback_state(PlaybackState::Playing);
        if publishes_audio_resume {
            self.push_player_event(PlayerEvent::AudioPlaybackResumed);
        }
        Ok(true)
    }

    /// Проверяет минимальный readiness для перехода из `Buffering` в `Playing`.
    fn autoplay_preroll_ready(&self, audio_preroll_target_ms: f64) -> bool {
        let audio_ready = self
            .autoplay_audio_readiness(audio_preroll_target_ms)
            .is_ready();
        let video_ready = self.autoplay_video_preroll_ready();

        audio_ready && video_ready
    }

    /// Проверяет video gate autoplay-preroll без раскрытия очередей pipeline.
    fn autoplay_video_preroll_ready(&self) -> bool {
        if !self.pipeline.has_selected_video_track() {
            return true;
        }

        self.pipeline.has_present_video_frame() || !self.pipeline.video_present_queue_is_empty()
    }

    /// Замораживает presentation clock перед demux-owned buffering transition.
    ///
    /// В отличие от пользовательской Pause этот boundary намеренно сохраняет
    /// video presentation queue: готовые кадры входят в общий resume preroll gate.
    pub(super) fn freeze_playback_for_demux_buffering(&mut self) -> PlayerResult<()> {
        let audio_pause_result = self.pipeline.pause_audio_output_and_capture_clock();
        let frozen_media_position = match audio_pause_result {
            Some(Ok(captured_clock)) => captured_clock.media_position(),
            Some(Err(error)) => {
                warn!(error = %error, "Не удалось заморозить audio для demux buffering");
                return Err(PlayerError::new(
                    PlayerErrorKind::RuntimeError,
                    format!("Audio pause for demux buffering error: {error}"),
                ));
            }
            None => self.presentation_clock_position_at(Instant::now()),
        };

        self.freeze_current_position_for_pause(frozen_media_position);
        Ok(())
    }

    /// Переводит playback в `Paused` и останавливает audio output.
    pub(super) fn pause(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.cancel_active_scrub_for_external_command(CancelScrubReason::UserCancelled);
        if let Some(seek_commit) = self.seek_runtime.active_commit_mut() {
            seek_commit.resume_intent = PlaybackResumeIntent::Pause;
            self.pause_audio_output_for_seek();
            self.set_playback_state(PlaybackState::Seeking);
            return Ok(());
        }

        // Output owner возвращает timing, зафиксированный под callback mutex.
        // Для video-only тот же lifecycle фиксируется по monotonic anchor-у.
        let audio_pause_result = self.pipeline.pause_audio_output_and_capture_clock();
        let paused_media_position = match audio_pause_result {
            Some(Ok(captured_clock)) => captured_clock.media_position(),
            Some(Err(error)) => {
                warn!(error = %error, "Не удалось остановить audio");
                return Err(PlayerError::new(
                    PlayerErrorKind::RuntimeError,
                    format!("Audio pause error: {error}"),
                ));
            }
            None => self.presentation_clock_position_at(Instant::now()),
        };
        self.freeze_current_position_for_pause(paused_media_position);
        self.set_playback_state(PlaybackState::Paused);
        self.clear_queued_video_frames();

        Ok(())
    }

    /// Останавливает текущий media и сбрасывает timeline без завершения session.
    pub(super) fn stop(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.reset_media_state();
        self.set_playback_state(PlaybackState::Stopped);
        Ok(())
    }

    /// Валидирует и устанавливает громкость.
    pub(super) fn set_volume(&mut self, volume: f32) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.validate_volume_command_value("volume", volume)?;
        self.remember_last_nonzero_volume(volume);
        self.apply_audio_volume(volume);
        Ok(())
    }

    /// Переключает mute и восстанавливает предыдущую слышимую громкость.
    pub(super) fn toggle_mute(&mut self, fallback_volume: f32) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.validate_volume_command_value("fallback_volume", fallback_volume)?;

        if self.is_current_volume_audible() {
            self.remember_last_nonzero_volume(self.snapshot.volume);
            self.apply_audio_volume(0.0);
            return Ok(());
        }

        let restored_volume = self
            .last_nonzero_volume
            .filter(|volume| Self::is_rememberable_volume(*volume))
            .or_else(|| Self::is_rememberable_volume(fallback_volume).then_some(fallback_volume))
            .unwrap_or(1.0);

        self.remember_last_nonzero_volume(restored_volume);
        self.apply_audio_volume(restored_volume);
        Ok(())
    }

    /// Проверяет значение volume-like команды и пишет recoverable error без изменения state.
    fn validate_volume_command_value(&mut self, name: &str, volume: f32) -> PlayerResult<()> {
        if volume.is_finite() && (0.0..=1.0).contains(&volume) {
            return Ok(());
        }

        let error = PlayerError::new(
            PlayerErrorKind::InvalidCommand,
            format!("{name} must be finite and within 0.0..=1.0, got {volume}"),
        );
        self.record_recoverable_error(error.clone());
        Err(error)
    }

    /// Запоминает только громкость, которая не схлопнется в muted-состояние.
    fn remember_last_nonzero_volume(&mut self, volume: f32) {
        if Self::is_rememberable_volume(volume) {
            self.last_nonzero_volume = Some(volume);
        }
    }

    /// Применяет громкость к snapshot и активному output-у, не меняя remembered-volume.
    fn apply_audio_volume(&mut self, volume: f32) {
        self.snapshot.volume = volume;
        self.snapshot.muted = !Self::is_rememberable_volume(volume);
        let _output_was_present = self.pipeline.set_audio_output_volume(volume);
    }

    /// Текущий snapshot считается слышимым только если mute-флаг и число согласованы.
    fn is_current_volume_audible(&self) -> bool {
        !self.snapshot.muted && Self::is_rememberable_volume(self.snapshot.volume)
    }

    /// Практический non-zero: совпадает с существующей EPSILON mute-семантикой snapshot-а.
    fn is_rememberable_volume(volume: f32) -> bool {
        volume > f32::EPSILON
    }

    /// Выбирает video track через capability owner текущей session.
    pub(super) fn select_video_track(&mut self, track_id: TrackId) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.select_requested_video_track(track_id)?;
        self.push_player_event(PlayerEvent::VideoTrackSelected(track_id));
        Ok(())
    }

    /// Выбирает audio track через pipeline-owned track boundary.
    pub(super) fn select_audio_track(&mut self, track_id: TrackId) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.pipeline.select_audio_track(track_id);
        self.snapshot.selected_tracks.audio_track = Some(track_id);
        self.push_player_event(PlayerEvent::AudioTrackSelected(track_id));
        Ok(())
    }

    /// Выбирает subtitle track или отключает субтитры.
    pub(super) fn select_subtitle_track(&mut self, track_id: Option<TrackId>) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.snapshot.selected_tracks.subtitle_track = track_id;
        self.push_player_event(PlayerEvent::SubtitleTrackSelected(track_id));
        Ok(())
    }

    /// Фиксирует выбор качества как событие для будущего source/service слоя.
    pub(super) fn select_quality(&mut self, selection: QualitySelection) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.push_player_event(PlayerEvent::QualitySelectionChanged(selection));
        Ok(())
    }

    /// Запрашивает reload config без чтения файлов внутри player-core.
    pub(super) fn reload_config(&mut self) -> PlayerResult<()> {
        self.ensure_not_shutdown()?;
        self.push_player_event(PlayerEvent::ConfigReloadRequested);
        Ok(())
    }

    /// Обновляет session-owned frame-server policy без сброса текущего SeekLanding.
    pub(crate) fn apply_frame_server_policy_config(
        &mut self,
        frame_server_config: ValidatedFrameServerConfig,
    ) {
        self.frame_server_config = frame_server_config;
    }

    /// Возвращает read-only session-owned frame-server policy snapshot.
    #[cfg(test)]
    pub(crate) const fn frame_server_policy_config(&self) -> ValidatedFrameServerConfig {
        self.frame_server_config
    }

    /// Переводит session в stopped state.
    pub(super) fn shutdown(&mut self) -> PlayerResult<()> {
        self.cancel_active_staged_media_install(MediaInstallCancellationCause::LifecycleShutdown);
        self.shutdown_requested = true;
        self.clear_installed_demux_retry();
        self.push_player_event(PlayerEvent::ShutdownRequested);
        self.set_playback_state(PlaybackState::Stopped);
        Ok(())
    }

    /// Запрещает команды после shutdown, кроме самого idempotent shutdown.
    pub(super) fn ensure_not_shutdown(&mut self) -> PlayerResult<()> {
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
    pub(super) fn set_playback_state(&mut self, playback_state: PlaybackState) {
        let previous_state = self.playback_state();
        self.eof_drain.sync_from_playback_state(playback_state);
        self.snapshot.playback_state = playback_state;

        if previous_state == playback_state {
            return;
        }

        debug!(
            previous_state = ?previous_state,
            playback_state = ?playback_state,
            draining_after_eof = self.is_eof_draining(),
            current_position_ms = self.snapshot.current_position.as_secs_f64() * 1000.0,
            duration_ms = ?self
                .snapshot
                .duration
                .map(|duration| duration.as_secs_f64() * 1000.0),
            "Playback state changed"
        );

        self.push_player_event(PlayerEvent::PlaybackStateChanged(playback_state));
    }

    /// Сохраняет recoverable error в snapshot и event queue.
    pub(super) fn record_recoverable_error(&mut self, error: PlayerError) {
        self.snapshot.last_error = Some(error.clone());
        self.push_player_event(PlayerEvent::RecoverableError(error));
    }

    /// Публикует редкое явное изменение позиции, например seek.
    pub(super) fn publish_position_changed(&mut self, position: Duration) {
        self.publish_clock_sample(position);
        self.reanchor_no_audio_clock(position, Instant::now());
        let relative_position = self
            .relative_position_for_source(MediaTime::from_duration(position))
            .as_duration();
        self.push_player_event(PlayerEvent::PositionChanged(relative_position));
    }

    /// Сохраняет runtime error как user-facing ошибку.
    pub(super) fn set_runtime_error(&mut self, message: String) {
        let error = PlayerError::new(PlayerErrorKind::RuntimeError, message);
        self.snapshot.last_error = Some(error.clone());
        self.push_player_event(PlayerEvent::RecoverableError(error));
    }

    /// Очищает последнюю ошибку после успешного media action.
    pub(super) fn clear_error(&mut self) {
        self.snapshot.last_error = None;
    }
}
