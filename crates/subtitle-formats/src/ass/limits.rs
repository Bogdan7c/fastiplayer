//! Лимиты и ошибки разбора ASS.

/// Пределы разбора ASS. Числа приходят из конфигурации приложения; `Default` намеренно
/// нет (библиотека не решает за пользователя, что «слишком много»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssParseLimits {
    /// Максимальный размер заголовка (`codec_private`) в байтах; проверяется ДО разбора.
    pub max_header_bytes: usize,
    /// Максимум стилей в сценарии (без встроенного). Лишние стили пропускаются и считаются.
    pub max_styles: usize,
    /// Максимум тегов на одну реплику (по всем блокам `{...}`). Лишние теги игнорируются
    /// и считаются в отчёте: враждебная реплика не должна раздуть память и время разбора.
    pub max_override_tags_per_event: usize,
    /// Сколько разных имён неизвестных тегов запоминается в отчёте (остальные только
    /// входят в общий счётчик).
    pub max_tracked_unknown_tag_names: usize,
}

/// Фатальные ошибки разбора заголовка ASS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AssParseError {
    /// Заголовок больше `max_header_bytes`; разбор не начинался.
    #[error("ASS header is too large ({actual_bytes} bytes, limit {limit_bytes} bytes)")]
    HeaderTooLarge {
        actual_bytes: usize,
        limit_bytes: usize,
    },
}
