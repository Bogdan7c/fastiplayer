//! Выбор между «перезапустить загруженное media» и «открыть заново» при Play строки (B9).

/// Что делать, если Play выбирает элемент, media которого уже загружено в player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveMediaReuse {
    /// Обычный Play строки: перезапустить загруженное media «с начала» без нового install.
    RestartInPlace,
    /// Загруженное media считается непригодным: всегда новый install (B9, повтор после
    /// потери сети посреди просмотра).
    ForceReopen,
}
