//! Какое из загруженных медиа сейчас показывает плеер (03b, пакет H).
//!
//! Зачем: `BrowserPlayback::load` только ставит медиа в очередь установки — плеер
//! ставит его асинхронно. До установки снимок плеера всё ещё описывает ПРЕЖНЕЕ медиа
//! (позиция, состояние). Медиа-часть цикла не должна приписывать эту позицию новому
//! поколению потока: монитор нового поколения по ней выбрасывает сегменты полки и
//! считает «буферизовано вперёд», а страница получила бы часы старого ролика.
//!
//! Правило: каждая загрузка получает [`MediaLoadTicket`]. Когда плеер сообщает
//! «установлено» (квитанция установки отдаёт `MediaInstanceId`), трекер запоминает
//! пару «квитанция → экземпляр медиа». Наблюдение относится к загрузке, только если
//! снимок плеера показывает ИМЕННО этот экземпляр. Пока нет — загрузка «ставится».
//!
//! Идентичность плеера (`MediaInstanceId`) наружу не выходит: медиа-часть сравнивает
//! только свои квитанции через [`super::PlaybackObservation::shows`].

use player_core::MediaInstanceId;

/// Квитанция одной загрузки медиа в плеер браузерного режима.
///
/// Выдаётся [`MediaLoadTicketIssuer`]; две загрузки никогда не получают одну
/// квитанцию (в пределах одного выдающего).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MediaLoadTicket(u64);

/// Выдаёт квитанции загрузок по порядку: 1, 2, 3…
#[derive(Debug, Default)]
pub(crate) struct MediaLoadTicketIssuer {
    /// Сколько квитанций уже выдано.
    issued: u64,
}

impl MediaLoadTicketIssuer {
    /// Свежая квитанция для очередной загрузки.
    pub(crate) fn issue(&mut self) -> MediaLoadTicket {
        // 2⁶⁴ загрузок за жизнь процесса недостижимо; насыщение — только защита от паники.
        self.issued = self.issued.saturating_add(1);
        MediaLoadTicket(self.issued)
    }
}

/// Загрузка, которую плеер уже установил.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InstalledLoad {
    /// Квитанция загрузки.
    ticket: MediaLoadTicket,
    /// Экземпляр медиа, который плеер для неё установил.
    instance: MediaInstanceId,
}

/// Связь «последняя установленная загрузка ↔ экземпляр медиа плеера».
#[derive(Debug, Default)]
pub(crate) struct ShownMediaTracker {
    /// Последняя установленная загрузка (более ранние плеер уже закрыл).
    installed: Option<InstalledLoad>,
}

impl ShownMediaTracker {
    /// Плеер установил медиа загрузки `ticket` как экземпляр `instance`.
    pub(crate) fn note_installed(&mut self, ticket: MediaLoadTicket, instance: MediaInstanceId) {
        self.installed = Some(InstalledLoad { ticket, instance });
    }

    /// Чью загрузку показывает снимок плеера с экземпляром `snapshot_instance`.
    ///
    /// `None` — снимок не про установленную загрузку: медиа ещё нет, плеер ещё
    /// показывает прежний экземпляр или уже показывает новый, о котором квитанция
    /// установки пока не сообщила (тогда ответ станет известен на следующем шаге).
    pub(crate) fn shown_load(
        &self,
        snapshot_instance: Option<MediaInstanceId>,
    ) -> Option<MediaLoadTicket> {
        let installed = self.installed?;
        (snapshot_instance == Some(installed.instance)).then_some(installed.ticket)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    fn instance(raw: u64) -> MediaInstanceId {
        MediaInstanceId::from_non_zero(NonZeroU64::new(raw).expect("non-zero"))
    }

    #[test]
    fn issued_tickets_are_distinct() {
        let mut issuer = MediaLoadTicketIssuer::default();
        let first = issuer.issue();
        let second = issuer.issue();
        assert_ne!(first, second);
    }

    #[test]
    fn nothing_is_shown_before_any_install() {
        let tracker = ShownMediaTracker::default();
        assert_eq!(tracker.shown_load(Some(instance(1))), None);
        assert_eq!(tracker.shown_load(None), None);
    }

    #[test]
    fn only_the_installed_instance_is_attributed_to_its_load() {
        let mut issuer = MediaLoadTicketIssuer::default();
        let first = issuer.issue();
        let second = issuer.issue();
        let mut tracker = ShownMediaTracker::default();

        tracker.note_installed(first, instance(7));
        assert_eq!(tracker.shown_load(Some(instance(7))), Some(first));

        // Вторая загрузка установлена, а снимок ещё про первый экземпляр — ничья.
        tracker.note_installed(second, instance(8));
        assert_eq!(tracker.shown_load(Some(instance(7))), None);
        assert_eq!(tracker.shown_load(None), None);
        assert_eq!(tracker.shown_load(Some(instance(8))), Some(second));
    }
}
