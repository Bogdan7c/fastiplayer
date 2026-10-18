//! HLS-реализация транспорта субтитров потока (WebVTT-сегменты rendition-ов `TYPE=SUBTITLES`).
//!
//! Сеть и разбор плейлиста делает `web_media_hls::HlsSubtitleTransport`; время реплик
//! привязывается к тому же «нулю эпохи», что и у демуксера видео
//! (`web_media_hls::HlsTimestampOrigins`), поэтому реплики попадают в кадр точно.
//!
//! Известные ограничения:
//! - VOD без `X-TIMESTAMP-MAP` привязывается «как hls.js»: время реплик считается отсчитанным
//!   от начала эпохи разрыва (см. [`decode`]); live без карты - по RFC 8216 §3.5 (отсутствие
//!   карты равно `MPEGTS:0 <-> LOCAL:0`: время реплики и есть время контейнера). Точной
//!   привязки начала сегмента у такого live нет, поэтому окно запрашивается у живого края,
//!   и DVR-перемотка назад старые сегменты не подтягивает;
//! - live дольше ~13 часов в одной эпохе: самые старые пары точек отсчёта вытесняются из
//!   `HlsTimestampOrigins`, привязка старых реплик становится приближённой.

mod decode;
mod timing;

#[cfg(test)]
mod timing_tests;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use source_core::CancellationToken;
use web_media_hls::{
    HlsSubtitleFetchError, HlsSubtitleRenditionId, HlsSubtitleSegmentByteLimit,
    HlsSubtitleSegmentRef, HlsSubtitleTransport, HlsSubtitleUnsupportedFeature,
    HlsTimestampOrigins,
};

use self::decode::{PlaylistKind, SegmentFacts, parse_segment, resolve_segment};
use super::error::{StreamSubtitleError, StreamSubtitleUnsupported};
use super::model::{StreamSubtitleSource, StreamSubtitleTrackId, StreamSubtitleTrackInfo};
use super::transport::{
    SegmentDecode, StreamIndexRefresh, StreamSegment, StreamSegmentIndex, StreamSegmentKey,
    StreamSegmentLimits, StreamSegmentRef, StreamSubtitleTransport, StreamTimelineEpoch,
};

/// Нижняя граница периода перечитывания live-плейлиста: защита от `TARGETDURATION:0`.
const MINIMUM_LIVE_REFRESH: Duration = Duration::from_secs(1);

/// Данные, которые транспорт прячет в opaque ссылке сегмента.
struct HlsSegmentHandle {
    reference: HlsSubtitleSegmentRef,
    facts: SegmentFacts,
}

/// Транспорт субтитров HLS-потока.
pub struct HlsStreamSubtitleTransport {
    transport: HlsSubtitleTransport,
    origins: HlsTimestampOrigins,
    /// Поколение стабильной шкалы последнего загруженного индекса по дорожкам.
    timeline_epochs: Mutex<HashMap<u32, StreamTimelineEpoch>>,
}

impl HlsStreamSubtitleTransport {
    /// Собирает транспорт из результатов открытия HLS (оба берутся ДО `into_parts`).
    #[must_use]
    pub fn new(transport: HlsSubtitleTransport, origins: HlsTimestampOrigins) -> Self {
        Self {
            transport,
            origins,
            timeline_epochs: Mutex::new(HashMap::new()),
        }
    }

    /// Готовый источник субтитров для приложения; `None`, если у потока нет дорожек.
    #[must_use]
    pub fn into_source(self) -> Option<StreamSubtitleSource> {
        StreamSubtitleSource::new(std::sync::Arc::new(self))
    }

    /// Rendition по идентификатору дорожки (идентификатор - порядковый номер в списке).
    fn rendition(
        &self,
        track: StreamSubtitleTrackId,
    ) -> Result<HlsSubtitleRenditionId, StreamSubtitleError> {
        let index =
            usize::try_from(track.index()).map_err(|_| StreamSubtitleError::UnknownTrack)?;
        self.transport
            .renditions()
            .get(index)
            .map(|info| info.id)
            .ok_or(StreamSubtitleError::UnknownTrack)
    }
}

