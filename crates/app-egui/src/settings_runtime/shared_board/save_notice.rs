//! Уведомление «изменение применено, но в файл пока не записано» (решение 135).
//!
//! Запись под замком может не удаться (файл занят соседом, битый, от новой версии,
//! ошибка диска). Тогда `AppConfigStore` не откатывает транзакцию, а держит изменение в
//! памяти и допишет его при следующей записи (`settings_file_persist_outcome` →
//! `NotSavedYet { notice }`). Человеку об этом нужно сказать, но без спама: громкость и
//! ширина сайдбара пишутся сами каждые несколько секунд, и каждая такая запись упрётся в
//! ту же причину. Поэтому уведомление повторяется только при смене причины, а успешная
//! запись закрывает эпизод (следующий сбой — снова одно уведомление).
//!
//! Режим «запись запрещена на весь запуск» (`SessionOnly`) сюда не относится: его плашку
//! показывает хост как раньше (`report_settings_kept_for_session_only`).

use std::sync::Arc;

use fastiplayer_settings::{SettingsFilePersistOutcome, settings_file_persist_outcome};
use settings_core::PersistReport;

/// Что сказать окну после одной попытки записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::settings_runtime) enum PersistNoticeRoute {
    /// Запись запрещена на весь запуск: показать прежнюю плашку «только на сеанс».
    SessionOnlyBanner,
    /// Ничего показывать не нужно (записано, нечего писать или причина уже показана).
    Quiet,
}

/// Учёт «уже сказали» и одно ждущее показа уведомление.
#[derive(Debug, Default)]
pub(in crate::settings_runtime) struct SaveDeferralNotices {
    /// Текст причины, о которой уже сообщили в текущем эпизоде неудачных записей.
    announced: Option<Arc<str>>,
    /// Новое уведомление, которое ещё не забрал кадр.
    pending: Option<Arc<str>>,
}

impl SaveDeferralNotices {
    /// Разбирает отчёт записи `AppConfigStore`.
    pub(in crate::settings_runtime) fn record_persist_report(
        &mut self,
        report: &PersistReport,
    ) -> PersistNoticeRoute {
        match settings_file_persist_outcome(report) {
            SettingsFilePersistOutcome::Saved
            | SettingsFilePersistOutcome::SavedDurabilityUnconfirmed { .. } => {
                self.note_saved();
                PersistNoticeRoute::Quiet
            }
            SettingsFilePersistOutcome::NotSavedYet { notice } => {
                self.note_not_saved_yet(notice);
                PersistNoticeRoute::Quiet
            }
            SettingsFilePersistOutcome::SessionOnly => PersistNoticeRoute::SessionOnlyBanner,
            SettingsFilePersistOutcome::NothingToSave => PersistNoticeRoute::Quiet,
        }
    }

    /// Изменения дошли до файла: эпизод сбоев закрыт.
    pub(in crate::settings_runtime) fn note_saved(&mut self) {
        self.announced = None;
        self.pending = None;
    }

    /// Запись снова не удалась; сообщить, только если причина новая.
    pub(in crate::settings_runtime) fn note_not_saved_yet(&mut self, notice: &str) {
        if self.announced.as_deref() == Some(notice) {
            return;
        }
        let notice: Arc<str> = Arc::from(notice);
        self.announced = Some(notice.clone());
        self.pending = Some(notice);
    }

    /// Забирает уведомление для показа ровно один раз.
    pub(in crate::settings_runtime) fn take_pending(&mut self) -> Option<Arc<str>> {
        self.pending.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deferred(notice: &str) -> PersistReport {
        PersistReport::kept_for_session_only().with_durability_warning(notice.to_string())
    }

    #[test]
    fn same_reason_is_announced_once_until_a_save_succeeds() {
        let mut notices = SaveDeferralNotices::default();

        assert_eq!(
            notices.record_persist_report(&deferred("file busy")),
            PersistNoticeRoute::Quiet
        );
        assert_eq!(notices.take_pending().as_deref(), Some("file busy"));
        let _ = notices.record_persist_report(&deferred("file busy"));
        assert_eq!(notices.take_pending(), None, "no spam for the same reason");

        let _ = notices.record_persist_report(&deferred("file broken"));
        assert_eq!(notices.take_pending().as_deref(), Some("file broken"));

        let _ = notices.record_persist_report(&PersistReport::persisted());
        let _ = notices.record_persist_report(&deferred("file broken"));
        assert_eq!(
            notices.take_pending().as_deref(),
            Some("file broken"),
            "a new episode after a successful save is announced again"
        );
    }

    #[test]
    fn session_only_keeps_the_existing_banner() {
        let mut notices = SaveDeferralNotices::default();

        let route = notices.record_persist_report(&PersistReport::kept_for_session_only());

        assert_eq!(route, PersistNoticeRoute::SessionOnlyBanner);
        assert_eq!(notices.take_pending(), None);
    }
}
