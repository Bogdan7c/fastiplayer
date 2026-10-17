// Порт поведения libass 0.17.5 (ISC): ass.c (ass_lazy_track_init).
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

//! Параметры `[Script Info]`, разрешённые по правилам libass.

/// Разрешение сценария (`PlayResX/Y`): всегда положительное, умолчания уже применены.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssPlayRes {
    pub width: i32,
    pub height: i32,
}

impl AssPlayRes {
    /// Применяет `ass_lazy_track_init`: заданные в файле значения (`<= 0` или `0` —
    /// «не задано») превращаются в положительное разрешение.
    ///
    /// - обе стороны не заданы: 384x288;
    /// - задана только X: Y = 1024 при X = 1280, иначе `max(1, (X-1) - (X-1)/4)`;
    /// - задана только Y: X = 1280 при Y = 1024, иначе `Y + Y/3`.
    pub fn resolve(declared_width: i32, declared_height: i32) -> Self {
        if declared_width > 0 && declared_height > 0 {
            return Self {
                width: declared_width,
                height: declared_height,
            };
        }
        if declared_width <= 0 && declared_height <= 0 {
            return Self {
                width: 384,
                height: 288,
            };
        }
        if declared_height <= 0 {
            // Арифметика в u32 — как в libass (защита от переполнения).
            let height = if declared_width == 1280 {
                1024
            } else {
                let base = u32::try_from(declared_width.saturating_sub(1)).unwrap_or(0);
                let scaled = base - base / 4;
                i32::try_from(scaled).unwrap_or(i32::MAX).max(1)
            };
            return Self {
                width: declared_width,
                height,
            };
        }
        let width = if declared_height == 1024 {
            1280
        } else {
            let base = u32::try_from(declared_height).unwrap_or(0);
            let widened = base + base / 3;
            i32::try_from(widened).unwrap_or(i32::MAX)
        };
        Self {
            width,
            height: declared_height,
        }
    }
}

/// `LayoutResX/Y`: размер «раскладки», задаётся только когда обе стороны положительны.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssLayoutRes {
    pub width: i32,
    pub height: i32,
}

/// Масштабировать ли контур и тень вместе с `PlayRes` (`ScaledBorderAndShadow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssScaledBorderAndShadow {
    /// Контур/тень в пикселях раскладки (`LayoutRes`/кадр), не зависят от `PlayRes`.
    Disabled,
    /// Контур/тень растут вместе с `PlayRes`.
    Enabled,
}

/// Режим переноса строк (`WrapStyle`, тег `\q`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssWrapStyle {
    /// 0: умный перенос.
    Smart,
    /// 1: перенос только по `\N`.
    EndOfLine,
    /// 2: без переноса; `\n` работает как перевод строки.
    NoWrap,
    /// 3: умный перенос, нижняя строка шире.
    SmartWideLower,
}

impl AssWrapStyle {
    /// Из числа libass. libass различает лишь `== 1` и `== 2`, поэтому любое значение вне
    /// 0..=3 ведёт себя как `Smart`.
    pub const fn from_libass_value(value: i32) -> Self {
        match value {
            1 => Self::EndOfLine,
            2 => Self::NoWrap,
            3 => Self::SmartWideLower,
            _ => Self::Smart,
        }
    }

    /// Допустимое значение аргумента `\q` (0..=3), иначе `None`.
    pub fn from_tag_value(value: i32) -> Option<Self> {
        (0..=3)
            .contains(&value)
            .then(|| Self::from_libass_value(value))
    }
}

/// Кернинг пар (`Kerning: yes`): включает OpenType-признак `kern` при шейпинге ASS-реплик.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssKerning {
    Disabled,
    Enabled,
}

/// Матрица `YCbCr Matrix` (хранится для будущей цветокоррекции, на слое 1 не применяется).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssYCbCrMatrix {
    /// Строка пуста или заголовка нет.
    Default,
    /// Значение не распознано.
    Unknown,
    None,
    Bt601Tv,
    Bt601Pc,
    Bt709Tv,
    Bt709Pc,
    Smpte240mTv,
    Smpte240mPc,
    FccTv,
    FccPc,
}

/// Разобранный `[Script Info]`.
#[derive(Debug, Clone, PartialEq)]
pub struct AssScriptInfo {
    pub play_res: AssPlayRes,
    pub layout_res: Option<AssLayoutRes>,
    pub scaled_border_and_shadow: AssScaledBorderAndShadow,
    pub wrap_style: AssWrapStyle,
    pub kerning: AssKerning,
    pub ycbcr_matrix: AssYCbCrMatrix,
    /// Первые два символа `Language:` (как `strndup(p, 2)` в libass).
    pub language: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(width: i32, height: i32) -> (i32, i32) {
        let res = AssPlayRes::resolve(width, height);
        (res.width, res.height)
    }

    #[test]
    fn both_declared_are_kept() {
        assert_eq!(resolved(1920, 1080), (1920, 1080));
    }

    #[test]
    fn neither_declared_gives_384x288() {
        assert_eq!(resolved(0, 0), (384, 288));
        assert_eq!(resolved(-5, 0), (384, 288));
    }

    #[test]
    fn only_width_declared() {
        assert_eq!(resolved(1280, 0), (1280, 1024));
        assert_eq!(resolved(640, 0), (640, 480));
        // (X-1) - (X-1)/4 для X = 1: 0 -> не меньше 1.
        assert_eq!(resolved(1, 0), (1, 1));
        assert_eq!(resolved(1920, -1), (1920, 1440));
    }

    #[test]
    fn only_height_declared() {
        assert_eq!(resolved(0, 1024), (1280, 1024));
        assert_eq!(resolved(0, 480), (640, 480));
        assert_eq!(resolved(0, 1080), (1440, 1080));
    }

    #[test]
    fn wrap_style_mapping() {
        assert_eq!(AssWrapStyle::from_libass_value(0), AssWrapStyle::Smart);
        assert_eq!(AssWrapStyle::from_libass_value(1), AssWrapStyle::EndOfLine);
        assert_eq!(AssWrapStyle::from_libass_value(2), AssWrapStyle::NoWrap);
        assert_eq!(
            AssWrapStyle::from_libass_value(3),
            AssWrapStyle::SmartWideLower
        );
        assert_eq!(AssWrapStyle::from_libass_value(9), AssWrapStyle::Smart);
        assert_eq!(AssWrapStyle::from_tag_value(4), None);
        assert_eq!(AssWrapStyle::from_tag_value(-1), None);
        assert_eq!(AssWrapStyle::from_tag_value(2), Some(AssWrapStyle::NoWrap));
    }
}
