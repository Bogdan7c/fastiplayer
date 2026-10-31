//! Плашки «настройки не пишутся в файл» и их снятие (04c, «общая доска»).
//!
//! Две плашки говорят человеку, что его настройки не переживут перезапуск:
//! - стартовое предупреждение итога загрузки config-а (до ×): например, «замок config-а
//!   был недоступен, изменения этого сеанса не сохранятся»;
//! - временная «Settings applied for this session only…» после каждого изменения в режиме
//!   «только на сеанс».
//!
//! Режим «только на сеанс» больше не пожизненный: когда «общая доска» прочитала
//! нормальный файл, восстановимый запрет снимается, и изменения снова пишутся. С этого
//! момента обе плашки — неправда, поэтому центр помнит их id и снимает по одному intent-
//! вызову. Остальные плашки (ошибки, «не сохранено сейчас» с причиной) не трогаются.
//!
//! Стартовое предупреждение без запрета записи (файл восстановлен в `.bak`, запись идёт)
//! тоже запоминается, но снятие зовёт только снятие запрета, а его без запрета не бывает.

use std::sync::Arc;
use std::time::Instant;

use super::{NotificationCenter, NotificationId};
use crate::config_startup_notice::SETTINGS_KEPT_FOR_SESSION_ONLY_MESSAGE;

impl NotificationCenter {
    /// Стартовое предупреждение итога загрузки config-а: висит до ×.
    pub(super) fn notify_config_startup_warning(
        &mut self,
        message: impl Into<Arc<str>>,
        now: Instant,
    ) -> NotificationId {
        let id = self.notify_until_dismissed(message, now);
        self.remember_settings_file_write_notice(id);
        id
    }

    /// «Настройка применена только на этот сеанс» (исчезнет сама); повтор продлевает ту же
    /// плашку.
    pub(crate) fn notify_settings_kept_for_session_only(&mut self, now: Instant) -> NotificationId {
        let id = self.notify_transient(SETTINGS_KEPT_FOR_SESSION_ONLY_MESSAGE, now);
        self.remember_settings_file_write_notice(id);
        id
    }

    /// Снимает плашки «настройки не пишутся в файл»: запись в файл снова разрешена.
    ///
    /// Возвращает, сколько плашек было на экране (уже закрытые × или истёкшие не считаются).
    pub(crate) fn withdraw_settings_file_write_notices(&mut self) -> usize {
        let withdrawn_ids = std::mem::take(&mut self.settings_file_write_notices);
        let visible_before = self.toasts.len();
        self.toasts
            .retain(|toast| !withdrawn_ids.contains(&toast.id));
        visible_before - self.toasts.len()
    }

    /// Запоминает id без дублей (повтор той же плашки возвращает тот же id).
    fn remember_settings_file_write_notice(&mut self, id: NotificationId) {
        if !self.settings_file_write_notices.contains(&id) {
            self.settings_file_write_notices.push(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::state::notifications::OpenProgress;
    use crate::ui::animation::UiMotion;

    use super::*;

    fn visible_messages(center: &mut NotificationCenter, now: Instant) -> Vec<String> {
        center
            .frame(OpenProgress::Idle, UiMotion::Standard, now)
            .toasts
            .iter()
            .map(|toast| toast.message.to_string())
            .collect()
    }

    /// Стартовое предупреждение и «только на сеанс» снимаются одним вызовом, чужая плашка
    /// остаётся; повторный вызов ничего не снимает.
    #[test]
    fn session_only_notices_are_withdrawn_and_other_toasts_stay() {
        let now = Instant::now();
        let mut center = NotificationCenter::with_startup_messages(
            None,
            Some("Changes made during this session will not be saved.".to_string()),
            now,
        );
        center.notify_settings_kept_for_session_only(now);
        center.notify_settings_kept_for_session_only(now);
        center.notify_info("Unrelated notice", now);
        assert_eq!(visible_messages(&mut center, now).len(), 3);

        let withdrawn = center.withdraw_settings_file_write_notices();
        let withdrawn_again = center.withdraw_settings_file_write_notices();

        assert_eq!(withdrawn, 2);
        assert_eq!(withdrawn_again, 0);
        assert_eq!(visible_messages(&mut center, now), vec!["Unrelated notice"]);
    }

    /// Плашка, закрытая человеком раньше, не мешает снятию остальных.
    #[test]
    fn already_dismissed_notice_is_not_counted() {
        let now = Instant::now();
        let mut center = NotificationCenter::default();
        let banner = center.notify_settings_kept_for_session_only(now);
        let _dismissed = center.dismiss(banner);

        assert_eq!(center.withdraw_settings_file_write_notices(), 0);
        assert!(visible_messages(&mut center, now).is_empty());
    }
}
