use std::time::Instant;

use tracing::debug;

use crate::pipeline::CapturedAudioClockMapping;
use crate::{
    PlaybackRate, PlaybackRateAudioTempoRejectReason, PlaybackState, PlayerCommandOutcome,
    PlayerCommandReject, PlayerError, PlayerErrorKind, PlayerSession,
};

use super::audio_tempo_rate_change::PreparedAudioTempoRateChange;
use super::audio_tempo_rate_change::audio_output_unavailable_reject;

/// Политика скорости при новом media load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaybackRateMediaLoadPolicy {
    /// V1 всегда возвращает новое media к прежнему `1.0x` поведению.
    ResetOnMediaLoad,

    /// Future extension point для playlist/session-preserve сценариев без TOML/UI wiring в S33.
    #[allow(dead_code)]
    PreserveAcrossPlaylist,
}

impl PlayerSession {
    /// Применяет checked playback rate только в состояниях, где S33 разрешает mutation.
    pub(super) fn set_playback_rate(
        &mut self,
        playback_rate: PlaybackRate,
    ) -> PlayerCommandOutcome {
        if self.snapshot.playback_rate == playback_rate {
            if self.pipeline.has_selected_audio_track()
                && (!self.pipeline.has_audio_output() || !self.pipeline.has_audio_clock())
            {
                return PlayerCommandOutcome::Rejected(audio_output_unavailable_reject());
            }
            return PlayerCommandOutcome::Applied;
        }

        match self.playback_state() {
            PlaybackState::Playing => self.apply_playing_playback_rate(playback_rate),
            PlaybackState::Paused => self.apply_paused_playback_rate(playback_rate),
            state => PlayerCommandOutcome::Rejected(
                PlayerCommandReject::PlaybackRateUnavailableForState { state },
            ),
        }
    }

    /// Применяет rate во время playback без seek transaction и generation advance.
    fn apply_playing_playback_rate(&mut self, playback_rate: PlaybackRate) -> PlayerCommandOutcome {
        let prepared_audio_change = match self.prepare_audio_tempo_rate_change(playback_rate) {
            Ok(prepared_audio_change) => prepared_audio_change,
            Err(reject) => return PlayerCommandOutcome::Rejected(reject),
        };

        let observed_at = Instant::now();
        let captured_audio_clock = self.pipeline.capture_audio_clock_mapping();
        let current_media_position = captured_audio_clock
            .map(CapturedAudioClockMapping::media_position)
            .unwrap_or_else(|| self.presentation_clock_position_at(observed_at));
        if let Err(reject) = self.commit_prepared_audio_clock_mapping(
            current_media_position,
            captured_audio_clock,
            playback_rate,
            &prepared_audio_change,
        ) {
            return PlayerCommandOutcome::Rejected(reject);
        }

        let previous_playback_rate = self.snapshot.playback_rate;
        self.snapshot.playback_rate = playback_rate;
        self.reconcile_video_backlog_recovery_after_rate_change(playback_rate);
        self.publish_clock_sample(current_media_position);

        if !self.pipeline.has_audio_clock() {
            self.pipeline.start_monotonic_media_clock(
                current_media_position,
                observed_at,
                playback_rate,
            );
        }

        log_committed_playback_rate(
            previous_playback_rate,
            playback_rate,
            PlaybackState::Playing,
            current_media_position,
            &prepared_audio_change,
        );

        PlayerCommandOutcome::Applied
    }

    /// Применяет rate на pause без движения media clock.
    fn apply_paused_playback_rate(&mut self, playback_rate: PlaybackRate) -> PlayerCommandOutcome {
        let prepared_audio_change = match self.prepare_audio_tempo_rate_change(playback_rate) {
            Ok(prepared_audio_change) => prepared_audio_change,
            Err(reject) => return PlayerCommandOutcome::Rejected(reject),
        };
        let current_media_position = self.snapshot.current_position;
        let captured_audio_clock = self
            .pipeline
            .capture_paused_audio_clock_mapping(current_media_position);
        if let Err(reject) = self.commit_prepared_audio_clock_mapping(
            current_media_position,
            captured_audio_clock,
            playback_rate,
            &prepared_audio_change,
        ) {
            return PlayerCommandOutcome::Rejected(reject);
        }

        let previous_playback_rate = self.snapshot.playback_rate;
        self.snapshot.playback_rate = playback_rate;
        self.reconcile_video_backlog_recovery_after_rate_change(playback_rate);
        log_committed_playback_rate(
            previous_playback_rate,
            playback_rate,
            PlaybackState::Paused,
            current_media_position,
            &prepared_audio_change,
        );
        PlayerCommandOutcome::Applied
    }

