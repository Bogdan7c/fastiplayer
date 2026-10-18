//! Окна srv3 -> события модели: бегущие строки автосубтитров.
//!
//! Абзацы одного окна (`<p w="N">`) показываются ВМЕСТЕ: в любой момент окно показывает
//! строки всех активных на этот момент абзацев (новые снизу), но не больше `rc` последних.
//! Модель же хранит события с постоянным содержимым, поэтому ось времени окна режется в
//! каждом начале и конце абзаца, а на каждом отрезке выпускается своё событие со сборными
//! строками. Слова, ещё не произнесённые, получают `reveal_at` и появятся по ходу отрезка.

use subtitle_core::{
    CuePlacement, SpanContent, SpanStyle, StyledText, SubtitleContent, SubtitleEvent,
    SubtitleLayer, SubtitleTextCue, SubtitleTime, TextSpan,
};

use super::lines::Line;
use super::timing::TimedParagraph;
use crate::error::SubtitleLoadError;

/// Сколько одновременно активных абзацев окна учитываем. Настоящие окна держат 2-3 строки;
/// предел защищает от враждебного файла из тысяч перекрывающихся абзацев (самые старые
/// отбрасываются), чтобы стоимость отрезка не росла вместе с файлом.
const MAX_ACTIVE_PARAGRAPHS: usize = 64;

/// Окно: абзацы, показываемые вместе, и сколько строк оно вмещает.
pub(super) struct WindowGroup<'document> {
    /// Абзацы по возрастанию начала (при равенстве — по порядку в файле).
    pub(super) paragraphs: Vec<ScheduledParagraph<'document>>,
    /// Максимум одновременно показываемых строк.
    pub(super) max_rows: usize,
}

/// Абзац окна вместе с размещением, которое для него вычислено.
pub(super) struct ScheduledParagraph<'document> {
    pub(super) timed: TimedParagraph<'document>,
    pub(super) placement: CuePlacement,
}

/// Строка, видимая на отрезке: слова и начало её абзаца (от него считаются смещения слов).
struct VisibleLine<'line> {
    paragraph_start_ms: i64,
    words: &'line Line,
}

/// Режет ось времени окна и добавляет события в `events`.
///
/// `max_events` — общий предел файла; превышение — фатальная ошибка `TooManyEvents`.
pub(super) fn emit_window_events(
    group: &WindowGroup<'_>,
    max_events: usize,
    events: &mut Vec<SubtitleEvent>,
) -> Result<(), SubtitleLoadError> {
    let boundaries = segment_boundaries(&group.paragraphs);
    let mut active: Vec<&ScheduledParagraph<'_>> = Vec::new();
    let mut next_to_start = 0;
    for pair in boundaries.windows(2) {
        let [segment_start_ms, segment_end_ms] = pair else {
            continue;
        };
        // Входят абзацы, начавшиеся не позже начала отрезка; уходят закончившиеся.
        while let Some(paragraph) = group.paragraphs.get(next_to_start)
            && paragraph.timed.start_ms <= *segment_start_ms
        {
            active.push(paragraph);
            next_to_start += 1;
        }
        active.retain(|paragraph| paragraph.timed.end_ms > *segment_start_ms);
        if active.len() > MAX_ACTIVE_PARAGRAPHS {
            active.drain(..active.len() - MAX_ACTIVE_PARAGRAPHS);
        }
        let Some(newest) = active.last() else {
            continue;
        };
        let lines = last_rows(&active, group.max_rows);
        if events.len() >= max_events {
            return Err(SubtitleLoadError::TooManyEvents { limit: max_events });
        }
        let cue = SubtitleTextCue {
            text: StyledText::new(spans_of(&lines, *segment_start_ms)),
            placement: newest.placement.clone(),
        };
        if let Ok(event) = SubtitleEvent::new(
            SubtitleTime::from_millis(*segment_start_ms),
            SubtitleTime::from_millis(*segment_end_ms),
            SubtitleLayer::DEFAULT,
            SubtitleContent::Text(cue),
        ) {
            events.push(event);
        }
    }
    Ok(())
}

/// Моменты, в которых набор абзацев окна меняется: все начала и концы по возрастанию.
fn segment_boundaries(paragraphs: &[ScheduledParagraph<'_>]) -> Vec<i64> {
    let mut boundaries: Vec<i64> = paragraphs
        .iter()
        .flat_map(|paragraph| [paragraph.timed.start_ms, paragraph.timed.end_ms])
        .collect();
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries
}

/// Последние `max_rows` строк активных абзацев (абзацы по возрастанию начала).
fn last_rows<'paragraph>(
    active: &[&'paragraph ScheduledParagraph<'_>],
    max_rows: usize,
) -> Vec<VisibleLine<'paragraph>> {
    let mut lines: Vec<VisibleLine<'paragraph>> = active
        .iter()
        .flat_map(|paragraph| {
            paragraph.timed.lines.iter().map(|words| VisibleLine {
                paragraph_start_ms: paragraph.timed.start_ms,
                words,
            })
        })
        .collect();
    let overflow = lines.len().saturating_sub(max_rows);
    lines.drain(..overflow);
    lines
}

/// Спаны события: слова строк, между строками — переводы строки.
///
/// Время появления слова = начало абзаца + смещение. Если оно не позже начала отрезка, слово
/// уже видно и `reveal_at` не нужен (`None`). Перевод строки появляется вместе с первым
/// словом следующей строки: иначе до него висела бы пустая строка.
fn spans_of(lines: &[VisibleLine<'_>], segment_start_ms: i64) -> Vec<TextSpan> {
    let mut spans = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        let mut line_spans: Vec<TextSpan> = line
            .words
            .iter()
            .map(|word| {
                let reveal_at = word
                    .offset_ms
                    .map(|offset| line.paragraph_start_ms.saturating_add(offset))
                    .filter(|&reveal_ms| reveal_ms > segment_start_ms)
                    .map(SubtitleTime::from_millis);
                TextSpan {
                    reveal_at,
                    ..TextSpan::text(word.text.clone(), word.style)
                }
            })
            .collect();
        if line_index > 0 {
            let reveal_at = line_spans.first().and_then(|span| span.reveal_at);
            spans.push(TextSpan {
                content: SpanContent::LineBreak,
                style: SpanStyle::default(),
                reveal_at,
                ..TextSpan::line_break()
            });
        }
        spans.append(&mut line_spans);
    }
    spans
}
