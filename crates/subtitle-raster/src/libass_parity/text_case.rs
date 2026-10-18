//! Разбор текстового кейса `.ass` (`cases/text_*.ass`) в входные данные `render_text_event`.
//!
//! Кейс — один стиль `Default` и одна реплика без тегов, кроме `\N`. Разбор берёт те же
//! строки файла, которые рендерил libass, поэтому стиль и текст не дублируются в тестах.

use std::path::Path;

use subtitle_core::{ScreenAnchor, SpanContent, SpanStyle, StyledText, SubtitleColor, TextSpan};

use super::ass_case::parse_ass_color;
use crate::event_fade::EventFade;
use crate::event_style::{
    AxisPair, BaseDirection, BorderStyle, BreakOpportunities, EventPlacement, EventStyle,
    StyleColors, TextMargins, WrapStyle,
};
use crate::fonts::{FontStyle, FontWeightClass};

/// Описание текстового кейса.
pub(super) struct TextCase {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) text: StyledText,
    pub(super) style: EventStyle,
}

/// Цвет ASS в прямую RGBA (прозрачность ASS 0 = непрозрачный -> альфа 255).
fn to_subtitle_color(text: &str) -> SubtitleColor {
    let color = parse_ass_color(text);
    SubtitleColor {
        red: color.red,
        green: color.green,
        blue: color.blue,
        alpha: 255 - color.transparency,
    }
}

/// Читает кейс.
pub(super) fn load_text_case(path: &Path) -> TextCase {
    let source = std::fs::read_to_string(path).expect("case file must be readable");
    let header = source
        .lines()
        .find(|line| line.starts_with("; reference:"))
        .expect("case must have a reference header");
    let number = |key: &str| -> u32 {
        header
            .split_whitespace()
            .find_map(|part| part.strip_prefix(key))
            .and_then(|value| value.parse().ok())
            .expect("header value must be a number")
    };
    let style_line = source
        .lines()
        .find(|line| line.starts_with("Style:"))
        .expect("case must have a style");
    let fields: Vec<&str> = style_line["Style:".len()..]
        .split(',')
        .map(str::trim)
        .collect();
    let field = |index: usize| *fields.get(index).expect("style field must exist");
    let float = |index: usize| -> f64 { field(index).parse().expect("style number") };
    let integer = |index: usize| -> i32 { field(index).parse().expect("style integer") };

    let wrap_style = match source
        .lines()
        .find_map(|line| line.strip_prefix("WrapStyle:"))
        .map(str::trim)
    {
        Some("1") => WrapStyle::EndOfLine,
        Some("2") => WrapStyle::NoWrap,
        // 0 и 3 (в libass 3 реализован как 0) — умный перенос.
        _ => WrapStyle::Smart,
    };
    let border_style = match integer(15) {
        3 => BorderStyle::OpaqueBox,
        4 => BorderStyle::EventBackground,
        _ => BorderStyle::OutlineAndShadow,
    };
    let style = EventStyle {
        font_family: Some(field(1).to_owned()),
        font_size: float(2),
        font_weight: if integer(7) != 0 {
            FontWeightClass::Bold
        } else {
            FontWeightClass::Regular
        },
        font_slant: if integer(8) != 0 {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
        border_style,
        border: AxisPair::uniform(float(16)),
        shadow: AxisPair::uniform(float(17)),
        colors: StyleColors {
            primary: to_subtitle_color(field(3)),
            secondary: to_subtitle_color(field(4)),
            outline: to_subtitle_color(field(5)),
            back: to_subtitle_color(field(6)),
        },
        alignment: ScreenAnchor::from_numpad(u8::try_from(integer(18)).expect("alignment"))
            .expect("alignment must be 1..=9"),
        margins: TextMargins {
            left: integer(19),
            right: integer(20),
            vertical: integer(21),
        },
        placement: EventPlacement::ByMargins,
        wrap_style,
        break_opportunities: BreakOpportunities::SpacesOnly,
        line_spacing: 0.0,
        // libass: Encoding = -1 — автоопределение, иначе слева направо.
        base_direction: if integer(22) == -1 {
            BaseDirection::Auto
        } else {
            BaseDirection::LeftToRight
        },
        fade: EventFade::NONE,
    };

    let dialogue = source
        .lines()
        .find(|line| line.starts_with("Dialogue:"))
        .expect("case must have a dialogue");
    let raw_text = dialogue
        .splitn(10, ',')
        .nth(9)
        .expect("dialogue must have text");
    let mut spans = Vec::new();
    for (index, part) in raw_text.split("\\N").enumerate() {
        if index > 0 {
            spans.push(TextSpan {
                content: SpanContent::LineBreak,
                ..TextSpan::text("", SpanStyle::default())
            });
        }
        if !part.is_empty() {
            spans.push(TextSpan::text(part, SpanStyle::default()));
        }
    }
    TextCase {
        width: number("width="),
        height: number("height="),
        text: StyledText::new(spans),
        style,
    }
}
