//! Чистые мосты между памятью выбора и остальным приложением (без `AppState`).
//!
//! `AppState` зовёт эти функции из своих тонких обёрток, а тесты — напрямую: так вся цепочка
//! «выбор пользователя -> память -> файл -> перезапуск -> восстановление» проверяется тем же
//! кодом, что работает в приложении, хотя сам `AppState` без окна не собрать.

use std::time::Instant;

use media_core::TrackId;
use player_core::{MediaInstanceId, TrackSummarySnapshot};

use super::hints::audio_hint_for_track;
use super::owner::{RecordOutcome, RememberedForVideo, TrackMemoryOwner};
use crate::state::subtitles::{SubtitleMemoryChanges, SubtitleRestoreRequest};

/// Запоминает аудиодорожку `track_id`, выбранную пользователем, по списку дорожек снимка.
pub(crate) fn record_user_audio_choice(
    owner: &mut TrackMemoryOwner,
    media_instance_id: MediaInstanceId,
    tracks: &[TrackSummarySnapshot],
    track_id: TrackId,
    now: Instant,
) -> RecordOutcome {
    let hint = match audio_hint_for_track(tracks, track_id) {
        Ok(Some(hint)) => hint,
        // Не аудиодорожка (или её нет в снимке): запоминать нечего.
        Ok(None) => return RecordOutcome::NotApplicable,
        Err(error) => {
            tracing::debug!(%error, "The audio track cannot be remembered");
            return RecordOutcome::NotApplicable;
        }
    };
    owner.remember_audio(media_instance_id, hint, now)
}

/// Записывает в память действия пользователя с субтитрами.
pub(crate) fn record_subtitle_changes(
    owner: &mut TrackMemoryOwner,
    changes: SubtitleMemoryChanges,
    now: Instant,
) {
    if let Some(choice) = changes.choice {
        let _outcome: RecordOutcome =
            owner.remember_subtitles(changes.media_instance_id, choice, now);
    }
    if let Some(offset) = changes.sync_offset {
        match playlist_state::SubtitleSyncOffset::from_millis(offset.as_micros() / 1_000) {
            Ok(offset) => {
                let _outcome: RecordOutcome =
                    owner.remember_sync_offset(changes.media_instance_id, offset, now);
            }
            Err(error) => {
                tracing::debug!(?error, "The subtitle delay is out of the remembered range");
            }
        }
    }
}

/// Что передать владельцу субтитров при старте media по запомненному.
pub(crate) fn subtitle_restore_request(remembered: &RememberedForVideo) -> SubtitleRestoreRequest {
    SubtitleRestoreRequest {
        choice: remembered.subtitles.clone(),
        sync_offset: subtitle_core::SubtitleSyncOffset::from_millis(
            remembered.sync_offset.millis(),
        ),
    }
}
