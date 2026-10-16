//! ASS/SSA в Matroska: `ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text`.
//!
//! Только `Text` (последнее поле, запятые внутри него допустимы, поэтому делим не больше чем
//! на девять частей). Из текста вырезаются блоки `{...}`; `\N`/`\n` — перевод строки,
//! `\h` — неразрывный пробел. Стили и позиции ASS — сессии 08–10.

use subtitle_core::{CuePlacement, SpanStyle, SubtitleLayer, TextSpan};

use super::text_lines::{check_char_limit, decode_utf8_lossy, finish_cue, trim_blank_edges};
use super::{EmbeddedCueError, EmbeddedCueLimits, EmbeddedCueMalformation, EmbeddedCueOutcome};

/// Число полей строки Events в Matroska (последнее — текст).
const ASS_FIELD_COUNT: usize = 9;
/// Индекс поля `Layer`.
const LAYER_FIELD_INDEX: usize = 1;
/// Индекс поля `Text`.
const TEXT_FIELD_INDEX: usize = ASS_FIELD_COUNT - 1;
/// Неразрывный пробел (`\h`).
const NO_BREAK_SPACE: char = '\u{a0}';

pub(super) fn parse_ass_event_line(
    payload: &[u8],
    limits: &EmbeddedCueLimits,
) -> Result<EmbeddedCueOutcome, EmbeddedCueError> {
    let decoded = decode_utf8_lossy(payload);
    check_char_limit(&decoded.text, limits)?;
    let fields: Vec<&str> = decoded.text.splitn(ASS_FIELD_COUNT, ',').collect();
    let Some(raw_text) = fields.get(TEXT_FIELD_INDEX) else {
        return Err(EmbeddedCueError::Malformed(
            EmbeddedCueMalformation::AssTooFewFields {
                expected: ASS_FIELD_COUNT,
                found: fields.len(),
            },
        ));
    };
    // Нечисловой Layer (SSA `Marked`) — не ошибка: берём слой по умолчанию.
    let layer = fields
        .get(LAYER_FIELD_INDEX)
        .and_then(|field| field.trim().parse::<i32>().ok())
        .map_or(SubtitleLayer::DEFAULT, SubtitleLayer::new);
    let text = visible_text(raw_text);
    let spans = lines_to_spans(&text);
    Ok(finish_cue(
        trim_blank_edges(spans),
        CuePlacement::default(),
        layer,
        decoded.had_replacement_characters,
        false,
    ))
}

/// Вырезает `{...}` и раскрывает `\N`, `\n`, `\h`; результат — текст с `\n` как переводом строки.
fn visible_text(raw_text: &str) -> String {
    let mut visible = String::with_capacity(raw_text.len());
    let mut rest = raw_text;
    while let Some(ch) = rest.chars().next() {
        match ch {
            // Блок без закрывающей `}` — обычный текст (так делает и libass).
            '{' if rest.contains('}') => {
                let close_index = rest.find('}').unwrap_or(0);
                rest = rest.get(close_index + 1..).unwrap_or_default();
            }
            '\\' => {
                let escaped = rest.get(1..).and_then(|tail| tail.chars().next());
                match escaped {
                    Some('N' | 'n') => visible.push('\n'),
                    Some('h') => visible.push(NO_BREAK_SPACE),
                    // Неизвестный escape остаётся как есть.
                    Some(_) | None => {
                        visible.push('\\');
                        rest = rest.get(1..).unwrap_or_default();
                        continue;
                    }
                }
                rest = rest.get(1 + 1..).unwrap_or_default();
            }
            _ => {
                visible.push(ch);
                rest = rest.get(ch.len_utf8()..).unwrap_or_default();
            }
        }
    }
    visible
}

/// Текст с `\n` -> спаны одного обычного стиля.
fn lines_to_spans(text: &str) -> Vec<TextSpan> {
    let mut spans = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            spans.push(TextSpan::line_break());
        }
        if !line.is_empty() {
            spans.push(TextSpan::text(line, SpanStyle::default()));
        }
    }
    spans
}
