//! Значения свойств CSS из поддерживаемого подмножества: цвета, насыщенность, наклон, декорации.
//!
//! Каждая функция возвращает `None`, если значение не поддерживается; вызывающий
//! считает такое объявление проигнорированным.

use subtitle_core::{FontSlant, FontWeight, SubtitleColor, TextStrikethrough, TextUnderline};

/// Числовой `font-weight` с этого порога считается жирным (как у CSS: bold = 700).
const BOLD_WEIGHT_THRESHOLD: u32 = 700;
/// Максимальное значение канала цвета и знаменатель для перевода долей альфа-канала.
const CHANNEL_MAX: f32 = 255.0;
/// Сколько компонентов у `rgb(...)` и `rgba(...)`.
const RGB_COMPONENTS: usize = 3;
const RGBA_COMPONENTS: usize = 4;

/// Цвет: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()/rgba()` (через запятую), базовые имена.
pub(super) fn parse_css_color(value: &str) -> Option<SubtitleColor> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    let lowered = value.to_ascii_lowercase();
    if let Some(arguments) = function_arguments(&lowered, "rgba") {
        return parse_rgb_arguments(arguments, RGBA_COMPONENTS);
    }
    if let Some(arguments) = function_arguments(&lowered, "rgb") {
        return parse_rgb_arguments(arguments, RGB_COMPONENTS);
    }
    SubtitleColor::from_css_name(value).ok()
}

/// `font-weight`: `bold`/`bolder`/`normal`/`lighter` или число (700+ — жирный).
pub(super) fn parse_font_weight(value: &str) -> Option<FontWeight> {
    match value.trim().to_ascii_lowercase().as_str() {
        "bold" | "bolder" => Some(FontWeight::Bold),
        "normal" | "lighter" => Some(FontWeight::Normal),
        numeric => numeric.parse::<u32>().ok().map(|weight| {
            if weight >= BOLD_WEIGHT_THRESHOLD {
                FontWeight::Bold
            } else {
                FontWeight::Normal
            }
        }),
    }
}

/// `font-style`: `italic`/`oblique` — наклон, `normal` — без наклона.
pub(super) fn parse_font_style(value: &str) -> Option<FontSlant> {
    match value.trim().to_ascii_lowercase().as_str() {
        "italic" | "oblique" => Some(FontSlant::Italic),
        "normal" => Some(FontSlant::Normal),
        _ => None,
    }
}

/// Результат разбора `text-decoration`: какие линии заданы (незаданные остаются `None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct ParsedTextDecoration {
    pub(super) underline: Option<TextUnderline>,
    pub(super) strikethrough: Option<TextStrikethrough>,
}

/// `text-decoration`: слова `underline`, `line-through`, `none`. Любое другое слово
/// (`overline`, цвет, стиль линии) делает значение неподдерживаемым целиком.
pub(super) fn parse_text_decoration(value: &str) -> Option<ParsedTextDecoration> {
    let mut parsed = ParsedTextDecoration::default();
    let mut any_word = false;
    for word in value.split_whitespace() {
        any_word = true;
        match word.to_ascii_lowercase().as_str() {
            "underline" => parsed.underline = Some(TextUnderline::Single),
            "line-through" => parsed.strikethrough = Some(TextStrikethrough::Single),
            "none" => {
                parsed.underline = Some(TextUnderline::None);
                parsed.strikethrough = Some(TextStrikethrough::None);
            }
            _ => return None,
        }
    }
    any_word.then_some(parsed)
}

/// Содержимое скобок `name(...)` без самих скобок.
fn function_arguments<'text>(value: &'text str, name: &str) -> Option<&'text str> {
    value
        .strip_prefix(name)?
        .trim_start()
        .strip_prefix('(')?
        .strip_suffix(')')
}

fn parse_hex_color(hex: &str) -> Option<SubtitleColor> {
    match hex.len() {
        // С альфа-каналом: цветовую часть разбирает общий парсер, альфа — отдельно.
        4 => {
            let (rgb, alpha) = hex.split_at(3);
            let base = SubtitleColor::from_hex_rgb_str(rgb).ok()?;
            let alpha = u8::from_str_radix(alpha, 16).ok()?.checked_mul(17)?;
            Some(SubtitleColor { alpha, ..base })
        }
        8 => {
            let (rgb, alpha) = hex.split_at(6);
            let base = SubtitleColor::from_hex_rgb_str(rgb).ok()?;
            let alpha = u8::from_str_radix(alpha, 16).ok()?;
            Some(SubtitleColor { alpha, ..base })
        }
        _ => SubtitleColor::from_hex_rgb_str(hex).ok(),
    }
}

