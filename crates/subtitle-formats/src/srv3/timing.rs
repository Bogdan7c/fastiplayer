//! Время абзацев srv3: проверка, дубликаты, конец показа.

use std::collections::{BTreeMap, HashSet};

use super::lines::{Line, build_lines};
use super::model::{RawDocument, RawParagraph};
use crate::report::{LineNumber, ParseIssue, ParseIssueKind, SubtitleParseReport};

/// Самое позднее допустимое время в файле, мс (около 3 лет): защита `мс -> мкс` от переполнения.
const MAX_TIME_MS: i64 = 100_000_000_000;

/// Сколько показывать абзац без `d`, если за ним в том же окне нет следующего абзаца.
///
/// У настоящих файлов YouTube `d` отсутствует только у служебных абзацев-переводов строки;
/// число нужно лишь как ограниченное значение для враждебных/нестандартных файлов.
const DEFAULT_PARAGRAPH_DURATION_MS: i64 = 5_000;

/// Абзац с проверенным временем и готовыми строками.
#[derive(Debug)]
pub(super) struct TimedParagraph<'document> {
    pub(super) raw: &'document RawParagraph,
    pub(super) start_ms: i64,
    pub(super) end_ms: i64,
    pub(super) lines: Vec<Line>,
}

/// Из абзацев файла оставляет показываемые: с корректным временем, текстом и без дубликатов.
///
/// Всё отброшенное попадает в отчёт (`report`), файл при этом не отвергается.
pub(super) fn timed_paragraphs<'document>(
    document: &'document RawDocument,
    report: &mut SubtitleParseReport,
) -> Vec<TimedParagraph<'document>> {
    let candidates = candidate_paragraphs(document, report);
    let next_starts = starts_by_window(&candidates);
    let mut timed = Vec::with_capacity(candidates.len());
    for (raw, start_ms, lines) in candidates {
        let end_ms = match raw.duration_ms {
            Some(duration_ms) => start_ms.saturating_add(duration_ms),
            None => end_without_duration(&next_starts, raw.window_id, start_ms),
        };
        if end_ms <= start_ms || end_ms > MAX_TIME_MS {
            report.record_skipped_cue(issue(raw, ParseIssueKind::EndNotAfterStart));
            continue;
        }
        timed.push(TimedParagraph {
            raw,
            start_ms,
            end_ms,
            lines,
        });
    }
    timed
}

/// Абзацы с корректным началом и текстом, без дубликатов.
fn candidate_paragraphs<'document>(
    document: &'document RawDocument,
    report: &mut SubtitleParseReport,
) -> Vec<(&'document RawParagraph, i64, Vec<Line>)> {
    // Дубликат — тот же момент, длительность, окно и текст (приём YTSubConverter: копия абзаца
    // с другим пером ради обводки). Остаётся первый.
    let mut seen: HashSet<(i64, Option<i64>, Option<u32>, String)> = HashSet::new();
    let mut candidates = Vec::new();
    for raw in &document.paragraphs {
        let Some(start_ms) = raw
            .start_ms
            .filter(|start| (0..=MAX_TIME_MS).contains(start))
        else {
            report.record_skipped_cue(issue(raw, ParseIssueKind::InvalidTimestamp));
            continue;
        };
        let lines = build_lines(raw, document);
        if lines.is_empty() {
            // Абзац-перевод строки автосубтитров (`a="1"`) — норма, а не пустая реплика.
            if !raw.is_append {
                report.record_empty_cue();
            }
            continue;
        }
        let key = (start_ms, raw.duration_ms, raw.window_id, plain_text(&lines));
        if !seen.insert(key) {
            report.record_ignored_style_rules(1);
            continue;
        }
        candidates.push((raw, start_ms, lines));
    }
    candidates
}

/// Текст строк через `\n` (для сравнения дубликатов).
fn plain_text(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|word| word.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Начала абзацев по окнам (`None` — абзацы вне окон), по возрастанию.
fn starts_by_window(
    candidates: &[(&RawParagraph, i64, Vec<Line>)],
) -> BTreeMap<Option<u32>, Vec<i64>> {
    let mut starts: BTreeMap<Option<u32>, Vec<i64>> = BTreeMap::new();
    for (raw, start_ms, _) in candidates {
        starts.entry(raw.window_id).or_default().push(*start_ms);
    }
    for window_starts in starts.values_mut() {
        window_starts.sort_unstable();
        window_starts.dedup();
    }
    starts
}

/// Конец абзаца без `d`: начало следующего абзаца окна, иначе ограниченное умолчание.
fn end_without_duration(
    starts: &BTreeMap<Option<u32>, Vec<i64>>,
    window_id: Option<u32>,
    start_ms: i64,
) -> i64 {
    starts
        .get(&window_id)
        .and_then(|window_starts| {
            let next = window_starts.partition_point(|&candidate| candidate <= start_ms);
            window_starts.get(next).copied()
        })
        .unwrap_or_else(|| start_ms.saturating_add(DEFAULT_PARAGRAPH_DURATION_MS))
}

fn issue(raw: &RawParagraph, kind: ParseIssueKind) -> ParseIssue {
    ParseIssue {
        // У XML нет строк: вместо номера строки в отчёте идёт номер абзаца.
        line_number: LineNumber::from_zero_based_index(raw.ordinal),
        kind,
    }
}
