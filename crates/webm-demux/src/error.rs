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
}
