//! Какой элемент очереди запускает Play, когда в плеере ничего не установлено (A15-live).
//!
//! Без этого Play при непустой очереди, но без открытого media, уходил в player как
//! «просто Play» и ничего не делал: пользователь нажимал кнопку и не получал ни открытия,
//! ни объяснения.

use playlist_core::PlaylistItemId;

use super::PlaylistController;

impl PlaylistController {
    /// Элемент, с которого Play начинает воспроизведение «из пустого плеера».
    ///
    /// Правило владельца: если у очереди есть `current` (куда она дошла или где остановилась)
    /// — он; иначе первый играбельный элемент. `None` означает, что Play этим путём вести
    /// нельзя: очередь пуста, что-то уже установлено, открывается или ждёт установки — тогда
    /// Play остаётся обычным переключением паузы/воспроизведения.
    pub(crate) fn idle_play_target(&self) -> Option<PlaylistItemId> {
        if self.active_media.is_some()
            || self.install_state.is_some()
            || self.pending_target.is_some()
        {
            return None;
        }
        self.queue
            .traversal_current()
            .map(|current| current.item_id())
            .filter(|current_id| self.queue.item(*current_id).is_some())
            .or_else(|| self.queue.iter_playable_ids().next())
    }
}

#[cfg(test)]
mod tests {
    use playlist_core::{
        CachedPlaylistMetadata, PlaylistItemDraft, PlaylistMediaKind, SecretUrlLocator,
    };

    use super::*;
    use crate::playlist_runtime::TransportActionOrigin;
    use crate::playlist_runtime::controller::{ControllerAppendOutcome, ControllerPlayItemOutcome};
    use crate::playlist_runtime::skip_notice_regressions::install_planned;

    fn controller_with_files(names: &[&str]) -> (PlaylistController, Vec<PlaylistItemId>) {
        let mut controller = PlaylistController::new();
        let drafts = names
            .iter()
            .map(|name| {
                PlaylistItemDraft::url(
                    SecretUrlLocator::from_reopenable_url(format!("http://media.test/{name}.mp4"))
                        .expect("valid test URL"),
                    CachedPlaylistMetadata::new(format!("clip {name}"), PlaylistMediaKind::Video),
                )
            })
            .collect();
        let ControllerAppendOutcome::Added { item_ids, .. } =
            controller.append(drafts).expect("append")
        else {
            panic!("очередь не пустая");
        };
        (controller, item_ids)
    }

    /// Без current Play идёт с первого элемента; пустая очередь цели не даёт.
    #[test]
    fn target_is_first_item_without_current_and_absent_for_empty_queue() {
        let (controller, item_ids) = controller_with_files(&["a", "b"]);
        assert_eq!(controller.idle_play_target(), Some(item_ids[0]));
        assert_eq!(PlaylistController::new().idle_play_target(), None);
    }

    /// Current очереди (media сброшено, current остался на B) важнее первого элемента.
    #[test]
    fn target_prefers_queue_current_over_first_item() {
        let (mut controller, item_ids) = controller_with_files(&["a", "b", "c"]);
        let ControllerPlayItemOutcome::StartInstall { install, .. } =
            controller.play_item(item_ids[1], TransportActionOrigin::Ui)
        else {
            panic!("B стартует install");
        };
        install_planned(&mut controller, install, 10);
        assert_eq!(controller.idle_play_target(), None, "media установлено");

        controller.active_media = None;

        assert_eq!(controller.idle_play_target(), Some(item_ids[1]));
    }
}
