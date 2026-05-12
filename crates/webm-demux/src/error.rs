use std::path::PathBuf;

/// Ошибки demuxer.
#[derive(Debug, thiserror::Error)]
pub enum DemuxError {
    #[error("Файл не найден: {0}")]
    FileNotFound(PathBuf),

    #[error("Неподдерживаемый формат: {0}")]
    UnsupportedFormat(String),

    #[error("Нет видео треков в файле")]
    NoVideoTracks,

    #[error("Нет аудио треков в файле")]
    NoAudioTracks,

    #[error("Ошибка чтения: {0}")]
    Io(#[from] std::io::Error),

    #[error("Ошибка парсинга: {0}")]
    Parse(#[from] symphonia::core::errors::Error),

    #[error("Seek недоступен: {0}")]
    SeekUnavailable(String),

    #[error("Ошибка seek: {0}")]
    SeekFailed(String),
}

impl DemuxError {
    /// Возвращает `true`, если ошибка означает отсутствие seek capability.
    #[must_use]
    pub fn is_seek_unavailable(&self) -> bool {
        matches!(self, Self::SeekUnavailable(_))
    }
}
