// Порт поведения libass 0.17.5 (ISC): ass.c (process_style, set_style_alpha, set_default_style).
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

//! Строка `Style:` и её раскладка `Format:` (порт `process_style` и `set_style_alpha`).

use subtitle_core::{AssAlignment, AssBold, AssBorderStyle, AssColor, AssStyle};

use super::scan::{FieldCursor, TrailingSpaces, parse_f64, parse_int_header};

/// Поле строки стиля.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StyleField {
    Name,
    FontName,
    PrimaryColour,
    SecondaryColour,
    OutlineColour,
    BackColour,
    /// Только SSA: прозрачность `AlphaLevel`.
    AlphaLevel,
    FontSize,
    Bold,
    Italic,
    Underline,
    StrikeOut,
    ScaleX,
    ScaleY,
    Spacing,
    Angle,
    BorderStyle,
    Outline,
    Shadow,
    Alignment,
    MarginL,
    MarginR,
    MarginV,
    Encoding,
    /// Неизвестное поле (в том числе SSA `TertiaryColour`): значение пропускается.
    Ignored,
}

/// Версия формата стилей: определяет умолчания и правила выравнивания/цвета.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StyleFlavor {
    Ass,
    Ssa,
}

const ASS_STYLE_FORMAT: &[StyleField] = &[
    StyleField::Name,
    StyleField::FontName,
    StyleField::FontSize,
    StyleField::PrimaryColour,
    StyleField::SecondaryColour,
    StyleField::OutlineColour,
    StyleField::BackColour,
    StyleField::Bold,
    StyleField::Italic,
    StyleField::Underline,
    StyleField::StrikeOut,
    StyleField::ScaleX,
    StyleField::ScaleY,
    StyleField::Spacing,
    StyleField::Angle,
    StyleField::BorderStyle,
    StyleField::Outline,
    StyleField::Shadow,
    StyleField::Alignment,
    StyleField::MarginL,
    StyleField::MarginR,
    StyleField::MarginV,
    StyleField::Encoding,
];

/// Раскладка SSA (`ssa_style_format`). `TertiaryColour` здесь — `Ignored`: цвет контура в
/// SSA берётся из `BackColour`.
const SSA_STYLE_FORMAT: &[StyleField] = &[
    StyleField::Name,
    StyleField::FontName,
    StyleField::FontSize,
    StyleField::PrimaryColour,
    StyleField::SecondaryColour,
    StyleField::Ignored,
    StyleField::BackColour,
    StyleField::Bold,
    StyleField::Italic,
    StyleField::BorderStyle,
    StyleField::Outline,
    StyleField::Shadow,
    StyleField::Alignment,
    StyleField::MarginL,
    StyleField::MarginR,
    StyleField::MarginV,
    StyleField::AlphaLevel,
    StyleField::Encoding,
];

/// Раскладка по умолчанию для строк `Style:` без `Format:`.
pub(super) fn default_style_format(flavor: StyleFlavor) -> Vec<StyleField> {
    match flavor {
        StyleFlavor::Ass => ASS_STYLE_FORMAT.to_vec(),
        StyleFlavor::Ssa => SSA_STYLE_FORMAT.to_vec(),
    }
}

/// Разбирает текст после `Format:` в список полей.
pub(super) fn parse_style_format(format_text: &str) -> Vec<StyleField> {
    let mut cursor = FieldCursor::new(format_text);
    let mut fields = Vec::new();
    while let Some(name) = cursor.next_token(TrailingSpaces::Trim) {
        fields.push(style_field_from_name(name));
    }
    fields
}

