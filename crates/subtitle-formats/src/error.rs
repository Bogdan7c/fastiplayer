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
    /// Файл заявлен как WebVTT, но не начинается с сигнатуры `WEBVTT`.
    #[error("file does not start with the WEBVTT signature")]
    NotWebVtt,
}
