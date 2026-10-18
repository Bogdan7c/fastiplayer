//! Снимок media playlist субтитров и opaque-ссылка на сегмент.

use std::fmt;
use std::time::Duration;

use hls_playlist_core::{HlsKeyMethod, MediaPlaylist};
use source_core::{HttpBoundedByteRange, HttpRequestTarget};

use super::error::{HlsSubtitleFetchError, HlsSubtitleUnsupportedFeature};
use super::timeline::PlaylistTimelineEpoch;
use crate::plan::{RangeCursor, parse_hls_duration};

/// Opaque ссылка на один сегмент субтитров. Адрес спрятан: `Debug` показывает только номер.
///
/// Ссылку выдаёт `load_playlist` и принимает `fetch_segment` того же транспорта; ссылка от
/// другого транспорта отклоняется как `Stale`.
#[derive(Clone, PartialEq, Eq)]
pub struct HlsSubtitleSegmentRef {
    /// Какой экземпляр транспорта выдал ссылку (защита от чужих/устаревших ссылок).
    pub(super) transport_instance: u64,
    /// Уже разрешённый абсолютный адрес сегмента (секрет).
    pub(super) target: HttpRequestTarget,
    /// Диапазон байт сегмента внутри ресурса, если playlist использует `EXT-X-BYTERANGE`.
    pub(super) byte_range: Option<HttpBoundedByteRange>,
    media_sequence: u64,
}

impl HlsSubtitleSegmentRef {
    /// Пустышка для модульных тестов шкалы: адрес не нужен, сети нет.
    #[cfg(test)]
    pub(super) fn for_tests(media_sequence: u64) -> Self {
        Self {
            transport_instance: 0,
            target: HttpRequestTarget::parse_exact("https://example.invalid/s.vtt")
                .expect("static test URL parses"),
            byte_range: None,
            media_sequence,
        }
    }

    /// Media sequence сегмента в playlist-е субтитров.
    #[must_use]
    pub const fn media_sequence(&self) -> u64 {
        self.media_sequence
    }
}

impl fmt::Debug for HlsSubtitleSegmentRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HlsSubtitleSegmentRef")
            .field("media_sequence", &self.media_sequence)
            .field("has_byte_range", &self.byte_range.is_some())
            .finish_non_exhaustive()
    }
}

/// Один сегмент субтитров с временем внутри playlist-а.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlsSubtitleSegment {
    /// Ссылка для `fetch_segment`.
    pub reference: HlsSubtitleSegmentRef,
    /// RFC media sequence сегмента.
    pub media_sequence: u64,
    /// Номер discontinuity-эпохи сегмента: ключ в `HlsTimestampOrigins`.
    pub discontinuity_sequence: u64,
    /// Начало сегмента на СТАБИЛЬНОЙ шкале rendition-а: у первого снимка это сумма `EXTINF`
    /// предыдущих сегментов, у следующих - закреплено по перекрытию с прошлым снимком, так что
    /// один и тот же сегмент сохраняет `start` при скольжении live-окна.
    pub start: Duration,
    /// Длительность `EXTINF`.
    pub duration: Duration,
}

/// Снимок media playlist субтитров на момент одной загрузки.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlsSubtitlePlaylistSnapshot {
    /// `EXT-X-TARGETDURATION`: подсказка, как часто перечитывать live playlist.
    pub target_duration: Duration,
    /// Нет `EXT-X-ENDLIST`: playlist ещё растёт и его нужно перечитывать.
    pub is_live: bool,
    /// Сегменты в порядке playlist-а.
    pub segments: Vec<HlsSubtitleSegment>,
    /// Поколение стабильной шкалы: растёт, когда шкалу пришлось закрепить заново (разрыв
    /// между перечитываниями). После смены ранее выученные по `start` смещения недействительны.
    pub timeline_epoch: PlaylistTimelineEpoch,
}

impl HlsSubtitlePlaylistSnapshot {
    /// Сумма длительностей всех сегментов снимка.
    #[must_use]
    pub fn total_duration(&self) -> Duration {
        self.segments.last().map_or(Duration::ZERO, |last| {
            last.start.saturating_add(last.duration)
        })
    }

    /// Строит снимок по лёгкому профилю: шифрование -> `Unsupported`, проверки A/V-профиля не нужны.
    pub(super) fn from_media_playlist(
        playlist: &MediaPlaylist,
        playlist_base: &HttpRequestTarget,
        transport_instance: u64,
    ) -> Result<Self, HlsSubtitleFetchError> {
        let mut segments = Vec::with_capacity(playlist.segments.len());
        let mut range_cursor = RangeCursor::default();
        let mut start = Duration::ZERO;
        for segment in playlist.segments.iter() {
            if segment
                .key
                .as_ref()
                .is_some_and(|key| key.method != HlsKeyMethod::None)
            {
                return Err(HlsSubtitleFetchError::Unsupported(
                    HlsSubtitleUnsupportedFeature::Encryption,
                ));
            }
            let target = playlist_base
                .resolve_reference(segment.uri.expose_for_resolution())
                .map_err(|_| HlsSubtitleFetchError::InvalidReference)?;
            let byte_range = range_cursor
                .resolve(&target, segment.byte_range.as_ref())
                .map_err(|_| HlsSubtitleFetchError::MalformedPlaylist)?;
            let duration = parse_hls_duration(&segment.duration)
                .map_err(|_| HlsSubtitleFetchError::MalformedPlaylist)?;
            segments.push(HlsSubtitleSegment {
                reference: HlsSubtitleSegmentRef {
                    transport_instance,
                    target,
                    byte_range,
                    media_sequence: segment.media_sequence,
                },
                media_sequence: segment.media_sequence,
                discontinuity_sequence: segment.discontinuity_sequence,
                start,
                duration,
            });
            start = start
                .checked_add(duration)
                .ok_or(HlsSubtitleFetchError::MalformedPlaylist)?;
        }
        Ok(Self {
            target_duration: Duration::from_secs(playlist.target_duration_seconds),
            is_live: !playlist.end_list,
            segments,
            // Поколение выставляет владелец шкалы (`HlsSubtitleTransport`) при закреплении.
            timeline_epoch: PlaylistTimelineEpoch::default(),
        })
    }
}
