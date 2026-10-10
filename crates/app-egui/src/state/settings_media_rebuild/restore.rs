//! Восстановление позиции, дорожек и громкости после установки пересобранного media.
//!
//! Раньше `restore_playback_after_media_reconfigure` отправлял команды и блокирующе ждал
//! итог (`wait_for_outcome`). Для `SeekTo` итог приходит только после реального seek-а
//! (сеть!), поэтому ожидание подвешивало окно. Теперь отправка и разбор итога разделены:
//! неблокирующий путь опрашивает receipt каждый кадр, блокирующий (откат настроек) ждёт.

use super::super::*;

/// Receipt восстановления, который ещё не получил итог от player owner-а.
pub(in crate::state) struct PendingPlaybackRestore {
    receipt: player_core::InstalledMediaStateRestoreReceipt,
    media_instance_id: player_core::MediaInstanceId,
}

/// Итог одного неблокирующего опроса receipt-а.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::state) enum PlaybackRestorePoll {
    /// Owner ещё не ответил.
    Pending,
    /// Позиция и дорожки восстановлены для того же instance.
    Applied,
    /// Восстановление не удалось; текст технический (в лог, не пользователю).
    Failed(String),
}

impl PendingPlaybackRestore {
    /// Забирает итог, если он уже опубликован; никогда не ждёт.
    pub(in crate::state) fn poll(&self) -> PlaybackRestorePoll {
        match self.receipt.try_take_outcome() {
            Ok(None) => PlaybackRestorePoll::Pending,
            Ok(Some(outcome)) => classify_restore_outcome(outcome, self.media_instance_id),
            Err(error) => PlaybackRestorePoll::Failed(format!(
                "exact position/track restore outcome missing: {error}"
            )),
        }
    }

    /// Блокирующее ожидание для синхронного пути (откат настроек).
    pub(in crate::state) fn wait(self) -> Result<(), String> {
        let outcome = self
            .receipt
            .wait_for_outcome()
            .map_err(|error| format!("exact position/track restore outcome missing: {error}"))?;
        match classify_restore_outcome(outcome, self.media_instance_id) {
            PlaybackRestorePoll::Applied => Ok(()),
            PlaybackRestorePoll::Failed(message) => Err(message),
            PlaybackRestorePoll::Pending => Err("restore outcome was not final".to_string()),
        }
    }
}

/// Принимает только `Applied` для того же instance; всё остальное — отказ с причиной.
fn classify_restore_outcome(
    outcome: player_core::InstalledMediaStateRestoreOutcome,
    media_instance_id: player_core::MediaInstanceId,
) -> PlaybackRestorePoll {
    match outcome {
        player_core::InstalledMediaStateRestoreOutcome::Applied {
            media_instance_id: applied_instance,
        } if applied_instance == media_instance_id => PlaybackRestorePoll::Applied,
        outcome => PlaybackRestorePoll::Failed(format!(
            "exact position/track restore was rejected: {outcome:?}"
        )),
    }
}

impl AppState {
    /// Отправляет команды восстановления controls и возвращает receipt итога.
    ///
    /// Работает только через exact request/instance границы после `Installed`.
    pub(in crate::state) fn dispatch_playback_restore(
        &mut self,
        snapshot: &PlayerSnapshot,
        installed: &InstalledSingleMediaOpen,
    ) -> Result<PendingPlaybackRestore, String> {
        let player_core::MediaInstallCompletion::Installed {
            media_instance_id,
            applied_intent,
            ..
        } = installed.completion
        else {
            return Err("media reconfigure completion was not Installed".to_string());
        };
        let desired_intent = playback_intent_from_snapshot(snapshot);
        if applied_intent != desired_intent {
            return Err(format!(
                "Installed applied unexpected playback intent: {applied_intent:?}"
            ));
        }

        self.player_worker
            .try_send_command(PlayerCommand::SetVolume(snapshot.volume))
            .map_err(|error| format!("volume restore dispatch failed: {error}"))?;
        if let Some(selected_quality) = snapshot
            .available_qualities
            .iter()
            .find(|quality| quality.selected)
        {
            self.player_worker
                .try_send_command(PlayerCommand::SelectQuality(QualitySelection::Specific(
                    selected_quality.id.clone(),
                )))
                .map_err(|error| format!("quality restore dispatch failed: {error}"))?;
        }

        let restore = player_core::InstalledMediaStateRestore {
            request_id: installed.player_request_id,
            media_instance_id,
            video_track: snapshot.selected_tracks.video_track.map_or(
                player_core::InstalledTrackRestore::KeepDefault,
                player_core::InstalledTrackRestore::Select,
            ),
            audio_track: snapshot.selected_tracks.audio_track.map_or(
                player_core::InstalledTrackRestore::KeepDefault,
                player_core::InstalledTrackRestore::Select,
            ),
            subtitle_track: snapshot.selected_tracks.subtitle_track.map_or(
                player_core::InstalledSubtitleRestore::KeepDefault,
                player_core::InstalledSubtitleRestore::Select,
            ),
            volume: player_core::InstalledVolumeRestore::Set(snapshot.volume),
            position: if snapshot.current_position > Duration::ZERO {
                player_core::InstalledPositionRestore::SeekTo(snapshot.current_position)
            } else {
                player_core::InstalledPositionRestore::KeepStart
            },
        };
        let receipt = self
            .player_worker
            .restore_installed_media_state(restore)
            .map_err(|error| format!("exact position/track restore dispatch failed: {error}"))?;
        Ok(PendingPlaybackRestore {
            receipt,
            media_instance_id,
        })
    }
}
