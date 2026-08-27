//! Public command dispatch и видимая по умолчанию scrub correlation telemetry.
//!
//! Модуль владеет только маршрутизацией команды к существующим session boundary
//! и публикацией её receipt. Состояние correlation остаётся у `PlayerSession`, а
//! public `PlayerCommand` и worker channel не получают diagnostic-only fields.

use tracing::{debug, info};

use crate::{PlayerCommand, PlayerCommandOutcome, PlayerResult};

use super::PlayerSession;
use super::scrub_command_correlation::{
    CorrelatedPlayerCommand, SCRUB_COMMAND_SCHEMA_VERSION, ScrubCommandCorrelation,
};

impl PlayerSession {
    /// Применяет команду к state machine через существующие intent boundaries.
    pub fn dispatch_command(
        &mut self,
        command: PlayerCommand,
    ) -> PlayerResult<PlayerCommandOutcome> {
        let correlated_command = self.scrub_command_correlation.correlate(command)?;
        self.trace_received_command(&correlated_command);
        let command = correlated_command.into_command();

        let command_result = match command {
            PlayerCommand::OpenMedia(request) => self.open_media(request),
            PlayerCommand::Play => self.play(),
            PlayerCommand::Pause => self.pause(),
            PlayerCommand::TogglePlayback => self.toggle_playback(),
            PlayerCommand::Seek(request) => self.seek(request),
            PlayerCommand::BeginScrub { live_scrub } => self.begin_scrub(live_scrub),
            PlayerCommand::UpdateScrub(request) => self.update_scrub(request),
            PlayerCommand::PreviewScrub {
                request,
                live_scrub,
            } => self.preview_scrub(request, live_scrub),
            PlayerCommand::EndScrub { policy, live_scrub } => {
                return self
                    .end_scrub(policy, live_scrub)
                    .map(PlayerCommandOutcome::ScrubCommit);
            }
            PlayerCommand::Stop => self.stop(),
            PlayerCommand::SetPlaybackRate(playback_rate) => {
                return Ok(self.set_playback_rate(playback_rate));
            }
            PlayerCommand::SetVolume(volume) => self.set_volume(volume),
            PlayerCommand::ToggleMute { fallback_volume } => self.toggle_mute(fallback_volume),
            PlayerCommand::SelectVideoTrack(track_id) => self.select_video_track(track_id),
            PlayerCommand::SelectAudioTrack(track_id) => self.select_audio_track(track_id),
            PlayerCommand::SelectSubtitleTrack(track_id) => self.select_subtitle_track(track_id),
            PlayerCommand::SelectQuality(selection) => self.select_quality(selection),
            PlayerCommand::ReloadConfig => self.reload_config(),
            PlayerCommand::Shutdown => self.shutdown(),
        };

        command_result.map(|()| PlayerCommandOutcome::Applied)
    }

    /// Публикует full DEBUG receipt и две INFO correlation forms scrub command-а.
    fn trace_received_command(&self, correlated_command: &CorrelatedPlayerCommand) {
        let Some(scrub) = correlated_command.scrub() else {
            self.trace_regular_command_debug(correlated_command);
            return;
        };

        self.trace_scrub_command_debug(correlated_command);
        self.trace_scrub_dispatch(scrub);
        self.trace_scrub_acceptance(scrub);
    }

    /// Сохраняет historical DEBUG marker обычных command parser consumers.
    fn trace_regular_command_debug(&self, correlated_command: &CorrelatedPlayerCommand) {
        debug!(
            command = ?correlated_command.command(),
            playback_state = ?self.playback_state(),
            draining_after_eof = self.is_eof_draining(),
            current_position_ms = self.snapshot.current_position.as_secs_f64() * 1000.0,
            duration_ms = ?self
                .snapshot
                .duration
                .map(|duration| duration.as_secs_f64() * 1000.0),
            "Player command received"
        );
    }

    /// Публикует full scrub Debug с message вне legacy/correlation marker families.
    fn trace_scrub_command_debug(&self, correlated_command: &CorrelatedPlayerCommand) {
        debug!(
            command = ?correlated_command.command(),
            playback_state = ?self.playback_state(),
            draining_after_eof = self.is_eof_draining(),
            current_position_ms = self.snapshot.current_position.as_secs_f64() * 1000.0,
            duration_ms = ?self
                .snapshot
                .duration
                .map(|duration| duration.as_secs_f64() * 1000.0),
            "Player scrub command debug received"
        );
    }

    /// Публикует INFO dispatch form, видимую при стандартном playback filter-е.
    fn trace_scrub_dispatch(&self, scrub: ScrubCommandCorrelation) {
        let requested_target = scrub.requested_target();
        info!(
            scrub_schema_version = SCRUB_COMMAND_SCHEMA_VERSION,
            scrub_command_id = scrub.id().get(),
            scrub_stage = scrub.stage().as_str(),
            scrub_command_form = "dispatch",
            scrub_target_kind = requested_target.kind(),
            scrub_requested_target_ms = requested_target.milliseconds(),
            "Player scrub command received"
        );
    }

    /// Публикует INFO acceptance form с owner-monotonic scrub span.
    fn trace_scrub_acceptance(&self, scrub: ScrubCommandCorrelation) {
        let requested_target = scrub.requested_target();
        let scrub_elapsed_ms = self
            .seek_runtime
            .simple_scrub_elapsed()
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or_default();
        info!(
            kind = "seek_acceptance",
            scrub_schema_version = SCRUB_COMMAND_SCHEMA_VERSION,
            scrub_command_id = scrub.id().get(),
            scrub_stage = scrub.stage().as_str(),
            scrub_command_form = "acceptance",
            scrub_target_kind = requested_target.kind(),
            scrub_requested_target_ms = requested_target.milliseconds(),
            scrub_elapsed_ms,
            current_position_ms = self
                .snapshot
                .timeline
                .current_position
                .as_duration()
                .as_millis(),
            "Player scrub command received"
        );
    }
}