fn style_field_from_name(name: &str) -> StyleField {
    const NAMES: &[(&str, StyleField)] = &[
        ("Name", StyleField::Name),
        ("FontName", StyleField::FontName),
        ("PrimaryColour", StyleField::PrimaryColour),
        ("SecondaryColour", StyleField::SecondaryColour),
        ("OutlineColour", StyleField::OutlineColour),
        ("BackColour", StyleField::BackColour),
        ("AlphaLevel", StyleField::AlphaLevel),
        ("FontSize", StyleField::FontSize),
        ("Bold", StyleField::Bold),
        ("Italic", StyleField::Italic),
        ("Underline", StyleField::Underline),
        ("StrikeOut", StyleField::StrikeOut),
        ("ScaleX", StyleField::ScaleX),
        ("ScaleY", StyleField::ScaleY),
        ("Spacing", StyleField::Spacing),
        ("Angle", StyleField::Angle),
        ("BorderStyle", StyleField::BorderStyle),
        ("Outline", StyleField::Outline),
        ("Shadow", StyleField::Shadow),
        ("Alignment", StyleField::Alignment),
        ("MarginL", StyleField::MarginL),
        ("MarginR", StyleField::MarginR),
        ("MarginV", StyleField::MarginV),
        ("Encoding", StyleField::Encoding),
    ];
    NAMES
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map_or(StyleField::Ignored, |(_, field)| *field)
}

