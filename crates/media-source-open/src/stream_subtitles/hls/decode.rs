//! Разбор WebVTT-сегмента HLS и привязка его реплик к шкале плеера.

use std::num::NonZeroUsize;
use std::time::Duration;

use subtitle_core::{SubtitleEvent, SubtitleTime, SubtitleTimeShift};
use subtitle_formats::{
    LegacyEncodingHint, SubtitleLoadError, SubtitleParseLimits, TextSubtitleFormat,
    WebVttTimestampMap, load_text_subtitle,
};
use web_media_hls::{HlsEpochTimestampOrigin, HlsTimestampOrigins};

use super::timing::{
    OriginMicros, cue_to_player_shift, duration_micros, media_to_player, unwrap_mpegts_near,
    wrap_shift_near,
};
use crate::stream_subtitles::error::StreamSubtitleError;
use crate::stream_subtitles::transport::{
    DecodedSegment, PendingSegmentDecode, PlayerTimeSpan, SegmentDecode,
};

/// Структурный предел числа реплик в одном сегменте: защита от враждебного файла.
const MAX_CUES_PER_SEGMENT: usize = 10_000;
/// Сколько проблем разбора запоминает отчёт (сами проблемы ленте не нужны).
const MAX_REPORTED_PARSE_ISSUES: usize = 8;

/// Вид плейлиста субтитров: от него зависит привязка сегментов без `X-TIMESTAMP-MAP`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlaylistKind {
    /// Завершённый плейлист (`EXT-X-ENDLIST`).
    Vod,
    /// Растущий плейлист.
    Live,
}

/// Что известно о сегменте из плейлиста.
#[derive(Clone, Copy, Debug)]
pub(super) struct SegmentFacts {
    /// VOD или live.
    pub(super) playlist_kind: PlaylistKind,
    /// Эпоха разрыва: ключ точки отсчёта у демуксера видео.
    pub(super) discontinuity_sequence: u64,
    /// Номинальная длительность `EXTINF`.
    pub(super) duration: Duration,
}

/// Разобранный, но ещё не привязанный к шкале плеера сегмент.
pub(super) struct ParsedSegment {
    events: Vec<SubtitleEvent>,
    timestamp_map: Option<WebVttTimestampMap>,
    facts: SegmentFacts,
}

/// Разбирает байты сегмента как WebVTT под лимитом `max_bytes`.
pub(super) fn parse_segment(
    bytes: &[u8],
    max_bytes: NonZeroUsize,
    facts: SegmentFacts,
) -> Result<ParsedSegment, StreamSubtitleError> {
    let limits = SubtitleParseLimits::new(
        max_bytes.get(),
        MAX_CUES_PER_SEGMENT,
        MAX_REPORTED_PARSE_ISSUES,
    );
    let loaded = load_text_subtitle(
        bytes,
        TextSubtitleFormat::WebVtt,
        LegacyEncodingHint::Generic,
        &limits,
    )
    .map_err(|error| match error {
        SubtitleLoadError::InputTooLarge { limit_bytes, .. } => {
            StreamSubtitleError::TooLarge { limit_bytes }
        }
        _ => StreamSubtitleError::Malformed,
    })?;
    let document = loaded.into_document();
    // Карту читаем до `into_parts`: он забирает документ целиком.
    let timestamp_map = document.webvtt_timestamp_map();
    let (events, _regions, _report) = document.into_parts();
    Ok(ParsedSegment {
        events,
        timestamp_map,
        facts,
    })
}

/// Сегмент, который ждёт точку отсчёта эпохи от демуксера видео.
struct PendingHlsSegment {
    parsed: ParsedSegment,
    origins: HlsTimestampOrigins,
}

impl PendingSegmentDecode for PendingHlsSegment {
    fn try_resolve(self: Box<Self>) -> Result<SegmentDecode, StreamSubtitleError> {
        resolve_segment(self.parsed, self.origins)
    }
}

