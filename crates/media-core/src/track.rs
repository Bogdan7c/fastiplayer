use std::fmt;
use std::time::Duration;

use bytes::Bytes;

use crate::TimeBase;

/// Идентификатор media-трека внутри текущего контейнера или stream manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackId(u32);

impl TrackId {
    /// Создаёт typed wrapper вокруг числового ID трека.
    #[must_use]
    pub const fn new(raw_track_id: u32) -> Self {
        Self(raw_track_id)
    }

    /// Возвращает исходный ID трека для контейнерных и backend-адаптеров.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for TrackId {
    /// Печатает ID как число, чтобы логи оставались компактными.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl From<u32> for TrackId {
    /// Создаёт [`TrackId`] из сырого container track id.
    fn from(raw_track_id: u32) -> Self {
        Self::new(raw_track_id)
    }
}

impl From<TrackId> for u32 {
    /// Возвращает сырой container track id для legacy-адаптеров.
    fn from(track_id: TrackId) -> Self {
        track_id.get()
    }
}

/// Тип media-трека, поддерживаемый текущим MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackKind {
    /// Видеотрек.
    Video,

    /// Аудиотрек.
    Audio,
}

/// Информация о media-треке, которую demuxer отдаёт до чтения packets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    /// Уникальный идентификатор трека внутри текущего media source.
    pub id: TrackId,

    /// Тип трека: video или audio.
    pub kind: TrackKind,

    /// Контейнерный codec id: например, `V_VP9` или `A_OPUS`.
    pub codec_id: String,

    /// Codec private data для инициализации декодера.
    pub codec_private: Option<Bytes>,

    /// Временная база трека для timestamp-конвертации.
    pub time_base: Option<TimeBase>,

    /// Длительность трека, если контейнер смог её сообщить.
    pub duration: Option<Duration>,

    /// Sample rate audio-трека в герцах.
    pub sample_rate: Option<u32>,

    /// Количество audio-каналов.
    pub channels: Option<u32>,
}
