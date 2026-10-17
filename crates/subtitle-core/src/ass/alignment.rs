// Порт поведения libass 0.17.5 (ISC): ass_utils.h (numpad2align), ass_parse.c (\an, \a).
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

//! Выравнивание ASS: перевод числовых форм libass в типизированную пару.

/// Горизонтальное выравнивание.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssHorizontalAlign {
    Left,
    Center,
    Right,
}

/// Вертикальное выравнивание.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssVerticalAlign {
    /// Низ кадра (в libass `VALIGN_SUB`).
    Bottom,
    /// Середина кадра (`VALIGN_CENTER`).
    Middle,
    /// Верх кадра (`VALIGN_TOP`).
    Top,
}

/// Выравнивание блока реплики.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssAlignment {
    pub horizontal: AssHorizontalAlign,
    pub vertical: AssVerticalAlign,
}

/// Биты внутреннего представления libass: младшие два — горизонталь (1..3).
const HALIGN_MASK: i32 = 3;
/// Бит «верх» во внутреннем представлении libass (`VALIGN_TOP`).
const VALIGN_TOP_BIT: i32 = 4;
/// Бит «середина» во внутреннем представлении libass (`VALIGN_CENTER`).
const VALIGN_CENTER_BIT: i32 = 8;

impl AssAlignment {
    /// Нижний центр: выравнивание по умолчанию (`\an2`).
    pub const BOTTOM_CENTER: Self = Self {
        horizontal: AssHorizontalAlign::Center,
        vertical: AssVerticalAlign::Bottom,
    };

    /// Из «внутреннего» числа libass (`VALIGN_* | HALIGN_*`), используемого
    /// стилями SSA и тегом `\a`.
    ///
    /// Значения, которых нормальный файл не даёт (горизонталь 0, оба вертикальных бита),
    /// libass на выходе трактует как «левый»/«низ» (ни одна ветка `switch` не срабатывает,
    /// сдвиг строки не применяется) — повторяем это.
    pub fn from_libass_bits(bits: i32) -> Self {
        let horizontal = match bits & HALIGN_MASK {
            2 => AssHorizontalAlign::Center,
            3 => AssHorizontalAlign::Right,
            _ => AssHorizontalAlign::Left,
        };
        let vertical = match bits & (VALIGN_TOP_BIT | VALIGN_CENTER_BIT) {
            VALIGN_TOP_BIT => AssVerticalAlign::Top,
            VALIGN_CENTER_BIT => AssVerticalAlign::Middle,
            _ => AssVerticalAlign::Bottom,
        };
        Self {
            horizontal,
            vertical,
        }
    }

    /// Из «цифровой клавиатуры» (`\an1..9`, стили ASS): точная копия `numpad2align`.
    pub fn from_numpad(value: i32) -> Self {
        Self::from_libass_bits(numpad_to_libass_bits(value))
    }

    /// Из аргумента тега `\a`: допустимы 1..=11, а «нелегальные» 4 и 8 libass
    /// трактует как 5 (причуда VSFilter). Остальное — `None` (взять стиль).
    pub fn from_legacy_tag(value: i32) -> Option<Self> {
        if !(1..=11).contains(&value) {
            return None;
        }
        let bits = if value & HALIGN_MASK == 0 { 5 } else { value };
        Some(Self::from_libass_bits(bits))
    }

    /// Из аргумента тега `\an`: допустимы 1..=9, остальное — `None` (взять стиль).
    pub fn from_numpad_tag(value: i32) -> Option<Self> {
        (1..=9).contains(&value).then(|| Self::from_numpad(value))
    }
}

/// `numpad2align` из libass (`ass_utils.h`) без изменений: возвращает внутренние биты.
pub fn numpad_to_libass_bits(value: i32) -> i32 {
    let value = if value < -i32::MAX {
        // libass выбирает «что-то из 1..3» для INT32_MIN.
        2
    } else if value < 0 {
        -value
    } else {
        value
    };
    // Остаток в C может быть отрицательным (для 0), поэтому `%` Rust даёт тот же результат.
    let horizontal = ((value - 1) % 3) + 1;
    let vertical_bits = if value <= 3 {
        0
    } else if value <= 6 {
        VALIGN_CENTER_BIT
    } else {
        VALIGN_TOP_BIT
    };
    horizontal | vertical_bits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numpad_grid_matches_keyboard_layout() {
        let expected = [
            (1, AssHorizontalAlign::Left, AssVerticalAlign::Bottom),
            (2, AssHorizontalAlign::Center, AssVerticalAlign::Bottom),
            (3, AssHorizontalAlign::Right, AssVerticalAlign::Bottom),
            (4, AssHorizontalAlign::Left, AssVerticalAlign::Middle),
            (5, AssHorizontalAlign::Center, AssVerticalAlign::Middle),
            (6, AssHorizontalAlign::Right, AssVerticalAlign::Middle),
            (7, AssHorizontalAlign::Left, AssVerticalAlign::Top),
            (8, AssHorizontalAlign::Center, AssVerticalAlign::Top),
            (9, AssHorizontalAlign::Right, AssVerticalAlign::Top),
        ];
        for (value, horizontal, vertical) in expected {
            assert_eq!(
                AssAlignment::from_numpad(value),
                AssAlignment {
                    horizontal,
                    vertical
                },
                "numpad {value}"
            );
        }
    }

    #[test]
    fn legacy_tag_maps_vsfilter_quirks() {
        // Старая схема: 1..3 — низ, 5..7 — верх, 9..11 — середина.
        assert_eq!(
            AssAlignment::from_legacy_tag(6),
            Some(AssAlignment {
                horizontal: AssHorizontalAlign::Center,
                vertical: AssVerticalAlign::Top
            })
        );
        assert_eq!(
            AssAlignment::from_legacy_tag(11),
            Some(AssAlignment {
                horizontal: AssHorizontalAlign::Right,
                vertical: AssVerticalAlign::Middle
            })
        );
        // Нелегальные \a4 и \a8 ведут себя как \a5 (верх слева).
        let like_five = AssAlignment::from_legacy_tag(5);
        assert_eq!(AssAlignment::from_legacy_tag(4), like_five);
        assert_eq!(AssAlignment::from_legacy_tag(8), like_five);
        assert_eq!(
            like_five.map(|align| align.vertical),
            Some(AssVerticalAlign::Top)
        );
        assert_eq!(AssAlignment::from_legacy_tag(0), None);
        assert_eq!(AssAlignment::from_legacy_tag(12), None);
    }

    #[test]
    fn numpad_tag_rejects_out_of_range() {
        assert_eq!(AssAlignment::from_numpad_tag(0), None);
        assert_eq!(AssAlignment::from_numpad_tag(10), None);
        assert_eq!(
            AssAlignment::from_numpad_tag(8),
            Some(AssAlignment {
                horizontal: AssHorizontalAlign::Center,
                vertical: AssVerticalAlign::Top
            })
        );
    }

    #[test]
    fn numpad_to_bits_handles_negative_and_extreme_values() {
        assert_eq!(numpad_to_libass_bits(-8), numpad_to_libass_bits(8));
        assert_eq!(numpad_to_libass_bits(i32::MIN), numpad_to_libass_bits(2));
        // 0: остаток C отрицательный, горизонталь 0, низ (как в libass).
        assert_eq!(numpad_to_libass_bits(0), 0);
    }
}
