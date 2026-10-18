//! Выпуск событий: абзац режется на отрезки, где набор видимых листьев постоянен.
//!
//! Это упрощённый ISD (Intermediate Synchronic Document): моменты смены — начала и концы
//! листьев (`span` со своим временем, `br`). На каждом отрезке собирается текст из листьев,
//! видимых на всём отрезке. Соседние отрезки с одинаковым текстом сливаются в одно событие,
//! абзац без конца получает конец по правилу [`open_ended_end`]. Документ из сэмпла контейнера
//! дополнительно обрезается окном сэмпла (см. [`TtmlSampleWindow`]).

use std::collections::BTreeSet;

use subtitle_core::{
    StyledText, SubtitleContent, SubtitleEvent, SubtitleLayer, SubtitleTextCue, SubtitleTime,
};

use super::cue_text::build_styled_text;
use super::paragraphs::{CollectedParagraphs, Leaf, Paragraph};
use super::region_placement::cue_placement;
use super::window::TtmlSampleWindow;
use crate::error::SubtitleLoadError;
use crate::report::{LineNumber, ParseIssue, ParseIssueKind, SubtitleParseReport};

/// Сколько показывать абзац без конца, если во всём документе нет ни одного более позднего
/// момента (структурная граница, как у абзацев srv3 без `d`): 5 секунд.
const OPEN_ENDED_FALLBACK_MICROS: i64 = 5_000_000;

/// Отрезок абзаца с готовым текстом.
struct Interval {
    start_micros: i64,
    end_micros: i64,
    text: StyledText,
}

/// Выпускает события всех абзацев; `max_events` — потолок числа событий файла.
pub(super) fn emit_events(
    collected: &CollectedParagraphs,
    sample_window: Option<TtmlSampleWindow>,
    max_events: usize,
    report: &mut SubtitleParseReport,
) -> Result<Vec<SubtitleEvent>, SubtitleLoadError> {
    let mut events = Vec::new();
    let begins = distinct_begins(collected);
    for (ordinal, paragraph) in collected.paragraphs.iter().enumerate() {
        let begin = paragraph.window.begin_micros;
        let end = paragraph.window.end_micros.unwrap_or_else(|| {
            open_ended_end(
                begin,
                &begins,
                collected.latest_finite_micros,
                sample_window,
            )
        });
        let line_number = LineNumber::from_zero_based_index(ordinal);
        if end <= begin {
            report.record_skipped_cue(ParseIssue {
                line_number,
                kind: ParseIssueKind::EndNotAfterStart,
            });
            continue;
        }
        let intervals = paragraph_intervals(paragraph, begin, end);
        if intervals.is_empty() {
            report.record_empty_cue();
            continue;
        }
        let placement = cue_placement(paragraph.region, paragraph.style);
        for mut interval in intervals {
            // Документ из сэмпла активен только в интервале сэмпла: хвосты обрезаем.
            if let Some(window) = sample_window {
                interval.start_micros = interval.start_micros.max(window.start_micros());
                interval.end_micros = interval.end_micros.min(window.end_micros());
                if interval.end_micros <= interval.start_micros {
                    continue;
                }
            }
            if events.len() >= max_events {
                return Err(SubtitleLoadError::TooManyEvents { limit: max_events });
            }
            let content = SubtitleContent::Text(SubtitleTextCue {
                text: interval.text,
                placement: placement.clone(),
            });
            match SubtitleEvent::new(
                SubtitleTime::from_micros(interval.start_micros),
                SubtitleTime::from_micros(interval.end_micros),
                SubtitleLayer::DEFAULT,
                content,
            ) {
                Ok(event) => events.push(event),
                Err(_) => report.record_skipped_cue(ParseIssue {
                    line_number,
                    kind: ParseIssueKind::EndNotAfterStart,
                }),
            }
        }
    }
    Ok(events)
}

/// Различные начала абзацев по возрастанию (для правила «до следующего абзаца»).
fn distinct_begins(collected: &CollectedParagraphs) -> Vec<i64> {
    let mut begins: Vec<i64> = collected
        .paragraphs
        .iter()
        .map(|paragraph| paragraph.window.begin_micros)
        .collect();
    begins.sort_unstable();
    begins.dedup();
    begins
}

/// Конец абзаца, у которого он нигде не задан.
///
/// - Документ из сэмпла: конец окна сэмпла (абзац действует, пока действует сэмпл).
/// - Иначе: начало следующего по времени абзаца (чтобы открытые реплики не накрывали
///   последующие), а если следующего нет - позднейший момент документа ("до конца
///   документа"); если и он не позже начала - начало плюс [`OPEN_ENDED_FALLBACK_MICROS`].
fn open_ended_end(
    begin_micros: i64,
    sorted_begins: &[i64],
    latest_finite_micros: i64,
    sample_window: Option<TtmlSampleWindow>,
) -> i64 {
    if let Some(window) = sample_window {
        return window.end_micros();
    }
    let next_begin_index = sorted_begins.partition_point(|begin| *begin <= begin_micros);
    if let Some(next_begin) = sorted_begins.get(next_begin_index) {
        *next_begin
    } else if latest_finite_micros > begin_micros {
        latest_finite_micros
    } else {
        begin_micros.saturating_add(OPEN_ENDED_FALLBACK_MICROS)
    }
}

/// Лист с окном, зажатым в окно абзаца `[begin, end)`; пустое пересечение — `None`.
fn clipped_window(leaf: &Leaf, begin: i64, end: i64) -> Option<(i64, i64)> {
    let leaf_begin = leaf.window.begin_micros.max(begin);
    let leaf_end = leaf.window.end_micros.unwrap_or(end).min(end);
    (leaf_begin < leaf_end).then_some((leaf_begin, leaf_end))
}

/// Отрезки абзаца с непустым текстом; одинаковые соседние слиты.
fn paragraph_intervals(paragraph: &Paragraph, begin: i64, end: i64) -> Vec<Interval> {
    let visible: Vec<(&Leaf, i64, i64)> = paragraph
        .leaves
        .iter()
        .filter_map(|leaf| clipped_window(leaf, begin, end).map(|(b, e)| (leaf, b, e)))
        .collect();
    let mut boundaries: BTreeSet<i64> = BTreeSet::from([begin, end]);
    for (_, leaf_begin, leaf_end) in &visible {
        boundaries.insert(*leaf_begin);
        boundaries.insert(*leaf_end);
    }
    let points: Vec<i64> = boundaries.into_iter().collect();
    let mut intervals: Vec<Interval> = Vec::new();
    for pair in points.windows(2) {
        let [start, stop] = pair else { continue };
        let active: Vec<&Leaf> = visible
            .iter()
            .filter(|(_, leaf_begin, leaf_end)| leaf_begin <= start && stop <= leaf_end)
            .map(|(leaf, _, _)| *leaf)
            .collect();
        let Some(text) = build_styled_text(&active) else {
            continue;
        };
        match intervals.last_mut() {
            Some(previous) if previous.end_micros == *start && previous.text == text => {
                previous.end_micros = *stop;
            }
            _ => intervals.push(Interval {
                start_micros: *start,
                end_micros: *stop,
                text,
            }),
        }
    }
    intervals
}
