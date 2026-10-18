//! WebVTT: сигнатура, заголовок и цикл по блокам (`NOTE`, `STYLE`, `REGION`, реплики).
//!
//! Следует алгоритму разбора W3C WebVTT там, где он влияет на результат. Блоки `STYLE` и
//! `REGION` учитываются только до первой реплики. Плохая реплика пропускается и попадает в
//! отчёт, плохая настройка отбрасывается отдельно (реплика сохраняется), CSS никогда не
//! делает файл невалидным. Фатальны лишь отсутствие сигнатуры и превышение `max_events`.

mod blocks;
mod css_value;
mod cue_settings;
mod cue_text;
mod entities;
mod region;
mod style_block;
mod style_sheet;
mod tag;
mod timestamp;
mod timestamp_map;

use subtitle_core::{
    SubtitleContent, SubtitleEvent, SubtitleLayer, SubtitleRegion, SubtitleTextCue,
};

use self::blocks::{Block, NumberedLine, contains_timing_arrow, header_lines, split_into_blocks};
use self::cue_settings::parse_cue_settings;
use self::cue_text::{CueTimeWindow, parse_cue_text};
use self::region::{parse_region_block, register_region};
use self::style_block::parse_style_block;
use self::style_sheet::CueStyleSheet;
use self::timestamp::{TimingLine, is_webvtt_space, parse_timing_line};
use self::timestamp_map::scan_header_for_timestamp_map;
use crate::document::ParsedSubtitleDocument;
use crate::error::SubtitleLoadError;
use crate::limits::SubtitleParseLimits;
use crate::report::{CueSettingName, LineNumber, ParseIssue, ParseIssueKind, SubtitleParseReport};

pub(crate) use blocks::has_webvtt_signature;
pub use timestamp_map::{MpegTsTimestamp90kHz, MpegTsTimestampOutOfRange, WebVttTimestampMap};

/// Текст реплики WebVTT из пакета контейнера: без таймингов и таблицы стилей.
///
/// Окно времени пустое (`start == end`), поэтому караоке-метки внутри текста не откладывают
/// показ: у пакета нет собственной шкалы, к которой их можно было бы привязать.
pub(crate) fn parse_embedded_webvtt_cue_text(cue_text: &str) -> subtitle_core::StyledText {
    let window = CueTimeWindow {
        start: subtitle_core::SubtitleTime::from_millis(0),
        end: subtitle_core::SubtitleTime::from_millis(0),
    };
    parse_cue_text(cue_text, window, &CueStyleSheet::default())
}

/// Сколько первых строк блока может занимать «идентификатор + время» до текста.
const TIMING_LINE_SEARCH_DEPTH: usize = 2;
/// Заголовки блоков.
const NOTE_KEYWORD: &str = "NOTE";
const STYLE_KEYWORD: &str = "STYLE";
const REGION_KEYWORD: &str = "REGION";

/// Разбирает декодированный текст WebVTT.
pub(crate) fn parse_webvtt(
    text: &str,
    limits: &SubtitleParseLimits,
) -> Result<ParsedSubtitleDocument, SubtitleLoadError> {
    if !has_webvtt_signature(text) {
        return Err(SubtitleLoadError::NotWebVtt);
    }
    // CRLF, CR и LF приводим к LF.
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut parser = WebVttParser {
        limits: *limits,
        events: Vec::new(),
        regions: Vec::new(),
        style_sheet: CueStyleSheet::default(),
        report: SubtitleParseReport::new(limits.max_reported_issues),
        seen_cue: false,
    };
    // Заголовок смотрим до разбиения на блоки: `split_into_blocks` строки заголовка отбрасывает.
    let timestamp_map = parser.read_timestamp_map(&normalized);
    for block in split_into_blocks(&normalized) {
        parser.parse_block(&block)?;
    }
    Ok(
        ParsedSubtitleDocument::new(parser.events, parser.regions, parser.report)
            .with_webvtt_timestamp_map(timestamp_map),
    )
}

/// Накопитель результата разбора; владеет событиями, регионами, стилями и отчётом.
struct WebVttParser {
    limits: SubtitleParseLimits,
    events: Vec<SubtitleEvent>,
    regions: Vec<SubtitleRegion>,
    style_sheet: CueStyleSheet,
    report: SubtitleParseReport,
    /// Встречена ли уже реплика: после неё `STYLE` и `REGION` не действуют.
    seen_cue: bool,
}

impl WebVttParser {
    /// Читает `X-TIMESTAMP-MAP` из заголовка; плохие строки уходят в отчёт, не в ошибку.
    fn read_timestamp_map(&mut self, normalized: &str) -> Option<WebVttTimestampMap> {
        let scanned = scan_header_for_timestamp_map(&header_lines(normalized));
        for line_number in scanned.rejected_lines {
            self.report.record_issue(ParseIssue {
                line_number,
                kind: ParseIssueKind::MalformedTimestampMap,
            });
        }
        scanned.map
    }

