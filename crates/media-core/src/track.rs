use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use codec_core::{
    BitDepth, ChromaSubsampling, VideoColorMetadata, VideoDisplayOrientation, VideoProfile,
};

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

/// Доказанный способ framing-а compressed video packets на demux boundary.
///
/// Тип намеренно не знает ни MPEG-TS, ни MP4: он описывает bytes, которые
/// получит decoder, и источник доказательства для length-prefixed framing-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum VideoPacketFraming {
    /// Demuxer не смог доказать framing до чтения packets; legacy probe решает позже.
    #[default]
    Unspecified,
    /// Каждый packet содержит Annex-B start-code NAL units.
    AnnexB,
    /// NAL units имеют length prefix, размер которого задаёт codec configuration record.
    LengthPrefixedFromCodecConfiguration,
}

/// Информация о media-треке, которую demuxer отдаёт до чтения packets.
#[derive(Debug, Clone, PartialEq)]
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

    /// Container-level video metadata, если demuxer смог получить её до decode.
    pub video: Option<VideoTrackMetadata>,
}

/// Container metadata видеотрека без backend-specific ресурсов.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoTrackMetadata {
    /// Framing compressed elementary packets на выходе demuxer-а.
    pub packet_framing: VideoPacketFraming,

    /// Coded width из container track header.
    pub coded_width: Option<u32>,

    /// Coded height из container track header.
    pub coded_height: Option<u32>,

    /// Container или manifest hint по codec profile.
    pub profile: Option<VideoProfile>,

    /// Container hint по decoded bit depth.
    pub bit_depth: Option<BitDepth>,

    /// Container hint по chroma subsampling.
    pub chroma: Option<ChromaSubsampling>,

    /// Container Colour metadata после нормализации в общую color model.
    pub color: Option<VideoColorMetadata>,

    /// Display orientation из container track transform.
    pub orientation: VideoDisplayOrientation,
}

impl VideoTrackMetadata {
    /// Создаёт пустой video metadata контейнер для постепенного заполнения extractor-ом.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            packet_framing: VideoPacketFraming::Unspecified,
            coded_width: None,
            coded_height: None,
            profile: None,
            bit_depth: None,
            chroma: None,
            color: None,
            orientation: VideoDisplayOrientation::Identity,
        }
    }

    /// Возвращает `None`, если extractor не нашёл ни одного полезного video поля.
    #[must_use]
    pub fn into_option(self) -> Option<Self> {
        let has_metadata = self.coded_width.is_some()
            || self.coded_height.is_some()
            || self.profile.is_some()
            || self.bit_depth.is_some()
            || self.chroma.is_some()
            || self.color.is_some()
            || self.orientation != VideoDisplayOrientation::Identity
            || self.packet_framing != VideoPacketFraming::Unspecified;

        has_metadata.then_some(self)
    }
}
