//! Цвет авторской стилизации субтитров.

/// Некорректная запись цвета.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid subtitle color `{text}`")]
pub struct InvalidSubtitleColor {
    text: String,
}

impl InvalidSubtitleColor {
    fn new(text: &str) -> Self {
        Self {
            text: text.to_owned(),
        }
    }

    /// Исходный текст, который не удалось разобрать.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// RGBA-цвет, 8 бит на канал (не премультиплицированный).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubtitleColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

/// Базовые 16 HTML-цветов (CSS Color Level 1: без orange).
const BASIC_CSS_COLORS: [(&str, [u8; 3]); 16] = [
    ("black", [0x00, 0x00, 0x00]),
    ("silver", [0xc0, 0xc0, 0xc0]),
    ("gray", [0x80, 0x80, 0x80]),
    ("white", [0xff, 0xff, 0xff]),
    ("maroon", [0x80, 0x00, 0x00]),
    ("red", [0xff, 0x00, 0x00]),
    ("purple", [0x80, 0x00, 0x80]),
    ("fuchsia", [0xff, 0x00, 0xff]),
    ("green", [0x00, 0x80, 0x00]),
    ("lime", [0x00, 0xff, 0x00]),
    ("olive", [0x80, 0x80, 0x00]),
    ("yellow", [0xff, 0xff, 0x00]),
    ("navy", [0x00, 0x00, 0x80]),
    ("blue", [0x00, 0x00, 0xff]),
    ("teal", [0x00, 0x80, 0x80]),
    ("aqua", [0x00, 0xff, 0xff]),
];

impl SubtitleColor {
    /// Полностью непрозрачный цвет.
    pub const fn opaque(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha: u8::MAX,
        }
    }

    /// Полностью прозрачный цвет (CSS `transparent`).
    pub const TRANSPARENT: Self = Self {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 0,
    };

    /// Разбирает `#rrggbb`, `rrggbb`, `#rgb` или `rgb` (решётка необязательна).
    pub fn from_hex_rgb_str(text: &str) -> Result<Self, InvalidSubtitleColor> {
        let digits = text.strip_prefix('#').unwrap_or(text);
        let invalid = || InvalidSubtitleColor::new(text);
        // Только ASCII hex: иначе нарезка по байтам ниже могла бы попасть внутрь UTF-8 символа.
        if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        match digits.len() {
            6 => {
                let channel = |range: std::ops::Range<usize>| {
                    digits
                        .get(range)
                        .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                        .ok_or_else(invalid)
                };
                Ok(Self::opaque(channel(0..2)?, channel(2..4)?, channel(4..6)?))
            }
            3 => {
                // Короткая форма: каждая цифра удваивается (#f00 == #ff0000).
                let channel = |index: usize| {
                    digits
                        .get(index..=index)
                        .and_then(|digit| u8::from_str_radix(digit, 16).ok())
                        .map(|value| value * 17)
                        .ok_or_else(invalid)
                };
                Ok(Self::opaque(channel(0)?, channel(1)?, channel(2)?))
            }
            _ => Err(invalid()),
        }
    }

    /// Находит один из базовых 16 HTML-цветов или `transparent` (без учёта регистра).
    pub fn from_css_name(name: &str) -> Result<Self, InvalidSubtitleColor> {
        let lowered = name.trim().to_ascii_lowercase();
        if lowered == "transparent" {
            return Ok(Self::TRANSPARENT);
        }
        BASIC_CSS_COLORS
            .iter()
            .find(|(candidate, _)| *candidate == lowered)
            .map(|(_, [red, green, blue])| Self::opaque(*red, *green, *blue))
            .ok_or_else(|| InvalidSubtitleColor::new(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_hex_with_and_without_hash() {
        let expected = SubtitleColor::opaque(0xff, 0x80, 0x01);
        assert_eq!(SubtitleColor::from_hex_rgb_str("#ff8001"), Ok(expected));
        assert_eq!(SubtitleColor::from_hex_rgb_str("FF8001"), Ok(expected));
    }

    #[test]
    fn short_hex_doubles_digits() {
        assert_eq!(
            SubtitleColor::from_hex_rgb_str("#f00"),
            Ok(SubtitleColor::opaque(0xff, 0, 0))
        );
        assert_eq!(
            SubtitleColor::from_hex_rgb_str("1a3"),
            Ok(SubtitleColor::opaque(0x11, 0xaa, 0x33))
        );
    }

    #[test]
    fn malformed_hex_is_rejected() {
        for text in ["", "#", "#ff", "#ffff", "#gggggg", "#ff00000", "#ёёё"] {
            let error = SubtitleColor::from_hex_rgb_str(text).expect_err(text);
            assert_eq!(error.text(), text);
        }
    }

    #[test]
    fn css_names_are_case_insensitive() {
        assert_eq!(
            SubtitleColor::from_css_name("Red"),
            Ok(SubtitleColor::opaque(0xff, 0, 0))
        );
        assert_eq!(
            SubtitleColor::from_css_name(" LIME "),
            Ok(SubtitleColor::opaque(0, 0xff, 0))
        );
        assert_eq!(
            SubtitleColor::from_css_name("transparent"),
            Ok(SubtitleColor::TRANSPARENT)
        );
    }

    #[test]
    fn unknown_css_name_is_rejected() {
        assert!(SubtitleColor::from_css_name("rebeccapurple").is_err());
    }

    #[test]
    fn all_sixteen_basic_colors_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for (name, _) in BASIC_CSS_COLORS {
            let color = SubtitleColor::from_css_name(name).expect(name);
            assert!(seen.insert(color), "duplicate color for {name}");
        }
        assert_eq!(seen.len(), 16);
    }
}
