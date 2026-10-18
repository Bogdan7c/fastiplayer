//! Текст абзаца srv3 -> строки слов: очистка, перевод строки, стиль пера.

use subtitle_core::SpanStyle;

use super::model::{RawDocument, RawParagraph};

/// Символ нулевой ширины: YTSubConverter и YouTube вставляют его вокруг слов как «подкладку».
const ZERO_WIDTH_SPACE: char = '\u{200B}';

/// Слово (или фраза) одной строки с собственным стилем и временем появления.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Word {
    pub(super) text: String,
    pub(super) style: SpanStyle,
    /// Смещение появления от начала абзаца, мс; `None` — виден с начала абзаца.
    pub(super) offset_ms: Option<i64>,
}

/// Одна строка абзаца.
pub(super) type Line = Vec<Word>;

/// Строит строки абзаца.
///
/// - `U+200B` и `\r` удаляются;
/// - `\n` (литеральный в тексте или `<br/>`) начинает новую строку;
/// - пробелы по краям строки обрезаются (подкладка YTSubConverter), внутри — сохраняются:
///   в автосубтитрах пробел перед словом — это его разделитель;
/// - пустые строки отбрасываются; абзац без видимого текста даёт пустой список;
/// - пробельный текст между `<s>` внутри абзаца (форматирование файла) игнорируется.
pub(super) fn build_lines(paragraph: &RawParagraph, document: &RawDocument) -> Vec<Line> {
    let has_word_spans = paragraph.spans.iter().any(|span| !span.is_direct_text);
    let mut lines: Vec<Line> = vec![Vec::new()];
    for span in &paragraph.spans {
        if has_word_spans && span.is_direct_text && span.text.trim().is_empty() {
            continue;
        }
        let pen_id = span.pen_id.or(paragraph.pen_id);
        let style = pen_id
            .and_then(|id| document.pens.get(&id))
            .map(|pen| pen.span_style())
            .unwrap_or_default();
        let cleaned: String = span
            .text
            .chars()
            .filter(|&ch| ch != ZERO_WIDTH_SPACE && ch != '\r')
            .collect();
        for (piece_index, piece) in cleaned.split('\n').enumerate() {
            if piece_index > 0 {
                lines.push(Vec::new());
            }
            if piece.is_empty() {
                continue;
            }
            if let Some(current) = lines.last_mut() {
                current.push(Word {
                    text: piece.to_owned(),
                    style,
                    offset_ms: span.offset_ms.filter(|&offset| offset > 0),
                });
            }
        }
    }
    lines.iter_mut().for_each(trim_line);
    lines.retain(|line| !line.is_empty());
    lines
}

/// Обрезает пробелы в начале первого и в конце последнего слова строки; опустевшие слова
/// удаляются (вместе с их временем появления).
fn trim_line(line: &mut Line) {
    while let Some(first) = line.first_mut() {
        first.text = first.text.trim_start().to_owned();
        if first.text.is_empty() {
            line.remove(0);
        } else {
            break;
        }
    }
    while let Some(last) = line.last_mut() {
        last.text = last.text.trim_end().to_owned();
        if last.text.is_empty() {
            line.pop();
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::srv3::model::RawSpan;

    fn paragraph(spans: Vec<RawSpan>) -> RawParagraph {
        RawParagraph {
            ordinal: 0,
            start_ms: Some(0),
            duration_ms: Some(1000),
            window_id: None,
            position_id: None,
            style_id: None,
            pen_id: None,
            is_append: false,
            spans,
        }
    }

    fn word_span(offset_ms: Option<i64>, text: &str) -> RawSpan {
        RawSpan {
            offset_ms,
            pen_id: None,
            text: text.to_owned(),
            is_direct_text: false,
        }
    }

    fn texts(lines: &[Line]) -> Vec<Vec<&str>> {
        lines
            .iter()
            .map(|line| line.iter().map(|word| word.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn zero_width_spaces_and_padding_are_removed_but_inner_spaces_stay() {
        let spans = vec![
            word_span(None, "\u{200B}"),
            word_span(None, "\u{200B} \u{200B}- hi \u{200B} \u{200B}"),
            word_span(Some(300), " there"),
            word_span(None, "\u{200B}"),
        ];
        let lines = build_lines(&paragraph(spans), &RawDocument::default());
        assert_eq!(texts(&lines), vec![vec!["- hi  ", " there"]]);
        assert_eq!(lines[0][1].offset_ms, Some(300));
    }

    #[test]
    fn newline_inside_text_starts_a_new_line() {
        let spans = vec![RawSpan {
            offset_ms: None,
            pen_id: None,
            text: "first\nsecond\n".to_owned(),
            is_direct_text: true,
        }];
        let lines = build_lines(&paragraph(spans), &RawDocument::default());
        assert_eq!(texts(&lines), vec![vec!["first"], vec!["second"]]);
    }

    #[test]
    fn whitespace_only_paragraph_has_no_lines() {
        let spans = vec![RawSpan {
            offset_ms: None,
            pen_id: None,
            text: "\n".to_owned(),
            is_direct_text: true,
        }];
        assert!(build_lines(&paragraph(spans), &RawDocument::default()).is_empty());
    }

    #[test]
    fn formatting_whitespace_between_word_spans_is_not_a_line_break() {
        let spans = vec![
            word_span(None, "a"),
            RawSpan {
                offset_ms: None,
                pen_id: None,
                text: "\n  ".to_owned(),
                is_direct_text: true,
            },
            word_span(Some(10), " b"),
        ];
        let lines = build_lines(&paragraph(spans), &RawDocument::default());
        assert_eq!(texts(&lines), vec![vec!["a", " b"]]);
    }

    #[test]
    fn zero_offset_means_visible_from_paragraph_start() {
        let lines = build_lines(
            &paragraph(vec![word_span(Some(0), "a"), word_span(Some(-5), " b")]),
            &RawDocument::default(),
        );
        assert!(lines[0].iter().all(|word| word.offset_ms.is_none()));
    }
}
