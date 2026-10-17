// Порт поведения libass 0.17.5 (ISC): ass.c (process_event_tail, event_format_fallback).
//
// Copyright (C) 2006 Evgeniy Stepanov <eugeni.stepanov@gmail.com>
// Copyright (C) 2009 Grigori Goronzy <greg@geekmind.org>
//
// Permission to use, copy, modify, and distribute this software for any
// purpose with or without fee is hereby granted, provided that the above
// copyright notice and this permission notice appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
// WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
// MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
// ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
// ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
// OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
//
// Rust port © Fastiplayer contributors.

//! Поля строки события: раскладка `Format:` и разбор «хвоста» `Dialogue:` / пакета MKV
//! (порт `process_event_tail` из libass).

use subtitle_core::{AssEventField, AssEventFormat, AssEventMargins};

use super::scan::{FieldCursor, TrailingSpaces, parse_int_header, string_to_timecode_ms};

/// Стандартная раскладка ASS (`ass_event_format`).
const ASS_EVENT_FORMAT: &[AssEventField] = &[
    AssEventField::Layer,
    AssEventField::Start,
    AssEventField::End,
    AssEventField::Style,
    AssEventField::Name,
    AssEventField::MarginL,
    AssEventField::MarginR,
    AssEventField::MarginV,
    AssEventField::Effect,
    AssEventField::Text,
];

/// Раскладка SSA (`ssa_event_format`): на месте `Layer` стоит `Marked`, значение которого
/// libass не читает; форма полей та же.
const SSA_EVENT_FORMAT: &[AssEventField] = &[
    AssEventField::Ignored,
    AssEventField::Start,
    AssEventField::End,
    AssEventField::Style,
    AssEventField::Name,
    AssEventField::MarginL,
    AssEventField::MarginR,
    AssEventField::MarginV,
    AssEventField::Effect,
    AssEventField::Text,
];

/// Какая версия формата выбирает раскладку по умолчанию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EventFormatFlavor {
    Ass,
    Ssa,
}

/// Раскладка по умолчанию (`event_format_fallback`).
pub(super) fn default_event_format(flavor: EventFormatFlavor) -> AssEventFormat {
    AssEventFormat::new(match flavor {
        EventFormatFlavor::Ass => ASS_EVENT_FORMAT.to_vec(),
        EventFormatFlavor::Ssa => SSA_EVENT_FORMAT.to_vec(),
    })
}

/// Разбирает текст после `Format:` в раскладку. Имена нечувствительны к регистру;
/// `Actor` — синоним `Name`, а `Duration` libass читает так же, как `End`.
pub(super) fn parse_event_format(format_text: &str) -> AssEventFormat {
    let mut cursor = FieldCursor::new(format_text);
    let mut fields = Vec::new();
    while let Some(name) = cursor.next_token(TrailingSpaces::Trim) {
        fields.push(field_from_name(name));
    }
    AssEventFormat::new(fields)
}

fn field_from_name(name: &str) -> AssEventField {
    const NAMES: &[(&str, AssEventField)] = &[
        ("Layer", AssEventField::Layer),
        ("Start", AssEventField::Start),
        ("End", AssEventField::End),
        ("Duration", AssEventField::End),
        ("Style", AssEventField::Style),
        ("Name", AssEventField::Name),
        ("Actor", AssEventField::Name),
        ("MarginL", AssEventField::MarginL),
        ("MarginR", AssEventField::MarginR),
        ("MarginV", AssEventField::MarginV),
        ("Effect", AssEventField::Effect),
        ("Text", AssEventField::Text),
    ];
    NAMES
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map_or(AssEventField::Ignored, |(_, field)| *field)
}

/// Сырые поля события до превращения в `AssCue` (стиль ещё не найден по имени).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RawEventFields {
    pub(super) layer: i32,
    /// Начало в миллисекундах; 0, если поля нет или время не разобрано.
    pub(super) start_ms: i64,
    /// Конец в миллисекундах (аналогично).
    pub(super) end_ms: i64,
    pub(super) style_name: String,
    pub(super) name: String,
    pub(super) effect: String,
    pub(super) margins: AssEventMargins,
    pub(super) text: String,
}

impl RawEventFields {
    fn empty() -> Self {
        Self {
            layer: 0,
            start_ms: 0,
            end_ms: 0,
            style_name: String::new(),
            name: String::new(),
            effect: String::new(),
            margins: AssEventMargins::default(),
            text: String::new(),
        }
    }
}

/// Строка события не соответствует раскладке: не хватает полей или нет `Text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BrokenEventLine;

