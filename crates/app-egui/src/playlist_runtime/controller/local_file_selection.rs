//! Классификация главной команды «Открыть файл» относительно committed queue.

use std::path::Path;

use playlist_core::PlaylistItemId;

use super::PlaylistController;

/// Причина, по которой выбранный path нельзя переиспользовать как существующую строку.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalFileQueueReplacementReason {
    /// В очереди нет current native-local строки, задающей текущий каталог.
    NoCurrentLocalDirectory,
    /// Выбранный файл находится в другом lexical parent-каталоге.
    DifferentDirectory,
    /// Каталог тот же, но exact path ещё не committed в очередь.
    SameDirectoryItemNotCommitted,
}

/// Intent-результат классификации выбора из главного file picker-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalFileSelectionDisposition {
    /// Exact Item ID уже принадлежит текущей очереди и может пройти обычный Row Play.
    PlayCommittedItem { item_id: PlaylistItemId },
    /// Выбор требует существующего atomic queue-replacement workflow.
    ReplaceQueue {
        reason: LocalFileQueueReplacementReason,
    },
}

impl PlaylistController {
    /// Сравнивает выбранный native path с каталогом current committed item без filesystem I/O.
    pub(crate) fn classify_local_file_selection(
        &self,
        selected_path: &Path,
    ) -> LocalFileSelectionDisposition {
        let Some(current_item_id) = self
            .active_media
            .and_then(|active| active.item_id())
            .or_else(|| {
                self.queue
                    .traversal_current()
                    .map(|current| current.item_id())
            })
        else {
            return replacement(LocalFileQueueReplacementReason::NoCurrentLocalDirectory);
        };

        let Some(current_path) = self
            .queue
            .item(current_item_id)
            .and_then(|item| item.locator().as_local())
            .and_then(playlist_core::LocalLocator::expose_native_path_for_open)
        else {
            return replacement(LocalFileQueueReplacementReason::NoCurrentLocalDirectory);
        };

        let Some(selected_parent) = selected_path.parent() else {
            return replacement(LocalFileQueueReplacementReason::DifferentDirectory);
        };
        let Some(current_parent) = current_path.parent() else {
            return replacement(LocalFileQueueReplacementReason::NoCurrentLocalDirectory);
        };
        if selected_parent != current_parent {
            return replacement(LocalFileQueueReplacementReason::DifferentDirectory);
        }

        self.queue
            .iter_playable_items()
            .find_map(|item| {
                item.locator()
                    .as_local()
                    .and_then(playlist_core::LocalLocator::expose_native_path_for_open)
                    .filter(|committed_path| *committed_path == selected_path)
                    .map(|_| LocalFileSelectionDisposition::PlayCommittedItem {
                        item_id: item.item_id(),
                    })
            })
            .unwrap_or_else(|| {
                replacement(LocalFileQueueReplacementReason::SameDirectoryItemNotCommitted)
            })
    }
}

/// Строит typed replacement outcome без позиционного `bool` в callsite-ах.
const fn replacement(reason: LocalFileQueueReplacementReason) -> LocalFileSelectionDisposition {
    LocalFileSelectionDisposition::ReplaceQueue { reason }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use playlist_core::{
        CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind,
    };

    use super::*;

    /// Создаёт минимальную native-local строку для focused classification tests.
    fn local_draft(path: &str) -> PlaylistItemDraft {
        PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from(path)),
            None,
            CachedPlaylistMetadata::new(path, PlaylistMediaKind::Video),
        )
    }

    /// Коммитит строки и назначает указанный индекс current traversal-якорем.
    fn controller_with_current(paths: &[&str], current_index: usize) -> PlaylistController {
        let mut controller = PlaylistController::new();
        let outcome = controller
            .append(paths.iter().map(|path| local_draft(path)).collect())
            .expect("test queue append must succeed");
        let super::super::ControllerAppendOutcome::Added { item_ids, .. } = outcome else {
            panic!("test append must add rows");
        };
        controller
            .queue
            .set_traversal_current(item_ids[current_index])
            .expect("test current must be committed");
        controller
    }

    #[test]
    fn same_directory_committed_path_reuses_exact_item_id() {
        let controller = controller_with_current(&["/media/a/01.mkv", "/media/a/02.mkv"], 0);

        let disposition = controller.classify_local_file_selection(Path::new("/media/a/02.mkv"));

        let LocalFileSelectionDisposition::PlayCommittedItem { item_id } = disposition else {
            panic!("same-directory committed row must be reused");
        };
        assert_eq!(controller.queue.iter_playable_ids().nth(1), Some(item_id));
    }

    #[test]
    fn different_directory_requests_atomic_replacement() {
        let controller = controller_with_current(&["/media/a/01.mkv"], 0);

        assert_eq!(
            controller.classify_local_file_selection(Path::new("/media/b/01.mkv")),
            LocalFileSelectionDisposition::ReplaceQueue {
                reason: LocalFileQueueReplacementReason::DifferentDirectory,
            }
        );
    }

    #[test]
    fn same_directory_uncommitted_path_does_not_forge_an_item_id() {
        let controller = controller_with_current(&["/media/a/01.mkv"], 0);

        assert_eq!(
            controller.classify_local_file_selection(Path::new("/media/a/not-yet-admitted.mkv")),
            LocalFileSelectionDisposition::ReplaceQueue {
                reason: LocalFileQueueReplacementReason::SameDirectoryItemNotCommitted,
            }
        );
    }

    #[test]
    fn empty_queue_has_no_comparable_current_directory() {
        let controller = PlaylistController::new();

        assert_eq!(
            controller.classify_local_file_selection(Path::new("/media/a/01.mkv")),
            LocalFileSelectionDisposition::ReplaceQueue {
                reason: LocalFileQueueReplacementReason::NoCurrentLocalDirectory,
            }
        );
    }
}
