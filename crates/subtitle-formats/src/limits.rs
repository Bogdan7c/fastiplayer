//! Лимиты разбора: защита от слишком больших и враждебных файлов субтитров.

/// Пределы, которые вызывающий код передаёт загрузчику.
///
/// Конкретные числа приходят из конфигурации приложения (сессия 03), поэтому
/// `Default` намеренно отсутствует: библиотека не решает за пользователя, какой файл «слишком большой».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubtitleParseLimits {
    /// Максимальный размер входных байтов; проверяется ДО декодирования.
    pub max_input_bytes: usize,
    /// Максимальное число событий в одном файле; превышение — фатальная ошибка.
    pub max_events: usize,
    /// Сколько проблем попадает в `SubtitleParseReport::issues` (остальные только считаются).
    pub max_reported_issues: usize,
}

impl SubtitleParseLimits {
    pub const fn new(
        max_input_bytes: usize,
        max_events: usize,
        max_reported_issues: usize,
    ) -> Self {
        Self {
            max_input_bytes,
            max_events,
            max_reported_issues,
        }
    }
}
