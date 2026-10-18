//! Индекс сегментов и скачивание/преобразование сегмента субтитров в документы.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::time::Duration;

use dash_mpd_core::DashTextKind;
use source_core::{CancellationToken, SourceError};
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveTransportError,
};
use web_media_transport_api::SourceGeneration;

use super::{
    DashTextDocument, DashTextDocumentFormat, DashTextError, DashTextFetchedSegment,
    DashTextSampleInterval, DashTextSegment, DashTextSegmentHandle, InitCacheKey,
    SegmentHandleInner, TextShared,
};
use crate::plan::{DashPlannedResource, DashTextPeriodPlan, DashTextTimeBasis};
use crate::request::DashSerializedFragmentKind;
use crate::text_fmp4::{
    TextFmp4Error, TextSample, TextSampleFormat, TextTrackInit, parse_text_mp4,
    webvtt_document_from_samples,
};

/// Отмена проверяется до и после сетевых операций.
pub(super) fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<(), DashTextError> {
    if cancellation.is_cancelled() {
        Err(DashTextError::Cancelled)
    } else {
        Ok(())
    }
}

/// Сегменты всех Period дорожки на шкале плеера; ушедшие целиком до начала шкалы пропускаются.
pub(super) fn index_segments<'plan>(
    periods: impl Iterator<Item = &'plan DashTextPeriodPlan>,
    track_index: u32,
    source_origin: Duration,
) -> Vec<DashTextSegment> {
    let mut segments = Vec::new();
    for period in periods {
        let period_key = (track_index, micros_u64(period.period_start));
        let init_key = InitCacheKey {
            period: period_key,
            initialization_fingerprint: initialization_fingerprint(period),
        };
        for resource in &period.segments {
            let (Some(local_start), Some(duration)) = (resource.timeline_start, resource.duration)
            else {
                continue;
            };
            let Some(presentation_start) = period.period_start.checked_add(local_start) else {
                continue;
            };
            let Some(presentation_end) = presentation_start.checked_add(duration) else {
                continue;
            };
            // Сегмент целиком раньше начала шкалы плеера (live до первого снимка) не нужен.
            if presentation_end <= source_origin {
                continue;
            }
            segments.push(DashTextSegment {
                key: micros_u64(presentation_start),
                start: presentation_start.saturating_sub(source_origin),
                // Начало упёрлось в 0 шкалы плеера: покрытый отрезок короче на обрезанную часть.
                duration: presentation_end.saturating_sub(source_origin.max(presentation_start)),
                handle: DashTextSegmentHandle {
                    inner: std::sync::Arc::new(SegmentHandleInner {
                        kind: period.kind,
                        basis: period.basis,
                        period_start: period.period_start,
                        local_start,
                        resource: resource.clone(),
                        initialization: period.initialization.clone(),
                        init_key,
                        period_key,
                        source_origin,
                    }),
                },
            });
        }
    }
    segments.sort_by_key(|segment| segment.start);
    segments
}

