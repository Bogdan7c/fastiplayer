use std::path::PathBuf;

use media_core::DemuxSeekMode;

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

    #[error(
        "Слишком много corrupted packets подряд: пропущено {skipped}, лимит {limit}; последняя ошибка: {last_error}"
    )]
    TooManyCorruptedPackets {
        /// Настроенный лимит последовательных corrupted packets.
        limit: usize,

        /// Сколько corrupted packets встретилось подряд фактически.
        skipped: usize,

        /// Последняя причина, которую demuxer посчитал corrupted packet.
        last_error: String,
    },

    #[error("Packet ссылается на неизвестный track id {track_id}")]
    UnknownPacketTrack {
        /// Track id из container packet-а.
        track_id: u32,
    },

    #[error("Demux reset required: dynamic track changes are not supported yet")]
    ResetRequired,

    #[error("Seek недоступен: {0}")]
    SeekUnavailable(String),

    #[error("Seek mode {mode:?} не поддерживается этой реализацией demuxer-а")]
    UnsupportedSeekMode {
        /// Container-level режим, который demuxer не умеет честно выполнить.
        mode: DemuxSeekMode,
    },

    #[error("Ошибка seek: {0}")]
    SeekFailed(String),
}

impl DemuxError {
    /// Возвращает `true`, если ошибка означает отсутствие seek capability.
    #[must_use]
    pub fn is_seek_unavailable(&self) -> bool {
        matches!(
            self,
            Self::SeekUnavailable(_) | Self::UnsupportedSeekMode { .. }
        )
    }
}