/// Разбирает хвост строки события по раскладке `fields`.
///
/// Значения идут по порядку полей раскладки через запятую; `Text` забирает остаток строки
/// целиком (запятые внутри допустимы) и должен быть последним полем. Пробелы справа у
/// текста (`\r`, табуляция, пробел) срезаются. Если значений меньше, чем полей до `Text`,
/// или `Text` в раскладке нет — строка битая.
pub(super) fn parse_event_tail(
    fields: &[AssEventField],
    tail: &str,
) -> Result<RawEventFields, BrokenEventLine> {
    let mut cursor = FieldCursor::new(tail);
    let mut event = RawEventFields::empty();
    for field in fields {
        if *field == AssEventField::Text {
            let text = cursor.remainder();
            event.text = text.trim_end_matches(['\r', '\t', ' ']).to_owned();
            return Ok(event);
        }
        let token = cursor
            .next_token(TrailingSpaces::Keep)
            .ok_or(BrokenEventLine)?;
        match field {
            AssEventField::Layer => event.layer = parse_int_header(token),
            AssEventField::Style => event.style_name = token.to_owned(),
            AssEventField::Name => event.name = token.to_owned(),
            AssEventField::Effect => event.effect = token.to_owned(),
            AssEventField::MarginL => event.margins.left = parse_int_header(token),
            AssEventField::MarginR => event.margins.right = parse_int_header(token),
            AssEventField::MarginV => event.margins.vertical = parse_int_header(token),
            AssEventField::Start => event.start_ms = string_to_timecode_ms(token).unwrap_or(0),
            AssEventField::End => event.end_ms = string_to_timecode_ms(token).unwrap_or(0),
            AssEventField::Ignored | AssEventField::Text => {}
        }
    }
    Err(BrokenEventLine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_line_is_split_by_format() {
        let fields = default_event_format(EventFormatFlavor::Ass);
        let event = parse_event_tail(
            fields.fields(),
            "3,0:00:01.50,0:00:02.00,Sign,Bob,10,20,30,Banner;1,Hello, world  \r",
        )
        .expect("valid line");
        assert_eq!(event.layer, 3);
        assert_eq!((event.start_ms, event.end_ms), (1500, 2000));
        assert_eq!(event.style_name, "Sign");
        assert_eq!(event.name, "Bob");
        assert_eq!(
            event.margins,
            AssEventMargins {
                left: 10,
                right: 20,
                vertical: 30
            }
        );
        assert_eq!(event.effect, "Banner;1");
        assert_eq!(event.text, "Hello, world");
    }

    #[test]
    fn custom_order_and_aliases_are_honoured() {
        let format = parse_event_format(" Start, End , Actor, Style, Text");
        let event = parse_event_tail(format.fields(), "0:00:05.00,0:00:06.00,Alice,Narrator,Line")
            .expect("valid line");
        assert_eq!((event.start_ms, event.end_ms), (5000, 6000));
        assert_eq!(event.name, "Alice");
        assert_eq!(event.style_name, "Narrator");
        assert_eq!(event.text, "Line");
        assert_eq!(event.layer, 0);
    }

    #[test]
    fn missing_values_or_text_field_make_line_broken() {
        let fields = default_event_format(EventFormatFlavor::Ass);
        assert_eq!(
            parse_event_tail(fields.fields(), "0,0:00:01.00,0:00:02.00"),
            Err(BrokenEventLine)
        );
        let no_text = parse_event_format("Start, End");
        assert_eq!(
            parse_event_tail(no_text.fields(), "0:00:01.00,0:00:02.00,x"),
            Err(BrokenEventLine)
        );
    }

    #[test]
    fn empty_text_after_last_comma_is_valid() {
        let fields = default_event_format(EventFormatFlavor::Ass);
        let event = parse_event_tail(fields.fields(), "0,0:00:01.00,0:00:02.00,Default,,0,0,0,,")
            .expect("valid line");
        assert_eq!(event.text, "");
    }

    #[test]
    fn broken_timecode_becomes_zero_like_libass() {
        let fields = default_event_format(EventFormatFlavor::Ass);
        let event = parse_event_tail(fields.fields(), "0,bad,0:00:02.00,Default,,0,0,0,,x")
            .expect("valid line");
        assert_eq!((event.start_ms, event.end_ms), (0, 2000));
    }

    #[test]
    fn ssa_marked_field_does_not_set_layer() {
        let fields = default_event_format(EventFormatFlavor::Ssa);
        let event = parse_event_tail(
            fields.fields(),
            "Marked=0,0:00:01.00,0:00:02.00,Default,,0,0,0,,x",
        )
        .expect("valid line");
        assert_eq!(event.layer, 0);
        assert_eq!(event.text, "x");
    }
}
