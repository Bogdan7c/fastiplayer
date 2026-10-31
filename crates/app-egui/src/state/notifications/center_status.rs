//! Сообщение состояния окна в центре (мост, сессия 04b).
//!
//! Не уведомление о событии, а описание того, в каком состоянии окно: «вкладка закрыта —
//! скачанное можно досмотреть», «вкладка закрыта» перед закрытием окна. Поэтому у него
//! нет ни срока, ни кнопки ×: оно висит, пока владелец состояния (окно браузерного
//! режима) его не снимет или не заменит. Одно на окно; новый текст заменяет прежний.
//!
//! Правило центра — в [`NotificationCenter::frame`]: прогресс открытия важнее, ошибку
//! media оно перекрывает (объясняет, что окно сделает дальше), спиннер под ним не
//! рисуется.

use std::sync::Arc;

use super::NotificationCenter;

/// Изменилось ли сообщение (вызывающему — нужна ли перерисовка).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CenterStatusUpdate {
    /// Показано новое, заменено или снято.
    Changed,
    /// Уже было ровно таким: ничего не изменилось.
    Unchanged,
}

impl NotificationCenter {
    /// Ставит (или заменяет) сообщение состояния окна.
    pub(crate) fn show_center_status(&mut self, message: &str) -> CenterStatusUpdate {
        if self.center_status.as_deref() == Some(message) {
            return CenterStatusUpdate::Unchanged;
        }
        self.center_status = Some(Arc::from(message));
        CenterStatusUpdate::Changed
    }

    /// Снимает сообщение состояния окна (его не было — ничего не меняется).
    pub(crate) fn clear_center_status(&mut self) -> CenterStatusUpdate {
        match self.center_status.take() {
            Some(_) => CenterStatusUpdate::Changed,
            None => CenterStatusUpdate::Unchanged,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::state::notifications::{CenterNotice, MediaFailureOrigin, OpenProgress};
    use crate::ui::animation::UiMotion;

    fn center(
        notifications: &mut NotificationCenter,
        progress: OpenProgress<'_>,
    ) -> Option<CenterNotice> {
        notifications
            .frame(progress, UiMotion::Standard, Instant::now())
            .center
    }

    #[test]
    fn the_status_stays_until_its_owner_clears_it() {
        let mut notifications = NotificationCenter::default();
        assert_eq!(
            notifications.show_center_status("Source tab closed"),
            CenterStatusUpdate::Changed
        );
        for _ in 0..3 {
            assert_eq!(
                center(&mut notifications, OpenProgress::Idle),
                Some(CenterNotice::Status(Arc::from("Source tab closed")))
            );
        }
        assert_eq!(
            notifications.show_center_status("Source tab closed"),
            CenterStatusUpdate::Unchanged
        );
        assert_eq!(
            notifications.clear_center_status(),
            CenterStatusUpdate::Changed
        );
        assert_eq!(center(&mut notifications, OpenProgress::Idle), None);
        assert_eq!(
            notifications.clear_center_status(),
            CenterStatusUpdate::Unchanged
        );
    }

    #[test]
    fn a_new_text_replaces_the_old_one() {
        let mut notifications = NotificationCenter::default();
        notifications
            .show_center_status("Source tab closed — you can still watch what was downloaded");
        assert_eq!(
            notifications.show_center_status("Source tab closed"),
            CenterStatusUpdate::Changed
        );
        assert_eq!(
            center(&mut notifications, OpenProgress::Idle),
            Some(CenterNotice::Status(Arc::from("Source tab closed")))
        );
    }

    /// Состояние окна важнее ошибки media, но уступает прогрессу открытия; ошибка под
    /// ним не теряется и видна снова, когда состояние снято.
    #[test]
    fn the_status_covers_a_media_failure_but_yields_to_an_open_in_progress() {
        let mut notifications = NotificationCenter::default();
        notifications.show_media_failure("decoder died", MediaFailureOrigin::PlayerFailed);
        notifications.show_center_status("Source tab closed — playback failed");
        assert_eq!(
            center(&mut notifications, OpenProgress::Idle),
            Some(CenterNotice::Status(Arc::from(
                "Source tab closed — playback failed"
            )))
        );
        assert!(matches!(
            center(&mut notifications, OpenProgress::InProgress("Opening…")),
            Some(CenterNotice::Progress(_))
        ));
        notifications.clear_center_status();
        assert!(matches!(
            center(&mut notifications, OpenProgress::Idle),
            Some(CenterNotice::MediaFailure(_))
        ));
    }

    /// Снятие состояния не трогает чужое: toast-ы и ошибка остаются.
    #[test]
    fn clearing_the_status_keeps_toasts_and_the_failure() {
        let mut notifications = NotificationCenter::default();
        let now = Instant::now();
        notifications.notify_info("Subtitles loaded", now);
        notifications.show_media_failure("decoder died", MediaFailureOrigin::PlayerFailed);
        notifications.show_center_status("Source tab closed");
        notifications.clear_center_status();
        let frame = notifications.frame(OpenProgress::Idle, UiMotion::Standard, now);
        assert_eq!(frame.toasts.len(), 1);
        assert!(matches!(frame.center, Some(CenterNotice::MediaFailure(_))));
    }
}
