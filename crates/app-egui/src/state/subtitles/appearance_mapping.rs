//! Перевод пользовательского стиля из `SubtitlesConfig` в `SubtitleAppearance` растеризатора.
//!
//! Правила (решение владельца, сессия 04):
//! - основа: `decoration = box` -> `youtube_like()`, `outline` -> `outline_and_shadow()`;
//! - `text_size_percent` умножает `PRESET_FONT_SIZE` (100 % = пресет);
//! - `text_color` задаёт цвет текста; для плашки `background_color` + `background_opacity_percent`.
//!
//! Конфиг уже проверен `validate()`, но этот код не полагается на это вслепую: непонятное
//! значение не роняет приложение, а заменяется значением пресета с записью в лог.

use fastiplayer_config::{SubtitleDecorationPreference, SubtitlesConfig};
use subtitle_core::SubtitleColor;
use subtitle_raster::{FontSizeFraction, SubtitleAppearance, TextDecoration};

/// Максимум канала альфы и процентов для пересчёта «проценты -> 0..=255».
const ALPHA_MAX: u32 = 255;
const PERCENT_MAX: u32 = 100;

/// Собирает стиль растеризатора из секции конфига.
pub(super) fn appearance_from_config(config: &SubtitlesConfig) -> SubtitleAppearance {
    let mut appearance = match config.decoration {
        SubtitleDecorationPreference::Box => SubtitleAppearance::youtube_like(),
        SubtitleDecorationPreference::Outline => SubtitleAppearance::outline_and_shadow(),
    };

    appearance.font_size = scaled_font_size(config.text_size_percent, appearance.font_size);
    if let Some(text_color) = parse_color(&config.text_color, "text_color") {
        appearance.text_color = text_color;
    }
    if let TextDecoration::BackgroundBox(plate) = &mut appearance.decoration
        && let Some(base_color) = parse_color(&config.background_color, "background_color")
    {
        plate.color = SubtitleColor {
            alpha: alpha_from_percent(config.background_opacity_percent),
            ..base_color
        };
    }
    appearance
}

/// Размер шрифта пресета, умноженный на процент; вне допустимого диапазона — пресет.
fn scaled_font_size(percent: u16, preset: FontSizeFraction) -> FontSizeFraction {
    // Множитель считается первым: при 100 % он ровно 1.0, и размер пресета остаётся побитово тем же.
    let scale = f32::from(percent) / 100.0;
    let scaled = SubtitleAppearance::PRESET_FONT_SIZE * scale;
    FontSizeFraction::new(scaled).unwrap_or_else(|error| {
        tracing::warn!(%error, "Subtitle text size is out of range; using the default size");
        preset
    })
}

/// Разбирает `#RRGGBB`; ошибка пишется в лог, цвет остаётся прежним (пресета).
fn parse_color(text: &str, setting_name: &str) -> Option<SubtitleColor> {
    match SubtitleColor::from_hex_rgb_str(text) {
        Ok(color) => Some(color),
        Err(error) => {
            tracing::warn!(%error, setting_name, "Subtitle color is invalid; using the default");
            None
        }
    }
}

/// Проценты непрозрачности -> альфа 0..=255 с округлением (75 % -> 191, как в `youtube_like`).
fn alpha_from_percent(percent: u8) -> u8 {
    let percent = u32::from(percent).min(PERCENT_MAX);
    let alpha = (percent * ALPHA_MAX + PERCENT_MAX / 2) / PERCENT_MAX;
    u8::try_from(alpha).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_reproduces_the_youtube_like_preset_exactly() {
        assert_eq!(
            appearance_from_config(&SubtitlesConfig::default()),
            SubtitleAppearance::youtube_like()
        );
    }

    #[test]
    fn outline_decoration_starts_from_the_outline_preset() {
        let config = SubtitlesConfig {
            decoration: SubtitleDecorationPreference::Outline,
            ..SubtitlesConfig::default()
        };
        assert_eq!(
            appearance_from_config(&config),
            SubtitleAppearance::outline_and_shadow()
        );
    }

    #[test]
    fn size_color_and_box_settings_change_the_appearance() {
        let config = SubtitlesConfig {
            text_size_percent: 150,
            text_color: "#FFCC00".to_owned(),
            background_color: "#102030".to_owned(),
            background_opacity_percent: 50,
            ..SubtitlesConfig::default()
        };
        let appearance = appearance_from_config(&config);
        let expected_size = SubtitleAppearance::PRESET_FONT_SIZE * 1.5;
        assert!((appearance.font_size.get() - expected_size).abs() < 1e-6);
        assert_eq!(
            appearance.text_color,
            SubtitleColor::opaque(0xFF, 0xCC, 0x00)
        );
        let TextDecoration::BackgroundBox(plate) = appearance.decoration else {
            panic!("box decoration expected");
        };
        assert_eq!(
            plate.color,
            SubtitleColor {
                red: 0x10,
                green: 0x20,
                blue: 0x30,
                alpha: 128
            }
        );
    }

    #[test]
    fn invalid_color_falls_back_to_the_preset_instead_of_failing() {
        let config = SubtitlesConfig {
            text_color: "not-a-color".to_owned(),
            ..SubtitlesConfig::default()
        };
        assert_eq!(
            appearance_from_config(&config).text_color,
            SubtitleAppearance::youtube_like().text_color
        );
    }

    #[test]
    fn opacity_percent_maps_to_alpha_with_rounding() {
        assert_eq!(alpha_from_percent(0), 0);
        assert_eq!(alpha_from_percent(75), 191);
        assert_eq!(alpha_from_percent(100), 255);
    }
}