/// Аргументы `rgb(r,g,b)` / `rgba(r,g,b,a)`: каналы — числа 0..255 (с зажимом), альфа — 0..1 или N%.
fn parse_rgb_arguments(arguments: &str, expected_components: usize) -> Option<SubtitleColor> {
    let components: Vec<&str> = arguments.split(',').map(str::trim).collect();
    if components.len() != expected_components {
        return None;
    }
    let mut channels = [0_u8; RGB_COMPONENTS];
    for (channel, text) in channels.iter_mut().zip(&components) {
        *channel = parse_channel(text)?;
    }
    let alpha = match components.get(RGB_COMPONENTS) {
        Some(text) => parse_alpha(text)?,
        None => u8::MAX,
    };
    let [red, green, blue] = channels;
    Some(SubtitleColor {
        red,
        green,
        blue,
        alpha,
    })
}

/// Канал `rgb`: число 0..255, значения вне диапазона зажимаются (как в CSS).
fn parse_channel(text: &str) -> Option<u8> {
    float_to_byte(text.parse().ok()?)
}

/// Альфа: доля 0..1 или процент; вне диапазона зажимается.
fn parse_alpha(text: &str) -> Option<u8> {
    let fraction = match text.strip_suffix('%') {
        Some(percent) => percent.parse::<f32>().ok()? / 100.0,
        None => text.parse::<f32>().ok()?,
    };
    float_to_byte(fraction.clamp(0.0, 1.0) * CHANNEL_MAX)
}

/// Округляет число в диапазон байта с зажимом; NaN отвергается.
fn float_to_byte(value: f32) -> Option<u8> {
    if value.is_nan() {
        return None;
    }
    // После зажима значение лежит в 0..=255, поэтому приведение ничего не обрезает.
    u8::try_from(value.clamp(0.0, CHANNEL_MAX).round() as i64).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_forms_including_alpha() {
        assert_eq!(
            parse_css_color("#f00"),
            Some(SubtitleColor::opaque(255, 0, 0))
        );
        assert_eq!(
            parse_css_color("#00ff0080"),
            Some(SubtitleColor {
                red: 0,
                green: 255,
                blue: 0,
                alpha: 128
            })
        );
        assert_eq!(
            parse_css_color("#0f08"),
            Some(SubtitleColor {
                red: 0,
                green: 255,
                blue: 0,
                alpha: 136
            })
        );
        assert_eq!(parse_css_color("#12345"), None);
    }

    #[test]
    fn rgb_and_rgba_functions() {
        assert_eq!(
            parse_css_color("rgb(10, 20, 30)"),
            Some(SubtitleColor::opaque(10, 20, 30))
        );
        assert_eq!(
            parse_css_color("RGBA(0,0,0,0.5)"),
            Some(SubtitleColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 128
            })
        );
        assert_eq!(
            parse_css_color("rgba(0,0,0,50%)"),
            Some(SubtitleColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 128
            })
        );
        assert_eq!(
            parse_css_color("rgba(0,0,0,1)"),
            Some(SubtitleColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255
            })
        );
        assert_eq!(
            parse_css_color("rgb(300,-5,0)"),
            Some(SubtitleColor::opaque(255, 0, 0))
        );
        assert_eq!(parse_css_color("rgb(1,2)"), None);
        assert_eq!(parse_css_color("rgba(1,2,3)"), None);
    }

    #[test]
    fn named_colors_and_unknowns() {
        assert_eq!(
            parse_css_color(" Yellow "),
            Some(SubtitleColor::opaque(255, 255, 0))
        );
        assert_eq!(
            parse_css_color("transparent"),
            Some(SubtitleColor::TRANSPARENT)
        );
        assert_eq!(parse_css_color("papayawhip"), None);
        assert_eq!(parse_css_color("currentcolor"), None);
    }

    #[test]
    fn font_weight_and_style_values() {
        assert_eq!(parse_font_weight("bold"), Some(FontWeight::Bold));
        assert_eq!(parse_font_weight("700"), Some(FontWeight::Bold));
        assert_eq!(parse_font_weight("400"), Some(FontWeight::Normal));
        assert_eq!(parse_font_weight("heavy"), None);
        assert_eq!(parse_font_style("oblique"), Some(FontSlant::Italic));
        assert_eq!(parse_font_style("normal"), Some(FontSlant::Normal));
        assert_eq!(parse_font_style("inherit"), None);
    }

    #[test]
    fn text_decoration_combinations() {
        let both = parse_text_decoration("underline line-through").unwrap();
        assert_eq!(both.underline, Some(TextUnderline::Single));
        assert_eq!(both.strikethrough, Some(TextStrikethrough::Single));
        let reset = parse_text_decoration("none").unwrap();
        assert_eq!(reset.underline, Some(TextUnderline::None));
        assert_eq!(parse_text_decoration("overline"), None);
        assert_eq!(parse_text_decoration("underline wavy"), None);
        assert_eq!(parse_text_decoration("  "), None);
    }
}
