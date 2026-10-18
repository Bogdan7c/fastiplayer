//! Цвета TTML: `#rrggbb[aa]`, `rgb()`, `rgba()` и именованные цвета.
//!
//! Альфа в `rgba()` и в `#rrggbbaa` — число 0..=255 (так в TTML, в отличие от CSS).

use subtitle_core::SubtitleColor;

/// Разбирает значение цветового атрибута. `None` — запись не распознана.
pub(super) fn parse_color(text: &str) -> Option<SubtitleColor> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix('#') {
        return parse_hex(hex);
    }
    if let Some(arguments) = function_arguments(text, "rgba") {
        return parse_channels(arguments, ChannelCount::WithAlpha);
    }
    if let Some(arguments) = function_arguments(text, "rgb") {
        return parse_channels(arguments, ChannelCount::WithoutAlpha);
    }
    parse_named(text)
}

/// `#rrggbb` или `#rrggbbaa`.
fn parse_hex(digits: &str) -> Option<SubtitleColor> {
    if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |index: usize| {
        digits
            .get(index * 2..index * 2 + 2)
            .and_then(|pair| u8::from_str_radix(pair, 16).ok())
    };
    match digits.len() {
        6 => Some(SubtitleColor::opaque(channel(0)?, channel(1)?, channel(2)?)),
        8 => Some(SubtitleColor {
            red: channel(0)?,
            green: channel(1)?,
            blue: channel(2)?,
            alpha: channel(3)?,
        }),
        _ => None,
    }
}

/// Содержимое скобок `name( ... )`.
fn function_arguments<'text>(text: &'text str, name: &str) -> Option<&'text str> {
    let rest = text.strip_prefix(name)?.trim_start();
    rest.strip_prefix('(')?.strip_suffix(')')
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChannelCount {
    WithoutAlpha,
    WithAlpha,
}

/// `r, g, b[, a]` — целые 0..=255.
fn parse_channels(arguments: &str, count: ChannelCount) -> Option<SubtitleColor> {
    let values: Vec<u8> = arguments
        .split(',')
        .map(|part| part.trim().parse::<u8>().ok())
        .collect::<Option<_>>()?;
    match (count, values.as_slice()) {
        (ChannelCount::WithoutAlpha, [red, green, blue]) => {
            Some(SubtitleColor::opaque(*red, *green, *blue))
        }
        (ChannelCount::WithAlpha, [red, green, blue, alpha]) => Some(SubtitleColor {
            red: *red,
            green: *green,
            blue: *blue,
            alpha: *alpha,
        }),
        _ => None,
    }
}

/// Именованные цвета TTML (регистр не важен): `transparent` и 18 основных.
fn parse_named(name: &str) -> Option<SubtitleColor> {
    // `cyan` и `magenta` — синонимы `aqua` и `fuchsia`, которых нет в таблице ядра.
    match name.to_ascii_lowercase().as_str() {
        "cyan" => Some(SubtitleColor::opaque(0x00, 0xff, 0xff)),
        "magenta" => Some(SubtitleColor::opaque(0xff, 0x00, 0xff)),
        other => SubtitleColor::from_css_name(other).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_with_and_without_alpha() {
        assert_eq!(
            parse_color("#ff8001"),
            Some(SubtitleColor::opaque(255, 128, 1))
        );
        assert_eq!(
            parse_color("#ff800180"),
            Some(SubtitleColor {
                red: 255,
                green: 128,
                blue: 1,
                alpha: 0x80
            })
        );
        assert_eq!(parse_color("#fff"), None);
        assert_eq!(parse_color("#gg0000"), None);
    }

    #[test]
    fn rgb_and_rgba_functions() {
        assert_eq!(
            parse_color("rgb(1, 2,3)"),
            Some(SubtitleColor::opaque(1, 2, 3))
        );
        assert_eq!(
            parse_color("rgba(1,2,3,4)"),
            Some(SubtitleColor {
                red: 1,
                green: 2,
                blue: 3,
                alpha: 4
            })
        );
        assert_eq!(parse_color("rgb(1,2)"), None);
        assert_eq!(parse_color("rgb(1,2,300)"), None);
        assert_eq!(parse_color("rgba(1,2,3)"), None);
    }

    #[test]
    fn named_colors_including_ttml_synonyms() {
        assert_eq!(
            parse_color("Yellow"),
            Some(SubtitleColor::opaque(255, 255, 0))
        );
        assert_eq!(
            parse_color("cyan"),
            Some(SubtitleColor::opaque(0, 255, 255))
        );
        assert_eq!(
            parse_color("magenta"),
            Some(SubtitleColor::opaque(255, 0, 255))
        );
        assert_eq!(parse_color("transparent"), Some(SubtitleColor::TRANSPARENT));
        assert_eq!(parse_color("notacolor"), None);
    }
}
