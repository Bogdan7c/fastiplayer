//! Лимиты поиска. Приходят от вызывающего кода; `Default` сознательно нет (как у остальных
//! subtitle-крейтов), чтобы значения не прятались внутри библиотеки.

/// Ограничения на работу поиска. Превышение — не ошибка: сканирование останавливается,
/// а факт попадает в `SidecarSearchReport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidecarSearchLimits {
    /// Сколько записей каталога просматривать в одном каталоге (защита от огромных папок).
    pub max_directory_entries_scanned_per_directory: usize,
    /// Сколько кандидатов оставить в итоговом списке (после упорядочивания).
    pub max_candidates: usize,
    /// Глубина подкаталогов: 0 — только каталог видео, 1 — ещё `Subs`, 2 — ещё `Subs/<Movie>`.
    pub max_subdirectory_depth: u8,
}
