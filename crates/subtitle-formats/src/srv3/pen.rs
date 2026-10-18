//! Перо srv3 (`<pen>`): стиль текста, на который ссылаются слова.

use subtitle_core::{FontSlant, FontWeight, SpanStyle, SubtitleColor, TextUnderline};

/// Непрозрачность в srv3 идёт 0..=254 (254 — «полностью видно»), в модели — 0..=255.
const SRV3_OPAQUE_LEVEL: u32 = 254;

/// Размер шрифта в процентах, при котором `sz` ничего не меняет.
const DEFAULT_SIZE_PERCENT: u32 = 100;

/// Разобранное перо. Хранит только то, что модель умеет показывать.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Pen {
    bold: bool,
    italic: bool,
    underline: bool,
    /// Цвет текста `fc` вместе с непрозрачностью `fo`; `None` — цвет пользовательского стиля.
    color: Option<SubtitleColor>,
    /// Фон `bc` с непрозрачностью `bo`; задан только при `bo > 0`.
    background: Option<SubtitleColor>,
}

/// Перо и признак «в нём были атрибуты, которые мы не показываем».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ParsedPen {
    pub(super) pen: Pen,
    pub(super) has_ignored_attributes: bool,
}

impl Pen {
    /// Собирает перо из пар «имя атрибута — значение» элемента `<pen>`.
    ///
    /// Неподдерживаемое (обводка `et/ec`, размер `sz` не 100%, стиль шрифта `fs`, ruby `rb`,
    /// `hg`, нечитаемые значения) не ломает перо: остальное применяется, а вызывающий код
    /// считает такое перо одним «проигнорированным правилом стиля».
    pub(super) fn from_attributes<'attr>(
        attributes: impl IntoIterator<Item = (&'attr str, &'attr str)>,
    ) -> ParsedPen {
        let mut pen = Self::default();
        let mut has_ignored_attributes = false;
        let mut color_rgb = None;
        let mut color_opacity = None;
        let mut background_rgb = None;
        let mut background_opacity = 0_u32;
        for (name, value) in attributes {
            match name {
                "id" => {}
                "b" => pen.bold = value == "1",
                "i" => pen.italic = value == "1",
                "u" => pen.underline = value == "1",
                "fc" => match parse_rgb(value) {
                    Some(rgb) => color_rgb = Some(rgb),
                    None => has_ignored_attributes = true,
                },
                "fo" => match value.parse::<u32>() {
                    Ok(level) => color_opacity = Some(level),
                    Err(_) => has_ignored_attributes = true,
                },
                "bc" => match parse_rgb(value) {
                    Some(rgb) => background_rgb = Some(rgb),
                    None => has_ignored_attributes = true,
                },
                "bo" => match value.parse::<u32>() {
                    Ok(level) => background_opacity = level,
                    Err(_) => has_ignored_attributes = true,
                },
                "sz" => {
                    if value.parse::<u32>().ok() != Some(DEFAULT_SIZE_PERCENT) {
                        has_ignored_attributes = true;
                    }
                }
                // Обводка/тень (`et`, `ec`), стиль шрифта (`fs`), ruby (`rb`), `hg`, прочее.
                _ => has_ignored_attributes = true,
            }
        }
        pen.color = color_rgb.map(|(red, green, blue)| SubtitleColor {
            red,
            green,
            blue,
            alpha: color_opacity.map_or(u8::MAX, opacity_to_alpha),
        });
        pen.background =
            background_rgb
                .filter(|_| background_opacity > 0)
                .map(|(red, green, blue)| SubtitleColor {
                    red,
                    green,
                    blue,
                    alpha: opacity_to_alpha(background_opacity),
                });
        ParsedPen {
            pen,
            has_ignored_attributes,
        }
    }

    /// Стиль спана модели. `None` у цветов остаётся «стилем пользователя».
    pub(super) fn span_style(&self) -> SpanStyle {
        SpanStyle {
            weight: if self.bold {
                FontWeight::Bold
            } else {
                FontWeight::Normal
            },
            slant: if self.italic {
                FontSlant::Italic
            } else {
                FontSlant::Normal
            },
            underline: if self.underline {
                TextUnderline::Single
            } else {
                TextUnderline::None
            },
            color: self.color,
            background: self.background,
            ..SpanStyle::default()
        }
    }
}

/// Непрозрачность srv3 (0..=254) -> альфа 0..=255.
fn opacity_to_alpha(level: u32) -> u8 {
    if level >= SRV3_OPAQUE_LEVEL {
        u8::MAX
    } else {
        u8::try_from(level).unwrap_or(u8::MAX)
    }
}

/// `#RRGGBB` -> компоненты; прочие записи цвета отвергаются.
fn parse_rgb(value: &str) -> Option<(u8, u8, u8)> {
    let color = SubtitleColor::from_hex_rgb_str(value).ok()?;
    Some((color.red, color.green, color.blue))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pen(attributes: &[(&str, &str)]) -> ParsedPen {
        Pen::from_attributes(attributes.iter().copied())
    }

    #[test]
    fn bold_italic_underline_and_colors_are_mapped() {
        let parsed = pen(&[
            ("id", "3"),
            ("b", "1"),
            ("i", "1"),
            ("u", "1"),
            ("fc", "#FFD93D"),
            ("fo", "254"),
            ("bc", "#000000"),
            ("bo", "128"),
        ]);
        let style = parsed.pen.span_style();
        assert_eq!(style.weight, FontWeight::Bold);
        assert_eq!(style.slant, FontSlant::Italic);
        assert_eq!(style.underline, TextUnderline::Single);
        assert_eq!(style.color, Some(SubtitleColor::opaque(0xFF, 0xD9, 0x3D)));
        assert_eq!(
            style.background,
            Some(SubtitleColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 128
            })
        );
        assert!(!parsed.has_ignored_attributes);
    }

    #[test]
    fn background_without_opacity_is_not_applied() {
        let style = pen(&[("bc", "#112233"), ("bo", "0")]).pen.span_style();
        assert_eq!(style.background, None);
    }

    #[test]
    fn unsupported_attributes_are_flagged_but_supported_ones_still_apply() {
        let parsed = pen(&[("b", "1"), ("et", "3"), ("ec", "#000000"), ("sz", "150")]);
        assert!(parsed.has_ignored_attributes);
        assert_eq!(parsed.pen.span_style().weight, FontWeight::Bold);
        assert!(!pen(&[("sz", "100")]).has_ignored_attributes);
    }

    #[test]
    fn invalid_color_is_ignored_and_flagged() {
        let parsed = pen(&[("fc", "red-ish")]);
        assert_eq!(parsed.pen.span_style().color, None);
        assert!(parsed.has_ignored_attributes);
    }

    #[test]
    fn zero_font_opacity_makes_the_color_transparent() {
        let style = pen(&[("fc", "#FFFFFF"), ("fo", "0")]).pen.span_style();
        assert_eq!(style.color.map(|color| color.alpha), Some(0));
    }
}
