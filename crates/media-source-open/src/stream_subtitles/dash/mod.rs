//! DASH-реализация транспорта субтитров потока: текстовые AdaptationSet MPD.
//!
//! Сеть, планирование ресурсов и привязка ко времени Period/PTO/live-сессии - в
//! `web_media_dash::DashTextSubtitles`; здесь только разбор документов (TTML/IMSC, WebVTT)
//! общими парсерами и сдвиг событий на шкалу плеера (см. [`decode`]).
//!
//! Форматы: отдельный TTML/WebVTT-файл (один сегмент на Period) и `stpp`/`wvtt` в fMP4
//! (SegmentTemplate/SegmentList/файл целиком). Живой MPD: индекс перечитывается по
//! `minimumUpdatePeriod`, новые сегменты берутся из последнего принятого снимка MPD.
//!
//! Ограничения:
//! - `ctim` (продолжение разрезанной реплики `wvtt`) не учитывается;
//! - нефрагментированный MP4 и шифрованные наборы не поддерживаются (набор пропускается
//!   парсером MPD либо сегмент отвергается как `Malformed`);
//! - у live при ротации токенов/CDN-адресов упавший сегмент субтитров не пере-адресуется
//!   (в отличие от A/V): он пропускается, следующий индекс берёт свежие адреса.

mod decode;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use source_core::CancellationToken;
use subtitle_core::SubtitleTime;
use web_media_dash::{
    DashTextError, DashTextRefresh, DashTextSegmentHandle, DashTextSubtitles, DashTextTrackId,
};

use super::error::StreamSubtitleError;
use super::model::{StreamSubtitleSource, StreamSubtitleTrackId, StreamSubtitleTrackInfo};
use super::transport::{
    DecodedSegment, PlayerTimeSpan, SegmentDecode, StreamIndexRefresh, StreamSegment,
    StreamSegmentIndex, StreamSegmentKey, StreamSegmentLimits, StreamSegmentRef,
    StreamSubtitleTransport,
};

/// Нижняя граница периода перечитывания живого индекса.
const MINIMUM_LIVE_REFRESH: std::time::Duration = std::time::Duration::from_secs(1);

/// Транспорт субтитров DASH-потока.
pub struct DashStreamSubtitleTransport {
    text: DashTextSubtitles,
}

impl DashStreamSubtitleTransport {
    /// Оборачивает субтитры открытия (берутся из результата открытия ДО `into_demuxer`).
    #[must_use]
    pub fn new(text: DashTextSubtitles) -> Self {
        Self { text }
    }

    /// Готовый источник для приложения; `None`, если у MPD нет текстовых дорожек.
    #[must_use]
    pub fn into_source(self) -> Option<StreamSubtitleSource> {
        StreamSubtitleSource::new(Arc::new(self))
    }
}

impl StreamSubtitleTransport for DashStreamSubtitleTransport {
    fn tracks(&self) -> Vec<StreamSubtitleTrackInfo> {
        self.text
            .tracks()
            .into_iter()
            .map(|info| StreamSubtitleTrackInfo {
                id: StreamSubtitleTrackId::new(info.id.index()),
                language: info.language,
                name: info.name,
                forced: info.forced,
                default: info.default,
                // DASH не различает «выбирается автоматически»: выбор за приложением.
                autoselect: true,
            })
            .collect()
    }

    fn load_index(
        &self,
        track: StreamSubtitleTrackId,
        cancellation: &CancellationToken,
    ) -> Result<StreamSegmentIndex, StreamSubtitleError> {
        let index = self
            .text
            .load_index(DashTextTrackId::from_index(track.index()), cancellation)?;
        let segments = index
            .segments
            .into_iter()
            .map(|segment| StreamSegment {
                key: StreamSegmentKey::new(segment.key),
                reference: StreamSegmentRef::new(segment.handle),
                start: segment.start,
                duration: segment.duration,
            })
            .collect();
        let refresh = match index.refresh {
            DashTextRefresh::Never => StreamIndexRefresh::Never,
            DashTextRefresh::After(period) => {
                StreamIndexRefresh::After(period.max(MINIMUM_LIVE_REFRESH))
            }
        };
        Ok(StreamSegmentIndex { segments, refresh })
    }

    fn fetch_and_decode(
        &self,
        segment: &StreamSegment,
        limits: StreamSegmentLimits,
        cancellation: &CancellationToken,
    ) -> Result<SegmentDecode, StreamSubtitleError> {
        let handle = segment
            .reference
            .downcast_ref::<DashTextSegmentHandle>()
            .ok_or(StreamSubtitleError::Stale)?;
        let fetched = self
            .text
            .fetch_segment(handle, limits.max_bytes, cancellation)?;
        let events = decode::decode_documents(&fetched, limits.max_bytes)?;
        Ok(SegmentDecode::Ready(DecodedSegment {
            covered: covered_span(segment, &events),
            events,
            // `StreamSegment::start` у DASH уже на шкале плеера, поэтому это точная привязка.
            segment_start: Some(time_from_duration(segment.start)),
        }))
    }
}

/// Участок шкалы плеера, который сегмент покрывает: номинал и реплики вместе.
fn covered_span(
    segment: &StreamSegment,
    events: &[subtitle_core::SubtitleEvent],
) -> Option<PlayerTimeSpan> {
    let mut start = time_from_duration(segment.start).as_micros();
    let mut end = time_from_duration(segment.end()).as_micros();
    for event in events {
        start = start.min(event.start().as_micros());
        end = end.max(event.end().as_micros());
    }
    (end >= start).then(|| PlayerTimeSpan {
        start: SubtitleTime::from_micros(start),
        end: SubtitleTime::from_micros(end),
    })
}

/// Длительность -> время субтитров (насыщение вместо паники на невозможных значениях).
fn time_from_duration(duration: std::time::Duration) -> SubtitleTime {
    SubtitleTime::from_micros(i64::try_from(duration.as_micros()).unwrap_or(i64::MAX))
}

impl From<DashTextError> for StreamSubtitleError {
    /// Сводит типизированные ошибки DASH-транспорта к нейтральным; URL в них нет.
    fn from(error: DashTextError) -> Self {
        match error {
            DashTextError::UnknownTrack => Self::UnknownTrack,
            DashTextError::Cancelled => Self::Cancelled,
            DashTextError::NotFound => Self::NotFound,
            DashTextError::Network => Self::Network,
            DashTextError::Rejected => Self::Rejected,
            DashTextError::TooLarge { limit_bytes } => Self::TooLarge { limit_bytes },
            DashTextError::Malformed => Self::Malformed,
            DashTextError::Stale => Self::Stale,
        }
    }
}

impl std::fmt::Debug for DashStreamSubtitleTransport {
    /// Адресов нет: только число дорожек.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DashStreamSubtitleTransport")
            .field("tracks", &self.text.tracks().len())
            .finish()
    }
}
