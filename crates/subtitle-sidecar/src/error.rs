//! Фатальные ошибки поиска. Нефатальные проблемы (нечитаемые каталоги, лимиты) — в отчёте.

/// Поиск не может быть выполнен или прерван.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SidecarSearchError {
    /// У пути видео нет родительского каталога (например, корень `/`).
    #[error("video path has no parent directory")]
    VideoPathHasNoParent,
    /// У пути видео нет имени файла, из которого можно взять основу (stem).
    #[error("video path has no file name to derive a stem from")]
    VideoPathHasNoFileStem,
    /// Вызывающий код отменил поиск (например, пользователь открыл другое видео).
    #[error("sidecar subtitle search was cancelled")]
    Cancelled,
}