/// Цвет стиля из `&HAABBGGRR`: `parse_color_header` (после перестановки байт libass получает
/// RGBA, где младший байт — прозрачность).
fn parse_style_color(token: &str) -> AssColor {
    let value = parse_int_header(token) as u32;
    AssColor::new(
        (value & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        ((value >> 16) & 0xff) as u8,
        (value >> 24) as u8,
    )
}

/// Выравнивание из поля стиля: ASS — цифровая клавиатура, SSA — старая схема с двумя
/// «совместимыми с VSFilter» заменами (8 -> 3, 4 -> 11).
fn style_alignment(flavor: StyleFlavor, value: i32) -> AssAlignment {
    match flavor {
        StyleFlavor::Ass => AssAlignment::from_numpad(value),
        StyleFlavor::Ssa => match value {
            8 => AssAlignment::from_libass_bits(3),
            4 => AssAlignment::from_libass_bits(11),
            other => AssAlignment::from_libass_bits(other),
        },
    }
}

/// Значения стиля до финальной нормализации. Умолчания — как у обнулённой структуры libass
/// (поля, которых нет в строке, остаются нулевыми), кроме масштабов (100).
struct StyleDraft {
    name: Option<String>,
    font_name: Option<String>,
    font_size: f64,
    primary: AssColor,
    secondary: AssColor,
    outline_color: AssColor,
    back: AssColor,
    bold: i32,
    italic: i32,
    underline: i32,
    strike_out: i32,
    scale_x: f64,
    scale_y: f64,
    spacing: f64,
    angle: f64,
    border_style: i32,
    outline: f64,
    shadow: f64,
    alignment: AssAlignment,
    margin_left: i32,
    margin_right: i32,
    margin_vertical: i32,
    encoding: i32,
    ssa_alpha: i32,
}

impl StyleDraft {
    fn blank() -> Self {
        let zero_color = AssColor::new(0, 0, 0, 0);
        Self {
            name: None,
            font_name: None,
            font_size: 0.0,
            primary: zero_color,
            secondary: zero_color,
            outline_color: zero_color,
            back: zero_color,
            bold: 0,
            italic: 0,
            underline: 0,
            strike_out: 0,
            scale_x: 100.0,
            scale_y: 100.0,
            spacing: 0.0,
            angle: 0.0,
            border_style: 0,
            outline: 0.0,
            shadow: 0.0,
            alignment: AssAlignment::from_libass_bits(0),
            margin_left: 0,
            margin_right: 0,
            margin_vertical: 0,
            encoding: 0,
            ssa_alpha: 0,
        }
    }

    /// Применяет значение одного поля.
    fn apply(&mut self, field: StyleField, flavor: StyleFlavor, token: &str) {
        match field {
            // `*` в начале имени «ничего не значат»: VSFilter убирает их сразу.
            StyleField::Name => self.name = Some(token.trim_start_matches('*').to_owned()),
            StyleField::FontName => self.font_name = Some(token.to_owned()),
            StyleField::PrimaryColour => self.primary = parse_style_color(token),
            StyleField::SecondaryColour => self.secondary = parse_style_color(token),
            StyleField::OutlineColour => self.outline_color = parse_style_color(token),
            StyleField::BackColour => {
                self.back = parse_style_color(token);
                // SSA использует BackColour и для контура, и для тени.
                if flavor == StyleFlavor::Ssa {
                    self.outline_color = self.back;
                }
            }
            StyleField::AlphaLevel => self.ssa_alpha = parse_int_header(token),
            StyleField::FontSize => self.font_size = parse_f64(token),
            StyleField::Bold => self.bold = parse_int_header(token),
            StyleField::Italic => self.italic = parse_int_header(token),
            StyleField::Underline => self.underline = parse_int_header(token),
            StyleField::StrikeOut => self.strike_out = parse_int_header(token),
            StyleField::Spacing => self.spacing = parse_f64(token),
            StyleField::Angle => self.angle = parse_f64(token),
            StyleField::BorderStyle => self.border_style = parse_int_header(token),
            StyleField::Alignment => {
                self.alignment = style_alignment(flavor, parse_int_header(token));
            }
            StyleField::MarginL => self.margin_left = parse_int_header(token),
            StyleField::MarginR => self.margin_right = parse_int_header(token),
            StyleField::MarginV => self.margin_vertical = parse_int_header(token),
            StyleField::Encoding => self.encoding = parse_int_header(token),
            StyleField::ScaleX => self.scale_x = parse_f64(token),
            StyleField::ScaleY => self.scale_y = parse_f64(token),
            StyleField::Outline => self.outline = parse_f64(token),
            StyleField::Shadow => self.shadow = parse_f64(token),
            StyleField::Ignored => {}
        }
    }

    /// Финальная нормализация `process_style`: SSA-прозрачность, неотрицательные значения,
    /// флаги, имена по умолчанию.
    fn finish(mut self, flavor: StyleFlavor) -> AssStyle {
        if flavor == StyleFlavor::Ssa {
            // VSFilter: прозрачность тени в SSA всегда 0x80, остальные цвета — AlphaLevel.
            let front = self.ssa_alpha.clamp(0, 0xff) as u8;
            self.primary = self.primary.with_transparency(front);
            self.secondary = self.secondary.with_transparency(front);
            self.outline_color = self.outline_color.with_transparency(front);
            self.back = self.back.with_transparency(0x80);
        }
        let name = self
            .name
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Default".to_owned());
        AssStyle {
            name,
            font_name: self.font_name.unwrap_or_else(|| "Arial".to_owned()),
            font_size: self.font_size,
            primary_color: self.primary,
            secondary_color: self.secondary,
            outline_color: self.outline_color,
            back_color: self.back,
            // Для файловых стилей libass сводит жирность к 0/1 (`!!Bold`).
            bold: if self.bold != 0 {
                AssBold::Bold
            } else {
                AssBold::Normal
            },
            italic: self.italic != 0,
            underline: self.underline != 0,
            strike_out: self.strike_out != 0,
            scale_x: self.scale_x.max(0.0) / 100.0,
            scale_y: self.scale_y.max(0.0) / 100.0,
            spacing: self.spacing.max(0.0),
            angle: self.angle,
            border_style: AssBorderStyle::from_libass_value(self.border_style),
            outline: self.outline.max(0.0),
            shadow: self.shadow.max(0.0),
            alignment: self.alignment,
            margin_left: self.margin_left,
            margin_right: self.margin_right,
            margin_vertical: self.margin_vertical,
            encoding: self.encoding,
        }
    }
}

/// Разбирает значения строки `Style:` по раскладке. Лишние и недостающие значения не ошибка:
/// libass просто останавливается на более коротком из двух списков.
pub(super) fn parse_style(format: &[StyleField], flavor: StyleFlavor, values: &str) -> AssStyle {
    let mut draft = StyleDraft::blank();
    let mut cursor = FieldCursor::new(values);
    for field in format {
        let Some(token) = cursor.next_token(TrailingSpaces::Keep) else {
            break;
        };
        draft.apply(*field, flavor, token);
    }
    draft.finish(flavor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use subtitle_core::{AssHorizontalAlign, AssVerticalAlign};

    #[test]
    fn full_ass_style_is_parsed_with_libass_normalization() {
        let style = parse_style(
            ASS_STYLE_FORMAT,
            StyleFlavor::Ass,
            "*Sign,Noto Sans,24.5,&H00FF8040,&H000000FF,&H00000000,&H80000000,-1,0,1,0,80,120,1.5,0,3,2,-1,8,11,12,13,204",
        );
        assert_eq!(style.name, "Sign");
        assert_eq!(style.font_name, "Noto Sans");
        assert_eq!(style.font_size, 24.5);
        // &H00FF8040: BB=FF GG=80 RR=40 -> красный 0x40, зелёный 0x80, синий 0xFF.
        assert_eq!(style.primary_color, AssColor::new(0x40, 0x80, 0xff, 0));
        assert_eq!(style.back_color, AssColor::new(0, 0, 0, 0x80));
        assert_eq!(style.bold, AssBold::Bold);
        assert!(!style.italic);
        assert!(style.underline);
        assert!(!style.strike_out);
        assert_eq!((style.scale_x, style.scale_y), (0.8, 1.2));
        assert_eq!(style.spacing, 1.5);
        assert_eq!(style.border_style, AssBorderStyle::OpaqueBox);
        assert_eq!(style.outline, 2.0);
        // Отрицательная тень зажимается нулём.
        assert_eq!(style.shadow, 0.0);
        assert_eq!(
            style.alignment,
            AssAlignment {
                horizontal: AssHorizontalAlign::Center,
                vertical: AssVerticalAlign::Top
            }
        );
        assert_eq!(
            (style.margin_left, style.margin_right, style.margin_vertical),
            (11, 12, 13)
        );
        assert_eq!(style.encoding, 204);
    }

    #[test]
    fn ssa_style_uses_back_colour_for_outline_and_forces_shadow_alpha() {
        // Name,Fontname,Fontsize,Primary,Secondary,Tertiary,Back,Bold,Italic,BorderStyle,
        // Outline,Shadow,Alignment,MarginL,MarginR,MarginV,AlphaLevel,Encoding
        let style = parse_style(
            SSA_STYLE_FORMAT,
            StyleFlavor::Ssa,
            "Default,Arial,20,16777215,255,65280,255,-1,0,1,2,3,6,10,10,10,64,0",
        );
        // Tertiary игнорируется: контур = Back (255 -> красный), альфа = AlphaLevel (64).
        assert_eq!(style.outline_color, AssColor::new(255, 0, 0, 64));
        assert_eq!(style.primary_color.transparency, 64);
        assert_eq!(style.secondary_color.transparency, 64);
        assert_eq!(style.back_color, AssColor::new(255, 0, 0, 0x80));
        assert_eq!(style.bold, AssBold::Bold);
        // SSA-выравнивание 6 = верх по центру (старая схема).
        assert_eq!(
            style.alignment,
            AssAlignment {
                horizontal: AssHorizontalAlign::Center,
                vertical: AssVerticalAlign::Top
            }
        );
    }

    #[test]
    fn ssa_alignment_vsfilter_quirks() {
        let format = [StyleField::Alignment];
        let eight = parse_style(&format, StyleFlavor::Ssa, "8");
        assert_eq!(eight.alignment, AssAlignment::from_libass_bits(3));
        let four = parse_style(&format, StyleFlavor::Ssa, "4");
        assert_eq!(four.alignment, AssAlignment::from_libass_bits(11));
    }

    #[test]
    fn short_line_keeps_blank_defaults() {
        let style = parse_style(ASS_STYLE_FORMAT, StyleFlavor::Ass, "Only,Courier");
        assert_eq!(style.name, "Only");
        assert_eq!(style.font_name, "Courier");
        assert_eq!(style.font_size, 0.0);
        // Масштаб по умолчанию 100% = 1.0.
        assert_eq!((style.scale_x, style.scale_y), (1.0, 1.0));
        assert_eq!(style.primary_color.transparency, 0);
    }

    #[test]
    fn empty_name_and_missing_font_get_defaults() {
        let style = parse_style(&[StyleField::Name], StyleFlavor::Ass, "***");
        assert_eq!(style.name, "Default");
        assert_eq!(style.font_name, "Arial");
    }

    #[test]
    fn format_names_are_case_insensitive_and_unknown_ignored() {
        let fields = parse_style_format(" name, FONTNAME , Whatever, fontsize");
        assert_eq!(
            fields,
            vec![
                StyleField::Name,
                StyleField::FontName,
                StyleField::Ignored,
                StyleField::FontSize
            ]
        );
    }
}