/// Привязывает сегмент к шкале плеера или оставляет его ждать точку отсчёта.
pub(super) fn resolve_segment(
    parsed: ParsedSegment,
    origins: HlsTimestampOrigins,
) -> Result<SegmentDecode, StreamSubtitleError> {
    match align_to_player(&parsed, &origins)? {
        Some(alignment) => Ok(SegmentDecode::Ready(apply_alignment(&parsed, alignment)?)),
        None => Ok(SegmentDecode::Pending(Box::new(PendingHlsSegment {
            parsed,
            origins,
        }))),
    }
}

/// Как сдвинуть реплики сегмента на шкалу плеера.
#[derive(Clone, Copy, Debug)]
struct Alignment {
    /// Сдвиг каждой реплики, микросекунды.
    cue_shift_micros: i64,
    /// Где на шкале плеера лежит точка `LOCAL` файла (около начала сегмента); только с картой.
    anchor_on_player_micros: Option<i64>,
}

/// Считает привязку; `None` - точка отсчёта эпохи демуксером ещё не опубликована.
///
/// С `X-TIMESTAMP-MAP`: `media = MPEGTS + (cue - LOCAL)`, метка MPEGTS разворачивается
/// к ближайшей к точке отсчёта эпохи (33-битный оборот). Для live пара берётся по времени
/// самого сегмента (`origin_for_media_time`), для VOD она одна на эпоху.
///
/// Без карты решение зависит от вида плейлиста; привязки к началу сегмента нет в обоих случаях
/// (`anchor_on_player_micros = None`).
/// - VOD (как hls.js): время реплик считается отсчитанным от начала эпохи разрыва, поэтому
///   `player = cue + timeline_start(эпохи)`. Тест на реальном VOD закрепляет этот выбор.
/// - Live (RFC 8216 §3.5): отсутствие карты равно `MPEGTS:0 <-> LOCAL:0`, значит время
///   реплики - это время контейнера (с точностью до оборота 33-битного счётчика), и
///   `player = cue - media_origin + timeline_start` по паре, действующей на это время.
///   Эпоха-относительное правило для live неверно: у скользящего окна «начало эпохи»
///   давно позади, а пара точек отсчёта у каждого сегмента своя.
fn align_to_player(
    parsed: &ParsedSegment,
    origins: &HlsTimestampOrigins,
) -> Result<Option<Alignment>, StreamSubtitleError> {
    let epoch_key = parsed.facts.discontinuity_sequence;
    let Some(epoch) = origins.epoch_origin(epoch_key) else {
        return Ok(None);
    };
    let Some(map) = parsed.timestamp_map else {
        if parsed.facts.playlist_kind == PlaylistKind::Live {
            return align_live_without_map(parsed, origins, epoch);
        }
        let timeline_start =
            duration_micros(epoch.timeline_start()).ok_or(StreamSubtitleError::Malformed)?;
        return Ok(Some(Alignment {
            cue_shift_micros: timeline_start,
            anchor_on_player_micros: None,
        }));
    };
    let local = map.local.as_micros();
    // Первая оценка относительно самой ранней пары эпохи, затем уточнение парой сегмента.
    let first_origin = origin_micros(epoch)?;
    let first_anchor = unwrap_mpegts_near(map.mpegts, epoch.media_origin())
        .ok_or(StreamSubtitleError::Malformed)?;
    let segment_epoch = origins
        .origin_for_media_time(epoch_key, duration_from_micros(first_anchor))
        .unwrap_or(epoch);
    let (origin, anchor) = if segment_epoch == epoch {
        (first_origin, first_anchor)
    } else {
        let refined = origin_micros(segment_epoch)?;
        let anchor = unwrap_mpegts_near(map.mpegts, segment_epoch.media_origin())
            .ok_or(StreamSubtitleError::Malformed)?;
        (refined, anchor)
    };
    let cue_shift_micros =
        cue_to_player_shift(anchor, local, origin).ok_or(StreamSubtitleError::Malformed)?;
    let anchor_on_player_micros =
        media_to_player(anchor, origin).ok_or(StreamSubtitleError::Malformed)?;
    Ok(Some(Alignment {
        cue_shift_micros,
        anchor_on_player_micros: Some(anchor_on_player_micros),
    }))
}

