//! SRT (SubRip): блоки, разделённые пустыми строками.
//!
//! Блок = необязательный номер + строка времени + текст до пустой строки. Номер не
//! проверяется (в реальных файлах он часто сбит). Битый блок пропускается целиком и
//! учитывается в отчёте, разбор продолжается со следующего блока.

mod markup;
mod timestamp;

use subtitle_core::{
    CuePlacement, SubtitleContent, SubtitleEvent, SubtitleLayer, SubtitleTextCue, SubtitleTime,
};

use crate::document::ParsedSubtitleDocument;
use crate::error::SubtitleLoadError;
use crate::limits::SubtitleParseLimits;
use crate::report::{LineNumber, ParseIssue, ParseIssueKind, SubtitleParseReport};

pub(crate) use markup::parse_srt_markup as parse_srt_markup_text;
pub(crate) use timestamp::parse_timing_line;

/// Сколько первых строк блока может занимать «номер + время» до текста.
const TIMING_LINE_SEARCH_DEPTH: usize = 2;
/// Маркер строки времени.
const TIMING_MARKER: &str = "-->";

/// Строка блока вместе с номером в исходном файле.
struct NumberedLine<'text> {
    number: LineNumber,
    text: &'text str,
}

/// Разбирает декодированный текст SRT.
///
/// Фатальны только превышение `max_events`; всё остальное — отчёт.
pub(crate) fn parse_srt(
    text: &str,
    limits: &SubtitleParseLimits,
) -> Result<ParsedSubtitleDocument, SubtitleLoadError> {
    let mut parser = SrtParser {
        limits: *limits,
        events: Vec::new(),
        report: SubtitleParseReport::new(limits.max_reported_issues),
    };
    // CRLF, CR и LF приводим к LF; одиночный CR (старый Mac) `str::lines` сам не разделяет.
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut block: Vec<NumberedLine<'_>> = Vec::new();
    for (index, line) in normalized.split('\n').enumerate() {
        if line.trim().is_empty() {
            parser.parse_block(&block)?;
            block.clear();
        } else {
            block.push(NumberedLine {
                number: LineNumber::from_zero_based_index(index),
                text: line,
            });
        }
    }
    parser.parse_block(&block)?;
    Ok(ParsedSubtitleDocument::new(
        parser.events,
        Vec::new(),
        parser.report,
    ))
}

/// Накопитель результата разбора.
struct SrtParser {
    limits: SubtitleParseLimits,
    events: Vec<SubtitleEvent>,
    report: SubtitleParseReport,
}

impl SrtParser {
    /// Разбирает один блок строк (пустой блок — между несколькими пустыми строками — игнорируется).
    fn parse_block(&mut self, block: &[NumberedLine<'_>]) -> Result<(), SubtitleLoadError> {
        let Some(first_line) = block.first() else {
            return Ok(());
        };
        let Some((timing_index, timing_line)) = block
            .iter()
            .enumerate()
            .take(TIMING_LINE_SEARCH_DEPTH)
            .find(|(_, line)| line.text.contains(TIMING_MARKER))
        else {
            self.skip_cue(first_line.number, ParseIssueKind::MissingTimingLine);
            return Ok(());
        };
        let Ok((start, end)) = parse_timing_line(timing_line.text) else {
            self.skip_cue(timing_line.number, ParseIssueKind::InvalidTimestamp);
            return Ok(());
        };
        let text_lines: Vec<&str> = block
            .iter()
            .skip(timing_index + 1)
            .map(|line| line.text.trim())
            .collect();
        self.push_cue(timing_line.number, start, end, &text_lines)
    }

    /// Собирает событие из времени и строк текста; пустой текст — не ошибка, а `empty_cues`.
    fn push_cue(
        &mut self,
        timing_line_number: LineNumber,
        start: SubtitleTime,
        end: SubtitleTime,
        text_lines: &[&str],
    ) -> Result<(), SubtitleLoadError> {
        let parsed = markup::parse_srt_markup(&text_lines.join("\n"));
        if parsed.text.is_empty() {
            self.report.record_empty_cue();
            return Ok(());
        }
        let placement = CuePlacement {
            anchor: parsed.anchor,
            ..CuePlacement::default()
        };
        let content = SubtitleContent::Text(SubtitleTextCue {
            text: parsed.text,
            placement,
        });
        let Ok(event) = SubtitleEvent::new(start, end, SubtitleLayer::DEFAULT, content) else {
            self.skip_cue(timing_line_number, ParseIssueKind::EndNotAfterStart);
            return Ok(());
        };
        if self.events.len() >= self.limits.max_events {
            return Err(SubtitleLoadError::TooManyEvents {
                limit: self.limits.max_events,
            });
        }
        self.events.push(event);
        Ok(())
    }

    fn skip_cue(&mut self, line_number: LineNumber, kind: ParseIssueKind) {
        self.report
            .record_skipped_cue(ParseIssue { line_number, kind });
    }
}
