// Порт поведения libass 0.17.5 (ISC): ass.c (set_default_style), ass_parse.c (ass_update_font).
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

//! Стиль ASS (`Style:` из `[V4+ Styles]` / `[V4 Styles]`) со всеми полями libass.

use super::alignment::AssAlignment;
use super::color::AssColor;

/// Жирность: форма хранения libass (`0` — нет, `1` — да, `>= 100` — точный вес).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssBold {
    Normal,
    Bold,
    /// Точный вес шрифта (CSS-подобные 100..900), как `\b700`.
    Weight(i32),
}

impl AssBold {
    /// Из числа стиля или тега `\b`: `0` — обычный, `1` и `-1` — жирный, прочее `>= 100`
    /// — точный вес. Остальные значения libass для стиля трактует как обычный шрифт
    /// (`ass_update_font`: `val <= 0` -> 400).
    pub const fn from_libass_value(value: i32) -> Self {
        match value {
            1 | -1 => Self::Bold,
            value if value <= 0 => Self::Normal,
            // 2..=99: libass всё равно передаёт значение как точный вес шрифта.
            value => Self::Weight(value),
        }
    }

    /// Вес для подбора шрифта: 400, 700 или точное значение (`ass_update_font`).
    pub const fn font_weight(self) -> i32 {
        match self {
            Self::Normal => 400,
            Self::Bold => 700,
            Self::Weight(weight) => weight,
        }
    }
}

/// Стиль рамки. libass различает только 3 и 4; всё остальное рисуется как контур.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssBorderStyle {
    /// `BorderStyle: 1` и любые значения, кроме 3 и 4: контур и тень.
    OutlineAndShadow,
    /// `BorderStyle: 3`: непрозрачная плашка за текстом.
    OpaqueBox,
    /// `BorderStyle: 4`: режим libass, при котором контур не растеризуется отдельно
    /// (`ass_render.c`, проверки `border_style == 4`).
    Style4,
}

impl AssBorderStyle {
    pub const fn from_libass_value(value: i32) -> Self {
        match value {
            3 => Self::OpaqueBox,
            4 => Self::Style4,
            _ => Self::OutlineAndShadow,
        }
    }
}

/// Один стиль. Поля открыты: это нейтральные данные, а нормализацию делает парсер
/// (как `process_style` в libass).
#[derive(Debug, Clone, PartialEq)]
pub struct AssStyle {
    pub name: String,
    pub font_name: String,
    /// Размер шрифта в единицах PlayRes (может быть дробным).
    pub font_size: f64,
    pub primary_color: AssColor,
    pub secondary_color: AssColor,
    /// Цвет контура (в SSA — тот же, что `back_color`).
    pub outline_color: AssColor,
    /// Цвет тени / плашки.
    pub back_color: AssColor,
    pub bold: AssBold,
    pub italic: bool,
    pub underline: bool,
    pub strike_out: bool,
    /// Масштаб по X как доля (`ScaleX / 100`, не меньше 0).
    pub scale_x: f64,
    /// Масштаб по Y как доля (`ScaleY / 100`, не меньше 0).
    pub scale_y: f64,
    /// Межбуквенный интервал (не меньше 0, как в `process_style`).
    pub spacing: f64,
    /// Угол поворота; libass хранит, но на слое 1 не использует.
    pub angle: f64,
    pub border_style: AssBorderStyle,
    /// Толщина контура (не меньше 0).
    pub outline: f64,
    /// Смещение тени (не меньше 0).
    pub shadow: f64,
    pub alignment: AssAlignment,
    pub margin_left: i32,
    pub margin_right: i32,
    pub margin_vertical: i32,
    /// Кодировка шрифта (`\fe`, поле `Encoding`); хранится как есть.
    pub encoding: i32,
}

impl AssStyle {
    /// Встроенный стиль «Default» libass (`set_default_style`): им пользуются реплики,
    /// чей стиль не найден, и сценарии без собственного `Default`.
    ///
    /// Значения повторяют VSFilter: Arial 18, белый основной цвет, чёрный контур,
    /// полупрозрачная тень (`BackColour = 0x00000080` -> прозрачность 0x80), жирность 200,
    /// контур 2, тень 3, нижний центр, поля 20.
    pub fn libass_default() -> Self {
        Self {
            name: "Default".to_owned(),
            font_name: "Arial".to_owned(),
            font_size: 18.0,
            primary_color: AssColor::new(0xff, 0xff, 0xff, 0),
            secondary_color: AssColor::new(0xff, 0xff, 0x00, 0),
            outline_color: AssColor::new(0, 0, 0, 0),
            back_color: AssColor::new(0, 0, 0, 0x80),
            bold: AssBold::Weight(200),
            italic: false,
            underline: false,
            strike_out: false,
            scale_x: 1.0,
            scale_y: 1.0,
            spacing: 0.0,
            angle: 0.0,
            border_style: AssBorderStyle::OutlineAndShadow,
            outline: 2.0,
            shadow: 3.0,
            alignment: AssAlignment::BOTTOM_CENTER,
            margin_left: 20,
            margin_right: 20,
            margin_vertical: 20,
            encoding: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_values_follow_update_font() {
        assert_eq!(AssBold::from_libass_value(0), AssBold::Normal);
        assert_eq!(AssBold::from_libass_value(1), AssBold::Bold);
        assert_eq!(AssBold::from_libass_value(-1), AssBold::Bold);
        assert_eq!(AssBold::from_libass_value(-5), AssBold::Normal);
        assert_eq!(AssBold::from_libass_value(900), AssBold::Weight(900));
        assert_eq!(AssBold::Normal.font_weight(), 400);
        assert_eq!(AssBold::Bold.font_weight(), 700);
        assert_eq!(AssBold::Weight(300).font_weight(), 300);
    }

    #[test]
    fn border_style_only_three_and_four_are_special() {
        assert_eq!(
            AssBorderStyle::from_libass_value(1),
            AssBorderStyle::OutlineAndShadow
        );
        assert_eq!(
            AssBorderStyle::from_libass_value(0),
            AssBorderStyle::OutlineAndShadow
        );
        assert_eq!(
            AssBorderStyle::from_libass_value(3),
            AssBorderStyle::OpaqueBox
        );
        assert_eq!(AssBorderStyle::from_libass_value(4), AssBorderStyle::Style4);
    }

    #[test]
    fn builtin_default_matches_libass_defaults() {
        let style = AssStyle::libass_default();
        assert_eq!(style.name, "Default");
        assert_eq!(style.font_name, "Arial");
        assert_eq!(style.font_size, 18.0);
        assert_eq!(style.back_color.opacity(), 0x7f);
        assert_eq!(style.alignment, AssAlignment::BOTTOM_CENTER);
        assert_eq!(
            (style.margin_left, style.margin_right, style.margin_vertical),
            (20, 20, 20)
        );
    }
}
