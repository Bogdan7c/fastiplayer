//! Какое намерение импорта выбрать для брошенного файла плейлиста.
//!
//! Решение владельца: поведение задаётся настройкой `playlist.dropped_playlist_file_action`;
//! по умолчанию «по месту» — на панель плейлиста добавляем, на видео заменяем. Во всех
//! случаях дальше идёт обычный предпросмотр импорта (а для замены — подтверждение).

use fastiplayer_config::DroppedPlaylistFileAction;

use super::request::DropTarget;
use crate::playlist_runtime::PlaylistImportIntent;

/// Чистое правило «настройка × место броска → намерение импорта».
pub(crate) fn playlist_import_intent(
    action: DroppedPlaylistFileAction,
    target: DropTarget,
) -> PlaylistImportIntent {
    match (action, target) {
        (DroppedPlaylistFileAction::NewPlaylist, _) => PlaylistImportIntent::ReplaceQueue,
        (DroppedPlaylistFileAction::AppendToQueue, _) => PlaylistImportIntent::AppendToQueue,
        (DroppedPlaylistFileAction::ByDropTarget, DropTarget::Playlist) => {
            PlaylistImportIntent::AppendToQueue
        }
        (DroppedPlaylistFileAction::ByDropTarget, DropTarget::Video) => {
            PlaylistImportIntent::ReplaceQueue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_and_drop_target_choose_the_import_intent() {
        use DroppedPlaylistFileAction::{AppendToQueue, ByDropTarget, NewPlaylist};
        let cases = [
            (
                ByDropTarget,
                DropTarget::Playlist,
                PlaylistImportIntent::AppendToQueue,
            ),
            (
                ByDropTarget,
                DropTarget::Video,
                PlaylistImportIntent::ReplaceQueue,
            ),
            (
                NewPlaylist,
                DropTarget::Playlist,
                PlaylistImportIntent::ReplaceQueue,
            ),
            (
                NewPlaylist,
                DropTarget::Video,
                PlaylistImportIntent::ReplaceQueue,
            ),
            (
                AppendToQueue,
                DropTarget::Playlist,
                PlaylistImportIntent::AppendToQueue,
            ),
            (
                AppendToQueue,
                DropTarget::Video,
                PlaylistImportIntent::AppendToQueue,
            ),
        ];
        for (action, target, expected) in cases {
            assert_eq!(playlist_import_intent(action, target), expected);
        }
    }
}