impl StreamSubtitleTransport for HlsStreamSubtitleTransport {
    fn tracks(&self) -> Vec<StreamSubtitleTrackInfo> {
        self.transport
            .renditions()
            .into_iter()
            .map(|info| StreamSubtitleTrackInfo {
                id: StreamSubtitleTrackId::new(info.id.index()),
                language: info.language,
                name: info.name,
                forced: info.forced,
                default: info.default,
                autoselect: info.autoselect,
            })
            .collect()
    }

    fn load_index(
        &self,
        track: StreamSubtitleTrackId,
        cancellation: &CancellationToken,
    ) -> Result<StreamSegmentIndex, StreamSubtitleError> {
        let rendition = self.rendition(track)?;
        let snapshot = self.transport.load_playlist(rendition, cancellation)?;
        self.timeline_epochs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                track.index(),
                StreamTimelineEpoch::new(snapshot.timeline_epoch.value()),
            );
        let playlist_kind = if snapshot.is_live {
            PlaylistKind::Live
        } else {
            PlaylistKind::Vod
        };
        let refresh = if snapshot.is_live {
            StreamIndexRefresh::After(snapshot.target_duration.max(MINIMUM_LIVE_REFRESH))
        } else {
            StreamIndexRefresh::Never
        };
        let segments = snapshot
            .segments
            .into_iter()
            .map(|segment| StreamSegment {
                key: StreamSegmentKey::new(segment.media_sequence),
                reference: StreamSegmentRef::new(HlsSegmentHandle {
                    reference: segment.reference,
                    facts: SegmentFacts {
                        discontinuity_sequence: segment.discontinuity_sequence,
                        duration: segment.duration,
                        playlist_kind,
                    },
                }),
                start: segment.start,
                duration: segment.duration,
            })
            .collect();
        Ok(StreamSegmentIndex { segments, refresh })
    }

    fn index_timeline_epoch(&self, track: StreamSubtitleTrackId) -> StreamTimelineEpoch {
        self.timeline_epochs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&track.index())
            .copied()
            .unwrap_or_default()
    }

    fn fetch_and_decode(
        &self,
        segment: &StreamSegment,
        limits: StreamSegmentLimits,
        cancellation: &CancellationToken,
    ) -> Result<SegmentDecode, StreamSubtitleError> {
        let handle = segment
            .reference
            .downcast_ref::<HlsSegmentHandle>()
            .ok_or(StreamSubtitleError::Stale)?;
        let bytes = self.transport.fetch_segment(
            &handle.reference,
            HlsSubtitleSegmentByteLimit::from_bytes(limits.max_bytes),
            cancellation,
        )?;
        let parsed = parse_segment(&bytes, limits.max_bytes, handle.facts)?;
        resolve_segment(parsed, self.origins.clone())
    }
}

impl From<HlsSubtitleFetchError> for StreamSubtitleError {
    /// Сводит типизированные ошибки HLS-транспорта к нейтральным; URL в них нет.
    fn from(error: HlsSubtitleFetchError) -> Self {
        match error {
            HlsSubtitleFetchError::UnknownRendition => Self::UnknownTrack,
            HlsSubtitleFetchError::Cancelled => Self::Cancelled,
            HlsSubtitleFetchError::NotFound | HlsSubtitleFetchError::Gone => Self::NotFound,
            // 408/429 и 5xx проходят сами; остальные 4xx - постоянный отказ.
            HlsSubtitleFetchError::HttpStatus { status } => {
                if status == 408 || status == 429 || status >= 500 {
                    Self::Network
                } else {
                    Self::NotFound
                }
            }
            HlsSubtitleFetchError::Network => Self::Network,
            HlsSubtitleFetchError::RequestRejected => Self::Rejected,
            HlsSubtitleFetchError::TooLarge { limit_bytes } => Self::TooLarge { limit_bytes },
            HlsSubtitleFetchError::MalformedPlaylist | HlsSubtitleFetchError::InvalidReference => {
                Self::Malformed
            }
            HlsSubtitleFetchError::Unsupported(HlsSubtitleUnsupportedFeature::Encryption) => {
                Self::Unsupported(StreamSubtitleUnsupported::Encryption)
            }
            HlsSubtitleFetchError::Stale => Self::Stale,
        }
    }
}

impl std::fmt::Debug for HlsStreamSubtitleTransport {
    /// Адресов нет: только число дорожек и известных эпох.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HlsStreamSubtitleTransport")
            .field("tracks", &self.transport.renditions().len())
            .field("known_epochs", &self.origins.known_epoch_count())
            .finish()
    }
}
