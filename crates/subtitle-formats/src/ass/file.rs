//! Файл `.ass` / `.ssa` целиком: заголовок + строки `Dialogue:` -> документ с событиями ASS.

use std::sync::Arc;

use subtitle_core::{
    AssCue, AssScript, AssTextItem, SubtitleContent, SubtitleEvent, SubtitleLayer, SubtitleTime,
};

use super::event_fields::RawEventFields;
use super::hard_overrides::scan_hard_overrides;
use super::limits::AssParseLimits;
use super::lines::AssLines;
use super::report::{AssParseReport, AssTagStats};
use super::scan::c_string_prefix;
use super::script_builder::{LineOutcome, LineScope, ScriptBuilder};
use super::text::parse_event_text;
use crate::document::ParsedSubtitleDocument;
use crate::error::SubtitleLoadError;
use crate::limits::SubtitleParseLimits;
use crate::report::{LineNumber, ParseIssue, ParseIssueKind, SubtitleParseReport};

/// Строка `Dialogue:` вместе с номером строки файла (для отчёта).
struct LocatedEvent {
    line_number: LineNumber,
    fields: RawEventFields,
}

/// Разбирает декодированный текст файла ASS/SSA.
///
/// - Битая строка `Dialogue:` пропускается и считается (`skipped_cues`, проблема со
///   строкой); `Comment:` игнорируется.
/// - Событие, где конец не позже начала, пропускается и считается: libass держит такие
///   события невидимыми, а `SubtitleEvent` требует `end > start`.
/// - Событие без видимого текста (только теги) пропускается как пустое.
/// - Стиль события ищется по ИТОГОВОМУ списку стилей (libass смотрит на стили, прочитанные
///   к этой строке): различается лишь при стилях, объявленных после `[Events]`.
pub(crate) fn parse_ass_document(
    text: &str,
    limits: &SubtitleParseLimits,
    ass_limits: &AssParseLimits,
) -> Result<ParsedSubtitleDocument, SubtitleLoadError> {
    // Тот же предел, что у заголовка встроенной дорожки: враждебный файл со стилями на
    // сотни мегабайт не должен раздувать сценарий (до разбора, по байтам до `[Events]`).
    let header_bytes = header_len_before_events(text);
    if header_bytes > ass_limits.max_header_bytes {
        return Err(SubtitleLoadError::AssHeaderTooLarge {
            actual_bytes: header_bytes,
            limit_bytes: ass_limits.max_header_bytes,
        });
    }
    let mut report = SubtitleParseReport::new(limits.max_reported_issues);
    let mut builder = ScriptBuilder::new(ass_limits, LineScope::FullFile);
    let mut located_events: Vec<LocatedEvent> = Vec::new();
    for (line_index, line) in AssLines::new(c_string_prefix(text)) {
        let line_number = LineNumber::from_zero_based_index(line_index);
        match builder.feed_line(line) {
            LineOutcome::Handled => {}
            LineOutcome::Dialogue(fields) => {
                if located_events.len() >= limits.max_events {
                    return Err(SubtitleLoadError::TooManyEvents {
                        limit: limits.max_events,
                    });
                }
                located_events.push(LocatedEvent {
                    line_number,
                    fields,
                });
            }
            LineOutcome::BrokenDialogue => report.record_skipped_cue(ParseIssue {
                line_number,
                kind: ParseIssueKind::MalformedAssDialogue,
            }),
        }
    }
    let skipped_styles = builder.skipped_styles();
    let script = Arc::new(builder.finish());
    let mut tag_stats = AssTagStats::new(ass_limits);
    let events = build_events(
        located_events,
        &script,
        ass_limits,
        &mut tag_stats,
        &mut report,
    );
    let ass_report = AssParseReport {
        header: super::report::AssHeaderReport {
            skipped_styles,
            had_replacement_characters: false,
        },
        tags: tag_stats,
    };
    Ok(ParsedSubtitleDocument::new(events, Vec::new(), report).with_ass(script, ass_report))
}

/// Сколько байт текста занимает всё до первой строки секции `[Events]` (заголовок сценария:
/// `[Script Info]`, стили, шрифты). Без секции событий заголовком считается весь текст.
fn header_len_before_events(text: &str) -> usize {
    const EVENTS_SECTION: &[u8] = b"[Events]";
    let mut offset = 0usize;
    for segment in text.split_inclusive(['\r', '\n']) {
        let line = segment.strip_prefix('\u{feff}').unwrap_or(segment);
        let is_events_header = line
            .as_bytes()
            .get(..EVENTS_SECTION.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(EVENTS_SECTION));
        if is_events_header {
            return offset;
        }
        offset += segment.len();
    }
    text.len()
}

/// Превращает сырые строки в события; непригодные пропускает с записью в отчёт.
fn build_events(
    located_events: Vec<LocatedEvent>,
    script: &Arc<AssScript>,
    ass_limits: &AssParseLimits,
    tag_stats: &mut AssTagStats,
    report: &mut SubtitleParseReport,
) -> Vec<SubtitleEvent> {
    let mut events = Vec::with_capacity(located_events.len());
    for (event_index, located) in located_events.into_iter().enumerate() {
        let LocatedEvent {
            line_number,
            fields,
        } = located;
        if fields.end_ms <= fields.start_ms {
            report.record_skipped_cue(ParseIssue {
                line_number,
                kind: ParseIssueKind::EndNotAfterStart,
            });
            continue;
        }
        let cue = AssCue {
            script: Arc::clone(script),
            style: script.lookup_style(&fields.style_name),
            // Как индекс события в libass: порядковый номер среди принятых строк `Dialogue:`.
            read_order: i32::try_from(event_index).unwrap_or(i32::MAX),
            margins: fields.margins,
            name: fields.name,
            effect: fields.effect,
            hard_overrides: scan_hard_overrides(&fields.text),
            text: parse_event_text(
                &fields.text,
                ass_limits.max_override_tags_per_event,
                tag_stats,
            ),
        };
        if !has_visible_text(&cue.text) {
            report.record_empty_cue();
            continue;
        }
        let event = SubtitleEvent::new(
            SubtitleTime::from_millis(fields.start_ms),
            SubtitleTime::from_millis(fields.end_ms),
            SubtitleLayer::new(fields.layer),
            SubtitleContent::Ass(cue),
        );
        match event {
            Ok(event) => events.push(event),
            Err(_interval_error) => report.record_skipped_cue(ParseIssue {
                line_number,
                kind: ParseIssueKind::EndNotAfterStart,
            }),
        }
    }
    events
}

/// Есть ли в тексте хоть что-то, что рисуется: символы или переводы строк
/// (блоки одних тегов ничего не рисуют).
pub(crate) fn has_visible_text(items: &[AssTextItem]) -> bool {
    items
        .iter()
        .any(|item| !matches!(item, AssTextItem::Overrides(_)))
}
