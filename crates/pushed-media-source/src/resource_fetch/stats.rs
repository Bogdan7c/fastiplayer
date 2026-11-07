//! Учёт очереди для журнала: только числа, без содержимого и номерков.

use super::writer::ResourceFetchFailure;

/// Снимок учёта очереди.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceFetchStats {
    /// Сколько запросов получили номер.
    pub issued: u64,
    /// Сколько запросов приложение забрало для отправки.
    pub sent: u64,
    /// Сколько запросов внешняя сторона завершила успешно (полностью или коротким
    /// диапазоном).
    pub completed: u64,
    /// Отказы по классам.
    pub failures: ResourceFailureCounts,
    /// Сколько запросов движок бросил до итога внешней стороны.
    pub abandoned: u64,
    /// Сколько байтов тел принято.
    pub bytes_accepted: u64,
    /// Сколько устаревших кусков и итогов отброшено молча.
    pub stale_frames: u64,
}

/// Отказы ресурсов по классам.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceFailureCounts {
    /// Сбой сети.
    pub network: u64,
    /// HTTP-ошибка.
    pub http: u64,
    /// Отказ внешней стороны.
    pub refused: u64,
    /// Ресурс больше потолка.
    pub too_large: u64,
    /// Внешняя сторона молчала дольше срока.
    pub page_silent: u64,
    /// Диапазон пришёл короче запрошенного.
    pub short_range: u64,
    /// Нарушения договора (больше одного не бывает: первое убивает очередь).
    pub page_violations: u64,
}

impl ResourceFetchStats {
    /// Учитывает отказ, о котором сообщила внешняя сторона.
    pub(super) const fn count_page_failure(&mut self, failure: ResourceFetchFailure) {
        let counter = match failure {
            ResourceFetchFailure::Network => &mut self.failures.network,
            ResourceFetchFailure::Http { .. } => &mut self.failures.http,
            ResourceFetchFailure::Refused => &mut self.failures.refused,
            ResourceFetchFailure::TooLarge => &mut self.failures.too_large,
        };
        *counter = counter.saturating_add(1);
    }
}

/// `value += 1` без переполнения: учёт не должен ронять очередь.
pub(super) const fn bump(value: &mut u64) {
    *value = value.saturating_add(1);
}
