//! Фатальные ошибки загрузки: файл отвергается целиком.
//!
//! Проблемы отдельных реплик ошибками НЕ являются — они попадают в `SubtitleParseReport`.

/// Причина, по которой файл субтитров не загружен вообще.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SubtitleLoadError {
    /// Входные байты больше `max_input_bytes`; декодирование даже не начиналось.
    #[error("subtitle file is too large ({actual_bytes} bytes, limit {limit_bytes} bytes)")]
    InputTooLarge {
        actual_bytes: usize,
        limit_bytes: usize,
    },
    /// В файле больше событий, чем разрешает `max_events`.
    #[error("subtitle file has more than {limit} cues")]
    TooManyEvents { limit: usize },
    /// Загружается ASS/SSA, а лимиты ASS не заданы (`SubtitleParseLimits::with_ass_limits`).
    #[error("ASS parse limits are required to load ASS/SSA subtitles")]
    AssLimitsMissing,
    /// ASS/SSA: часть файла до секции `[Events]` (заголовок и стили) больше лимита
    /// `max_header_bytes` (тот же `subtitles.max_ass_header_kb`, что и у встроенных дорожек).
    #[error("ASS header is too large ({actual_bytes} bytes, limit {limit_bytes} bytes)")]
    AssHeaderTooLarge {
        actual_bytes: usize,
        limit_bytes: usize,
    },
    /// Файл заявлен как WebVTT, но не начинается с сигнатуры `WEBVTT`.
    #[error("file does not start with the WEBVTT signature")]
    NotWebVtt,
    /// srv3: файл не является корректным XML (или использует запрещённые конструкции:
    /// DOCTYPE, пользовательские сущности). Содержимое файла в сообщение не попадает.
    #[error("srv3 file is not well-formed XML")]
    MalformedSrv3,
    /// srv3: XML корректен, но корень не `timedtext` формата 3 (например, srv1/srv2).
    #[error("file is not a srv3 (timedtext format 3) document")]
    NotSrv3,
    /// srv3: структура XML превысила лимиты (глубина, число токенов, атрибутов или текста).
    #[error("srv3 file exceeds the XML structure limits")]
    Srv3TooComplex,
}