    /// Синхронизирует video overload lifecycle только после успешного rate commit-а.
    fn reconcile_video_backlog_recovery_after_rate_change(&mut self, playback_rate: PlaybackRate) {
        let recovery_report = self
            .pipeline
            .reconcile_video_backlog_recovery_after_rate_change(playback_rate);
        if recovery_report.restored_staged_packets > 0 {
            debug!(
                restored_staged_video_packets = recovery_report.restored_staged_packets,
                pending_video_packets = self.pipeline.pending_video_packet_len(),
                playback_rate = %playback_rate,
                "Playback-rate downshift восстановил video recovery continuation"
            );
        }
    }

    /// Фиксирует clock mapping только после подтверждения audio backend-а.
    fn commit_prepared_audio_clock_mapping(
        &mut self,
        current_media_position: std::time::Duration,
        captured_audio_clock: Option<CapturedAudioClockMapping>,
        playback_rate: PlaybackRate,
        prepared_audio_change: &PreparedAudioTempoRateChange,
    ) -> Result<(), PlayerCommandReject> {
        let mapping_result = match (prepared_audio_change.tempo_report(), captured_audio_clock) {
            (Some(report), Some(captured_audio_clock)) => self
                .pipeline
                .reanchor_audio_clock_media_mapping_for_tempo_rate_change(
                    captured_audio_clock,
                    playback_rate,
                    report,
                ),
            (Some(_), None) => Err(anyhow::anyhow!(
                "tempo backend подтвердил rate change без audio clock snapshot"
            )),
            (None, Some(captured_audio_clock)) => {
                self.pipeline
                    .reanchor_audio_clock_media_mapping_for_captured_rate_change(
                        captured_audio_clock,
                        playback_rate,
                    );
                Ok(())
            }
            (None, None) => {
                self.pipeline
                    .reanchor_audio_clock_media_mapping_for_rate_change(
                        current_media_position,
                        playback_rate,
                    );
                Ok(())
            }
        };

        if let Err(error) = mapping_result {
            // Backend уже подтвердил segment, поэтому некорректный report — это
            // нарушение внутреннего контракта, которое невозможно откатить.
            self.mark_fatal_error(PlayerError::new(
                PlayerErrorKind::RuntimeError,
                format!("Audio tempo mapping contract violation: {error}"),
            ));
            return Err(PlayerCommandReject::PlaybackRateAudioTempoUnavailable {
                reason: PlaybackRateAudioTempoRejectReason::BackendRejected,
            });
        }

        Ok(())
    }

    /// Возвращает текущую internal policy без чтения config/settings.
    fn playback_rate_media_load_policy(&self) -> PlaybackRateMediaLoadPolicy {
        PlaybackRateMediaLoadPolicy::ResetOnMediaLoad
    }

    /// Применяет media-load policy к session-owned snapshot storage.
    pub(super) fn apply_playback_rate_media_load_policy(&mut self) {
        match self.playback_rate_media_load_policy() {
            PlaybackRateMediaLoadPolicy::ResetOnMediaLoad => {
                self.snapshot.playback_rate = PlaybackRate::NORMAL;
            }
            PlaybackRateMediaLoadPolicy::PreserveAcrossPlaylist => {}
        }
    }
}

/// Stable runtime marker подтверждает весь атомарный commit, а не только вход команды.
fn log_committed_playback_rate(
    previous_playback_rate: PlaybackRate,
    playback_rate: PlaybackRate,
    playback_state: PlaybackState,
    current_media_position: std::time::Duration,
    prepared_audio_change: &PreparedAudioTempoRateChange,
) {
    let tempo_pending_output_ms = prepared_audio_change
        .tempo_report()
        .map(|report| report.pending_processor_output().duration().as_millis())
        .unwrap_or(0);
    debug!(
        previous_rate = previous_playback_rate.as_f32(),
        new_rate = playback_rate.as_f32(),
        state = ?playback_state,
        anchor_media_ms = current_media_position.as_millis(),
        tempo_pending_output_ms,
        "Playback rate applied"
    );
}