/// Необратимый отпечаток ресурса инициализации Period (с id Representation).
///
/// Без init-ресурса (init внутри самих сегментов) отпечаток зависит только от id.
fn initialization_fingerprint(period: &DashTextPeriodPlan) -> u64 {
    let mut hasher = DefaultHasher::new();
    period.representation_id.hash(&mut hasher);
    if let Some(resource) = &period.initialization {
        resource.target.hash(&mut hasher);
        if let Some(range) = resource.byte_range {
            range.start().hash(&mut hasher);
            range.length().hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn micros_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// Скачивает и преобразует один сегмент.
pub(super) fn fetch_segment(
    shared: &TextShared,
    segment: &SegmentHandleInner,
    max_bytes: NonZeroUsize,
    cancellation: &CancellationToken,
) -> Result<DashTextFetchedSegment, DashTextError> {
    ensure_not_cancelled(cancellation)?;
    let snapshot = shared.source.snapshot()?;
    let limit = max_bytes.min(
        snapshot
            .http
            .maximum_resource_bytes(AdaptiveResourcePurpose::MediaSegment),
    );
    let fetcher = Fetcher {
        http: &snapshot.http,
        generation: snapshot.generation,
        limit,
        cancellation,
    };
    // Время сдвига считается от начала Period; ISOBMFF уточняет его ниже.
    let period_shift = signed_micros(segment.period_start)?
        .checked_sub(signed_micros(segment.source_origin)?)
        .ok_or(DashTextError::Malformed)?;
    match segment.kind {
        DashTextKind::SidecarTtml | DashTextKind::SidecarWebVtt => {
            let bytes = fetcher.fetch(&segment.resource, AdaptiveResourcePurpose::MediaSegment)?;
            let format = if segment.kind == DashTextKind::SidecarTtml {
                DashTextDocumentFormat::Ttml
            } else {
                DashTextDocumentFormat::WebVtt
            };
            Ok(DashTextFetchedSegment {
                documents: vec![DashTextDocument {
                    format,
                    bytes,
                    sample_interval: None,
                }],
                player_shift_micros: to_i64(period_shift)?,
            })
        }
        DashTextKind::SegmentedStpp | DashTextKind::SegmentedWvtt => {
            fetch_isobmff(shared, segment, &fetcher, period_shift)
        }
    }
}

/// `stpp`/`wvtt`: init (из кэша, отдельным запросом либо из самого ресурса) + сэмплы.
fn fetch_isobmff(
    shared: &TextShared,
    segment: &SegmentHandleInner,
    fetcher: &Fetcher<'_>,
    period_shift: i128,
) -> Result<DashTextFetchedSegment, DashTextError> {
    let cached_init = cached_init(shared, segment.init_key);
    let init = match (cached_init, &segment.initialization) {
        (Some(init), _) => Some(init),
        (None, Some(resource)) => {
            let init_bytes = fetcher.fetch(resource, AdaptiveResourcePurpose::Initialization)?;
            let content = parse_text_mp4(&init_bytes, None).map_err(map_mp4_error)?;
            let init = content.init.ok_or(DashTextError::Malformed)?;
            store_init(shared, segment.init_key, init);
            Some(init)
        }
        (None, None) => None,
    };
    let bytes = fetcher.fetch(&segment.resource, AdaptiveResourcePurpose::MediaSegment)?;
    let content = parse_text_mp4(&bytes, init.as_ref()).map_err(map_mp4_error)?;
    let init = content.init.or(init).ok_or(DashTextError::Malformed)?;
    let expected_format = if segment.kind == DashTextKind::SegmentedStpp {
        TextSampleFormat::Stpp
    } else {
        TextSampleFormat::Wvtt
    };
    if init.format != expected_format {
        return Err(DashTextError::Malformed);
    }
    let shift = isobmff_shift(shared, segment, &init, &content.samples, period_shift)?;
    let documents = match init.format {
        TextSampleFormat::Stpp => content
            .samples
            .into_iter()
            .filter(|sample| !sample.data.is_empty())
            .map(|sample| {
                Ok(DashTextDocument {
                    format: DashTextDocumentFormat::Ttml,
                    sample_interval: sample_interval(&sample, init.timescale.get())?,
                    bytes: sample.data,
                })
            })
            .collect::<Result<Vec<_>, DashTextError>>()?,
        TextSampleFormat::Wvtt => {
            let document = webvtt_document_from_samples(&content.samples, init.timescale.get())
                .map_err(map_mp4_error)?;
            vec![DashTextDocument {
                format: DashTextDocumentFormat::WebVtt,
                bytes: document.into_bytes(),
                sample_interval: None,
            }]
        }
    };
    Ok(DashTextFetchedSegment {
        documents,
        player_shift_micros: to_i64(shift)?,
    })
}

/// Сдвиг времени media timeline дорожки на шкалу плеера (в микросекундах, знаковый).
fn isobmff_shift(
    shared: &TextShared,
    segment: &SegmentHandleInner,
    init: &TextTrackInit,
    samples: &[TextSample],
    period_shift: i128,
) -> Result<i128, DashTextError> {
    match segment.basis {
        // media -> presentation: минус PTO, плюс начало Period; затем минус начало шкалы плеера.
        DashTextTimeBasis::MediaTimeline { media_time_origin } => period_shift
            .checked_sub(signed_micros(media_time_origin)?)
            .ok_or(DashTextError::Malformed),
        // Как `NormalizeAtFirstPacket` у A/V: смещение задаёт ПЕРВЫЙ сэмпл ПЕРВОГО скачанного
        // сегмента Period (он лежит в начале своего сегмента), остальные сегменты берут его же.
        // Иначе у разреженной дорожки сэмпл, начинающийся позже начала сегмента, "прилипал" бы
        // к началу сегмента и показывался раньше времени.
        DashTextTimeBasis::NormalizedAtSegmentStart => {
            let offset = normalization_offset(shared, segment, init, samples)?;
            match offset {
                Some(offset) => offset
                    .checked_sub(signed_micros(segment.source_origin)?)
                    .ok_or(DashTextError::Malformed),
                // Сегмент без сэмплов и смещение ещё не определено: сдвигать нечего.
                None => Ok(0),
            }
        }
        DashTextTimeBasis::PeriodTimeline => Ok(period_shift),
    }
}

/// Смещение media timeline -> презентация для SegmentList (микросекунды, без `source_origin`).
///
/// Первый сегмент с сэмплами записывает смещение в общий кэш, остальные его читают; `None`,
/// если смещения нет и этот сегмент пуст (запомнить нечего).
fn normalization_offset(
    shared: &TextShared,
    segment: &SegmentHandleInner,
    init: &TextTrackInit,
    samples: &[TextSample],
) -> Result<Option<i128>, DashTextError> {
    let mut cache = shared
        .normalizations
        .lock()
        .map_err(|_| DashTextError::Malformed)?;
    if let Some(known) = cache.get(&segment.period_key) {
        return Ok(Some(*known));
    }
    let Some(first) = samples.first() else {
        return Ok(None);
    };
    let first_media = ticks_to_micros(first.decode_time, init.timescale.get());
    let segment_start = segment
        .period_start
        .checked_add(segment.local_start)
        .ok_or(DashTextError::Malformed)?;
    let offset = signed_micros(segment_start)?
        .checked_sub(first_media)
        .ok_or(DashTextError::Malformed)?;
    if cache.len() >= super::MAX_CACHED_INITS {
        cache.clear();
    }
    cache.insert(segment.period_key, offset);
    Ok(Some(offset))
}

/// Интервал сэмпла в микросекундах media timeline; `None` у сэмпла нулевой длины.
fn sample_interval(
    sample: &TextSample,
    timescale: u32,
) -> Result<Option<DashTextSampleInterval>, DashTextError> {
    let end_ticks = sample
        .decode_time
        .checked_add(sample.duration)
        .ok_or(DashTextError::Malformed)?;
    let start_micros = to_i64(ticks_to_micros(sample.decode_time, timescale))?;
    let end_micros = to_i64(ticks_to_micros(end_ticks, timescale))?;
    Ok(
        (end_micros > start_micros).then_some(DashTextSampleInterval {
            start_micros,
            end_micros,
        }),
    )
}

fn ticks_to_micros(ticks: u64, timescale: u32) -> i128 {
    i128::from(ticks) * 1_000_000 / i128::from(timescale.max(1))
}

fn signed_micros(duration: Duration) -> Result<i128, DashTextError> {
    i128::try_from(duration.as_micros()).map_err(|_| DashTextError::Malformed)
}

fn to_i64(value: i128) -> Result<i64, DashTextError> {
    i64::try_from(value).map_err(|_| DashTextError::Malformed)
}

fn cached_init(shared: &TextShared, key: InitCacheKey) -> Option<TextTrackInit> {
    shared.inits.lock().ok()?.get(&key).copied()
}

fn store_init(shared: &TextShared, key: InitCacheKey, init: TextTrackInit) {
    if let Ok(mut cache) = shared.inits.lock() {
        if cache.len() >= super::MAX_CACHED_INITS {
            cache.clear();
        }
        cache.insert(key, init);
    }
}

fn map_mp4_error(_error: TextFmp4Error) -> DashTextError {
    DashTextError::Malformed
}

/// Скачивание одного ресурса с лимитом.
struct Fetcher<'a> {
    http: &'a AdaptiveHttpContext,
    generation: SourceGeneration,
    limit: NonZeroUsize,
    cancellation: &'a CancellationToken,
}

impl Fetcher<'_> {
    fn fetch(
        &self,
        resource: &DashPlannedResource,
        purpose: AdaptiveResourcePurpose,
    ) -> Result<Vec<u8>, DashTextError> {
        ensure_not_cancelled(self.cancellation)?;
        debug_assert!(matches!(
            resource.kind,
            DashSerializedFragmentKind::Initialization | DashSerializedFragmentKind::Media
        ));
        let query = AdaptiveResourceQueryApplication::ApplyScopedReplacement;
        let request = match resource.byte_range {
            Some(range) => {
                if range.length() > self.limit {
                    return Err(DashTextError::TooLarge {
                        limit_bytes: self.limit.get(),
                    });
                }
                AdaptiveResourceFetchRequest::range(
                    self.generation,
                    resource.target.clone(),
                    range,
                    range.length(),
                    purpose,
                    query,
                )
            }
            None => AdaptiveResourceFetchRequest::full(
                self.generation,
                resource.target.clone(),
                self.limit,
                purpose,
                query,
            ),
        };
        let fetched = self
            .http
            .fetch_resource_blocking(request)
            .map_err(|error| map_transport_error(error, self.limit))?;
        ensure_not_cancelled(self.cancellation)?;
        Ok(fetched.into_bytes())
    }
}

/// Сводит ошибку общего HTTP-слоя к безопасным вариантам (URL в них нет).
fn map_transport_error(error: AdaptiveTransportError, limit: NonZeroUsize) -> DashTextError {
    match error {
        AdaptiveTransportError::Cancelled | AdaptiveTransportError::RestartableReadInterrupted => {
            DashTextError::Cancelled
        }
        AdaptiveTransportError::StaleGeneration { .. } => DashTextError::Stale,
        AdaptiveTransportError::Target(_) => DashTextError::Malformed,
        AdaptiveTransportError::Redirect(_)
        | AdaptiveTransportError::SecretScopeRejected
        | AdaptiveTransportError::ExplicitCookieHeader
        | AdaptiveTransportError::InvalidResourcePolicy { .. } => DashTextError::Rejected,
        AdaptiveTransportError::ResourceBoundExceeded { .. } => DashTextError::TooLarge {
            limit_bytes: limit.get(),
        },
        AdaptiveTransportError::WorkerStopped => DashTextError::Network,
        AdaptiveTransportError::Source(source) => map_source_error(&source, limit),
    }
}

fn map_source_error(error: &SourceError, limit: NonZeroUsize) -> DashTextError {
    match error {
        SourceError::Cancelled | SourceError::SourceClosed => DashTextError::Cancelled,
        SourceError::HttpStatus { status, .. } => {
            let status = status.as_u16();
            // 408/429 и 5xx проходят сами; остальные коды - постоянный отказ.
            if status == 408 || status == 429 || status >= 500 {
                DashTextError::Network
            } else {
                DashTextError::NotFound
            }
        }
        SourceError::HttpBodyTooLarge { maximum_bytes, .. } => DashTextError::TooLarge {
            limit_bytes: *maximum_bytes,
        },
        _ => {
            let _ = limit;
            DashTextError::Network
        }
    }
}