    /// Определяет вид блока по W3C: строка со стрелкой в первых двух строках — реплика
    /// (этот признак главнее ключевых слов), иначе `NOTE`/`STYLE`/`REGION`, иначе мусор.
    fn parse_block(&mut self, block: &Block<'_>) -> Result<(), SubtitleLoadError> {
        let Some(first_line) = block.first() else {
            return Ok(());
        };
        let timing_index = block
            .iter()
            .take(TIMING_LINE_SEARCH_DEPTH)
            .position(|line| contains_timing_arrow(line.text));
        if let Some(timing_index) = timing_index {
            self.seen_cue = true;
            return self.parse_cue_block(block, timing_index);
        }
        if is_note_header(first_line.text) {
            return Ok(());
        }
        if is_block_keyword(first_line.text, STYLE_KEYWORD) {
            self.parse_style_block(block);
        } else if is_block_keyword(first_line.text, REGION_KEYWORD) {
            self.parse_region_block(block);
        } else {
            self.skip_cue(first_line.number, ParseIssueKind::MissingTimingLine);
        }
        Ok(())
    }

    /// `STYLE` до первой реплики: правила пополняют общую таблицу, неподдержанное считается.
    fn parse_style_block(&mut self, block: &Block<'_>) {
        if self.seen_cue {
            return;
        }
        let css = join_lines(block.iter().skip(1));
        let parsed = parse_style_block(&css);
        self.style_sheet.extend(parsed.rules);
        self.report
            .record_ignored_style_rules(parsed.ignored_rule_count);
    }

    /// `REGION` до первой реплики; поздний регион с тем же `id` заменяет ранний.
    fn parse_region_block(&mut self, block: &Block<'_>) {
        if self.seen_cue {
            return;
        }
        let Some(header_line) = block.first() else {
            return;
        };
        let parsed = parse_region_block(block.iter().skip(1).map(|line| line.text));
        for _ in 0..parsed.rejected_setting_count {
            self.report.record_issue(ParseIssue {
                line_number: header_line.number,
                kind: ParseIssueKind::InvalidRegionSetting,
            });
        }
        match parsed.region {
            Some(region) => register_region(&mut self.regions, region),
            None => self.report.record_issue(ParseIssue {
                line_number: header_line.number,
                kind: ParseIssueKind::RegionWithoutId,
            }),
        }
    }

    /// Реплика: время, настройки, текст. Любая ошибка времени пропускает только эту реплику.
    fn parse_cue_block(
        &mut self,
        block: &Block<'_>,
        timing_index: usize,
    ) -> Result<(), SubtitleLoadError> {
        let Some(timing_line) = block.get(timing_index) else {
            return Ok(());
        };
        let Ok(timing) = parse_timing_line(timing_line.text) else {
            self.skip_cue(timing_line.number, ParseIssueKind::InvalidTimestamp);
            return Ok(());
        };
        if timing.end <= timing.start {
            self.skip_cue(timing_line.number, ParseIssueKind::EndNotAfterStart);
            return Ok(());
        }
        let cue_text = join_lines(block.iter().skip(timing_index + 1));
        self.push_cue(timing_line.number, &timing, &cue_text)
    }

    /// Собирает событие; реплика без видимого текста — `empty_cues`, а не ошибка.
    fn push_cue(
        &mut self,
        timing_line_number: LineNumber,
        timing: &TimingLine<'_>,
        cue_text: &str,
    ) -> Result<(), SubtitleLoadError> {
        let window = CueTimeWindow {
            start: timing.start,
            end: timing.end,
        };
        let text = parse_cue_text(cue_text, window, &self.style_sheet);
        if text.plain_text().trim().is_empty() {
            self.report.record_empty_cue();
            return Ok(());
        }
        let parsed_settings = parse_cue_settings(timing.settings);
        let mut placement = parsed_settings.placement;
        let mut rejected = parsed_settings.rejected;
        // `region:` обязан ссылаться на объявленный регион, иначе настройка игнорируется.
        if let Some(region_id) = placement.region_id.take() {
            if self.regions.iter().any(|region| region.id == region_id) {
                placement.region_id = Some(region_id);
            } else {
                rejected.push(CueSettingName::Region);
            }
        }
        let content = SubtitleContent::Text(SubtitleTextCue { text, placement });
        let Ok(event) =
            SubtitleEvent::new(timing.start, timing.end, SubtitleLayer::DEFAULT, content)
        else {
            self.skip_cue(timing_line_number, ParseIssueKind::EndNotAfterStart);
            return Ok(());
        };
        if self.events.len() >= self.limits.max_events {
            return Err(SubtitleLoadError::TooManyEvents {
                limit: self.limits.max_events,
            });
        }
        for name in rejected.drain(..) {
            self.report.record_issue(ParseIssue {
                line_number: timing_line_number,
                kind: ParseIssueKind::InvalidCueSetting { name },
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

/// Строки блока, склеенные через `\n`.
fn join_lines<'block, 'text: 'block>(
    lines: impl Iterator<Item = &'block NumberedLine<'text>>,
) -> String {
    lines.map(|line| line.text).collect::<Vec<_>>().join("\n")
}

/// `NOTE` + конец строки либо пробел/табуляция.
fn is_note_header(line: &str) -> bool {
    line.strip_prefix(NOTE_KEYWORD)
        .is_some_and(|after| after.is_empty() || after.starts_with(is_webvtt_space))
}

/// Строка состоит из ключевого слова и, возможно, пробелов/табуляций после него.
fn is_block_keyword(line: &str, keyword: &str) -> bool {
    line.trim_end_matches(is_webvtt_space) == keyword
}