/// Live без `X-TIMESTAMP-MAP`: время реплики равно времени контейнера (RFC 8216 §3.5).
///
/// По самой ранней реплике сегмента выбирается пара точек отсчёта (как для сегмента с картой,
/// через `origin_for_media_time`) и разворот оборота счётчика; один сдвиг на весь сегмент.
fn align_live_without_map(
    parsed: &ParsedSegment,
    origins: &HlsTimestampOrigins,
    epoch: HlsEpochTimestampOrigin,
) -> Result<Option<Alignment>, StreamSubtitleError> {
    let Some(sample_micros) = parsed
        .events
        .iter()
        .map(|event| event.start().as_micros())
        .min()
    else {
        // Реплик нет - сдвигать нечего.
        return Ok(Some(Alignment {
            cue_shift_micros: 0,
            anchor_on_player_micros: None,
        }));
    };
    let epoch_key = parsed.facts.discontinuity_sequence;
    let first_wrap = wrap_shift_near(sample_micros, epoch.media_origin())
        .ok_or(StreamSubtitleError::Malformed)?;
    let first_media = sample_micros
        .checked_add(first_wrap)
        .ok_or(StreamSubtitleError::Malformed)?;
    let segment_epoch = origins
        .origin_for_media_time(epoch_key, duration_from_micros(first_media))
        .unwrap_or(epoch);
    let wrap = wrap_shift_near(sample_micros, segment_epoch.media_origin())
        .ok_or(StreamSubtitleError::Malformed)?;
    let origin = origin_micros(segment_epoch)?;
    // media = cue + wrap; player = media - media_origin + timeline_start.
    let cue_shift_micros = media_to_player(wrap, origin).ok_or(StreamSubtitleError::Malformed)?;
    Ok(Some(Alignment {
        cue_shift_micros,
        anchor_on_player_micros: None,
    }))
}

/// Пара эпохи в микросекундах.
fn origin_micros(origin: HlsEpochTimestampOrigin) -> Result<OriginMicros, StreamSubtitleError> {
    Ok(OriginMicros {
        media_origin: duration_micros(origin.media_origin())
            .ok_or(StreamSubtitleError::Malformed)?,
        timeline_start: duration_micros(origin.timeline_start())
            .ok_or(StreamSubtitleError::Malformed)?,
    })
}

/// Неотрицательная длительность из знаковых микросекунд (отрицательные -> ноль).
fn duration_from_micros(micros: i64) -> Duration {
    Duration::from_micros(u64::try_from(micros).unwrap_or(0))
}

/// Сдвигает реплики и считает покрытый участок шкалы плеера.
fn apply_alignment(
    parsed: &ParsedSegment,
    alignment: Alignment,
) -> Result<DecodedSegment, StreamSubtitleError> {
    let shift = SubtitleTimeShift::from_micros(alignment.cue_shift_micros);
    let events = parsed
        .events
        .iter()
        .map(|event| event.shifted_by(shift))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StreamSubtitleError::Malformed)?;
    let nominal = duration_micros(parsed.facts.duration).ok_or(StreamSubtitleError::Malformed)?;
    let mut span_start = alignment.anchor_on_player_micros;
    let mut span_end = span_start.and_then(|start| start.checked_add(nominal));
    for event in &events {
        span_start = Some(span_start.map_or(event.start().as_micros(), |known| {
            known.min(event.start().as_micros())
        }));
        span_end = Some(span_end.map_or(event.end().as_micros(), |known| {
            known.max(event.end().as_micros())
        }));
    }
    let covered = match (span_start, span_end) {
        (Some(start), Some(end)) if end >= start => Some(PlayerTimeSpan {
            start: SubtitleTime::from_micros(start),
            end: SubtitleTime::from_micros(end),
        }),
        _ => None,
    };
    Ok(DecodedSegment {
        events,
        covered,
        segment_start: alignment
            .anchor_on_player_micros
            .map(SubtitleTime::from_micros),
    })
}
