//! Разбиение файла WebVTT на блоки: сигнатура, заголовок и последовательность блоков.
//!
//! Блок — группа непустых строк; границей служит ПУСТАЯ строка (строка из пробелов не
//! граница: так делает W3C и браузеры). Дополнительная граница — строка со стрелкой `-->`
//! в блоке, который уже «заполнен»: так реплика без пустой строки перед следующей
//! не склеивается с ней.

use super::timestamp::{TIMING_ARROW, is_webvtt_space};
use crate::report::LineNumber;

/// Сигнатура файла.
const SIGNATURE: &str = "WEBVTT";
/// Символ BOM; декодер его уже снял, но парсер не должен зависеть от этого.
const BYTE_ORDER_MARK: char = '\u{feff}';

/// Строка блока вместе с номером в исходном файле.
#[derive(Debug, Clone, Copy)]
pub(super) struct NumberedLine<'text> {
    pub(super) number: LineNumber,
    pub(super) text: &'text str,
}

/// Блок: непустые подряд идущие строки.
pub(super) type Block<'text> = Vec<NumberedLine<'text>>;

/// Начинается ли текст с сигнатуры `WEBVTT` (за ней конец строки, пробел или табуляция).
pub(crate) fn has_webvtt_signature(text: &str) -> bool {
    let first_line = text
        .trim_start_matches(BYTE_ORDER_MARK)
        .split(['\n', '\r'])
        .next()
        .unwrap_or_default();
    first_line
        .strip_prefix(SIGNATURE)
        .is_some_and(|after| after.is_empty() || after.starts_with(is_webvtt_space))
}

/// Режет нормализованный (только `\n`) текст на блоки, пропуская сигнатуру и заголовок.
///
/// Заголовок — строки после сигнатуры до первой пустой строки. Если среди них оказалась
/// строка со стрелкой, значит, автор забыл пустую строку: с неё начинается первая реплика.
pub(super) fn split_into_blocks(normalized: &str) -> Vec<Block<'_>> {
    let mut blocks = Vec::new();
    let mut current: Block<'_> = Vec::new();
    let mut in_header = true;
    for (index, text) in normalized.split('\n').enumerate().skip(1) {
        let line = NumberedLine {
            number: LineNumber::from_zero_based_index(index),
            text,
        };
        if text.is_empty() {
            in_header = false;
            flush_block(&mut blocks, &mut current);
            continue;
        }
        let has_arrow = contains_timing_arrow(text);
        if in_header {
            if !has_arrow {
                continue;
            }
            in_header = false;
        } else if has_arrow && block_is_complete_before_arrow(&current) {
            flush_block(&mut blocks, &mut current);
        }
        current.push(line);
    }
    flush_block(&mut blocks, &mut current);
    blocks
}

/// Строка содержит разделитель времени `-->`.
pub(super) fn contains_timing_arrow(text: &str) -> bool {
    text.contains(TIMING_ARROW)
}

/// Строка со стрелкой начинает новый блок, если в текущем уже есть стрелка (две реплики
/// подряд) или две и более строк (на месте идентификатора может стоять только одна).
fn block_is_complete_before_arrow(current: &[NumberedLine<'_>]) -> bool {
    current.len() >= 2 || current.iter().any(|line| contains_timing_arrow(line.text))
}

fn flush_block<'text>(blocks: &mut Vec<Block<'text>>, current: &mut Block<'text>) {
    if !current.is_empty() {
        blocks.push(std::mem::take(current));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_texts(blocks: &[Block<'_>]) -> Vec<Vec<String>> {
        blocks
            .iter()
            .map(|block| block.iter().map(|line| line.text.to_owned()).collect())
            .collect()
    }

    #[test]
    fn signature_requires_boundary_after_keyword() {
        assert!(has_webvtt_signature("WEBVTT"));
        assert!(has_webvtt_signature("WEBVTT\n\n"));
        assert!(has_webvtt_signature("WEBVTT - some title"));
        assert!(has_webvtt_signature("WEBVTT\tx"));
        assert!(has_webvtt_signature("\u{feff}WEBVTT\r\n"));
        assert!(!has_webvtt_signature("WEBVTTX"));
        assert!(!has_webvtt_signature("webvtt"));
        assert!(!has_webvtt_signature(" WEBVTT"));
        assert!(!has_webvtt_signature("1\n00:00:01,000 --> 00:00:02,000"));
        assert!(!has_webvtt_signature(""));
    }

    #[test]
    fn header_is_skipped_and_blocks_are_split_on_empty_lines() {
        let blocks = split_into_blocks(
            "WEBVTT\nKind: captions\n\nid\n00:01.000 --> 00:02.000\ntext\n\n\nNOTE x\n",
        );
        assert_eq!(
            block_texts(&blocks),
            [
                vec!["id", "00:01.000 --> 00:02.000", "text"],
                vec!["NOTE x"]
            ]
        );
    }

    #[test]
    fn arrow_line_in_header_starts_first_cue() {
        let blocks = split_into_blocks("WEBVTT\n00:01.000 --> 00:02.000\ntext\n");
        assert_eq!(
            block_texts(&blocks),
            [vec!["00:01.000 --> 00:02.000", "text"]]
        );
    }

    #[test]
    fn missing_blank_line_between_cues_still_separates_them() {
        let blocks = split_into_blocks(
            "WEBVTT\n\n00:01.000 --> 00:02.000\na\nb\n00:03.000 --> 00:04.000\nc\n",
        );
        assert_eq!(
            block_texts(&blocks),
            [
                vec!["00:01.000 --> 00:02.000", "a", "b"],
                vec!["00:03.000 --> 00:04.000", "c"]
            ]
        );
    }

    #[test]
    fn whitespace_only_line_is_not_a_separator_and_line_numbers_are_one_based() {
        let blocks = split_into_blocks("WEBVTT\n\n00:01.000 --> 00:02.000\na\n \nb\n");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].len(), 4);
        assert_eq!(blocks[0][0].number.get(), 3);
    }
}
