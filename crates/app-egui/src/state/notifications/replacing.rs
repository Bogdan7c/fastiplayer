//! Временные toast-ы «с заменой»: повтор по тому же ключу обновляет уже видимую плашку.
//!
//! Нужны для величин, которые пользователь меняет серией нажатий (сдвиг субтитров клавишами
//! Z/X, смена дорожки клавишей V): десять нажатий должны дать одну плашку с актуальным
//! текстом, а не стопку из трёх устаревших. Обычное правило «одинаковый текст не дублируется»
//! тут не помогает, потому что текст каждый раз новый.

use std::sync::Arc;
use std::time::Instant;

use super::{NotificationCenter, NotificationId, ToastEntry, ToastKind};

/// Ключ плашки с заменой: одна плашка на ключ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TransientReplaceKey {
    /// Сдвиг синхронизации субтитров («Subtitle delay: +0.3 s»).
    SubtitleSync,
    /// Выбранная дорожка субтитров («Subtitles: Russian (SRT, file)»).
    SubtitleTrack,
}

impl NotificationCenter {
    /// Показывает временное уведомление, заменяющее прежнее с тем же ключом.
    ///
    /// Если плашка с ключом уже видна, обновляются только её текст и срок жизни (снова
    /// полные 5 с); id, момент появления и место в стопке сохраняются, поэтому плашка не
    /// мигает повторным появлением. Остальные плашки не затрагиваются.
    pub(crate) fn notify_transient_replacing(
        &mut self,
        key: TransientReplaceKey,
        message: impl Into<Arc<str>>,
        now: Instant,
    ) -> NotificationId {
        let message = message.into();
        let expires_at = ToastKind::Transient
            .lifetime()
            .map(|lifetime| now + lifetime);
        if let Some(existing) = self
            .toasts
            .iter_mut()
            .find(|toast| toast.replace_key == Some(key))
        {
            existing.message = message;
            existing.expires_at = expires_at;
            return existing.id;
        }
        let id = self.allocate_id();
        self.toasts.push_front(ToastEntry {
            id,
            kind: ToastKind::Transient,
            message,
            shown_at: now,
            expires_at,
            replace_key: Some(key),
        });
        self.evict_overflow();
        id
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::state::notifications::{OpenProgress, TRANSIENT_NOTIFICATION_LIFETIME};
    use crate::ui::animation::UiMotion;

    fn messages(center: &mut NotificationCenter, now: Instant) -> Vec<String> {
        center
            .frame(OpenProgress::Idle, UiMotion::Standard, now)
            .toasts
            .iter()
            .map(|toast| toast.message.to_string())
            .collect()
    }

    #[test]
    fn same_key_replaces_text_and_keeps_a_single_toast() {
        let start = Instant::now();
        let mut center = NotificationCenter::default();
        let first = center.notify_transient_replacing(
            TransientReplaceKey::SubtitleSync,
            "Subtitle delay: +0.1 s",
            start,
        );
        let second = center.notify_transient_replacing(
            TransientReplaceKey::SubtitleSync,
            "Subtitle delay: +0.2 s",
            start + Duration::from_secs(1),
        );
        assert_eq!(first, second, "the toast identity is kept");
        assert_eq!(
            messages(&mut center, start + Duration::from_secs(1)),
            ["Subtitle delay: +0.2 s"]
        );
    }

    #[test]
    fn different_keys_stack_independently() {
        let start = Instant::now();
        let mut center = NotificationCenter::default();
        center.notify_transient_replacing(TransientReplaceKey::SubtitleSync, "delay", start);
        center.notify_transient_replacing(TransientReplaceKey::SubtitleTrack, "track", start);
        assert_eq!(messages(&mut center, start), ["track", "delay"]);
    }

    #[test]
    fn replacement_restarts_the_lifetime_timer() {
        let start = Instant::now();
        let mut center = NotificationCenter::default();
        center.notify_transient_replacing(TransientReplaceKey::SubtitleSync, "a", start);
        let later = start + Duration::from_secs(4);
        center.notify_transient_replacing(TransientReplaceKey::SubtitleSync, "b", later);
        // Старый срок (start + 5 с) прошёл бы, новый — нет.
        let after_old_deadline = start + TRANSIENT_NOTIFICATION_LIFETIME + Duration::from_secs(1);
        assert_eq!(messages(&mut center, after_old_deadline), ["b"]);
        assert_eq!(
            center.next_wake_deadline(),
            Some(later + TRANSIENT_NOTIFICATION_LIFETIME)
        );
        let after_new_deadline = later + TRANSIENT_NOTIFICATION_LIFETIME;
        assert!(messages(&mut center, after_new_deadline).is_empty());
    }

    #[test]
    fn expired_keyed_toast_is_created_again_instead_of_resurrected() {
        let start = Instant::now();
        let mut center = NotificationCenter::default();
        let first =
            center.notify_transient_replacing(TransientReplaceKey::SubtitleSync, "a", start);
        let after_expiry = start + TRANSIENT_NOTIFICATION_LIFETIME + Duration::from_millis(1);
        assert!(messages(&mut center, after_expiry).is_empty());
        let second =
            center.notify_transient_replacing(TransientReplaceKey::SubtitleSync, "b", after_expiry);
        assert_ne!(first, second);
        assert_eq!(messages(&mut center, after_expiry), ["b"]);
    }

    #[test]
    fn replacement_does_not_touch_other_toasts() {
        let start = Instant::now();
        let mut center = NotificationCenter::default();
        center.notify_info("info", start);
        center.notify_transient("plain", start);
        center.notify_transient_replacing(TransientReplaceKey::SubtitleTrack, "t1", start);
        center.notify_transient_replacing(TransientReplaceKey::SubtitleTrack, "t2", start);
        assert_eq!(messages(&mut center, start), ["t2", "plain", "info"]);
    }
}
