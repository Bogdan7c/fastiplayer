use std::time::Duration;

use bytes::Bytes;

/// Тип трека: видео или аудио.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
}

/// Информация о медиа треке.
///
/// Доступна после открытия файла, до чтения packets.
/// Используется для инициализации декодеров и UI.
#[derive(Debug, Clone)]
pub struct TrackInfo {
    /// Уникальный идентификатор трека в контейнере.
    pub id: u32,

    /// Тип трека: видео или аудио.
    pub kind: TrackKind,

    /// Идентификатор кодека: "V_VP9", "A_OPUS", и т.д.
    pub codec_id: String,

    /// Codec private data (codec-specific initialization data).
    /// Для VP9 может быть пустым. Для Opus содержит OpusHead.
    pub codec_private: Option<Bytes>,

    /// Временная база для конвертации timestamp → Duration.
    pub time_base: Option<TimeBase>,

    /// Длительность трека, если известна из контейнера.
    pub duration: Option<Duration>,

    /// Sample rate для audio треков (Гц). None для video.
    pub sample_rate: Option<u32>,

    /// Количество каналов для audio треков. None для video.
    pub channels: Option<u32>,
}

/// Временная база для конвертации units → секунды.
///
/// Формула: seconds = (units * numer) / denom
#[derive(Debug, Clone, Copy)]
pub struct TimeBase {
    pub numer: u32,
    pub denom: u32,
}

/// Медиа packet — минимальная единица codec data.
///
/// Содержит raw codec data (VP9 frame или Opus packet)
/// с временными метками для синхронизации.
#[derive(Debug, Clone)]
pub struct Packet {
    /// ID трека, к которому принадлежит packet.
    pub track_id: u32,

    /// Тип трека (video/audio).
    pub kind: TrackKind,

    /// Presentation timestamp — когда показывать кадр/играть сэмпл.
    pub pts: Duration,

    /// Decode timestamp — когда декодировать (None, т.к. symphonia не отдаёт DTS).
    pub dts: Option<Duration>,

    /// Является ли packet ключевым кадром (для video).
    pub keyframe: bool,

    /// Raw codec data.
    /// Для VP9: raw VP9 frame (включая uncompressed header).
    /// Для Opus: raw Opus packet.
    pub data: Bytes,
}
