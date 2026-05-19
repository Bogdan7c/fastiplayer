use std::fmt;

/// Result alias для операций player-core.
pub type PlayerResult<T> = Result<T, PlayerError>;

/// Категория ошибки player state machine или media pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerErrorKind {
    /// Video codec не поддерживается текущей сборкой.
    UnsupportedVideoCodec,

    /// Профиль video codec не поддерживается аппаратным backend.
    UnsupportedVideoProfile,

    /// Bit depth video stream не поддерживается текущей production policy.
    UnsupportedVideoBitDepth,

    /// Chroma subsampling video stream не поддерживается текущей production policy.
    UnsupportedVideoChroma,

    /// HDR-режим пока не может быть корректно отображён.
    UnsupportedHdrMode,

    /// Audio codec не поддерживается текущей audio decoder factory.
    UnsupportedAudioCodec,

    /// Renderer backend не умеет принять формат кадра после decode.
    UnsupportedRenderFormat,

    /// Hardware decoder недоступен или не прошёл probe.
    HardwareDecoderUnavailable,

    /// Ошибка чтения контейнера или packets.
    DemuxError,

    /// Текущий media/source не поддерживает seek.
    SeekUnavailable,

    /// Seek transaction не дождался commit gates за отведённое время.
    SeekTimeout,

    /// Decoder thread не смог сбросить старое state перед seek transaction.
    DecoderFlushFailed,

    /// Ошибка сетевого источника или streaming manifest.
    NetworkError,

    /// Audio device недоступен или отказал во время playback.
    AudioDeviceUnavailable,

    /// Render device потерян или не может принять кадр.
    RenderDeviceLost,

    /// Ошибка чтения или валидации config.
    ConfigError,

    /// Runtime-ошибка, которую старый app layer пока хранит только строкой.
    RuntimeError,

    /// Некорректная команда для текущего состояния session.
    InvalidCommand,
}

/// Ошибка плеера с категорией и человекочитаемым сообщением.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerError {
    /// Машиночитаемая категория ошибки.
    pub kind: PlayerErrorKind,

    /// Сообщение для логов и UI.
    pub message: String,
}

impl PlayerError {
    /// Создаёт новую ошибку с явной категорией.
    #[must_use]
    pub fn new(kind: PlayerErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for PlayerError {
    /// Форматирует ошибку без потери категории.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for PlayerError {}
