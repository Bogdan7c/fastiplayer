//! Тесты стиля: перевод в растеризатор, нулевой сдвиг по умолчанию, запись обратно в конфиг.

use fastiplayer_config::AppConfig;
use fastiplayer_settings::app_config_registry;
use settings_core::SettingId;

use super::*;

/// Нестандартная секция конфига, у которой каждое поле стиля отличается от defaults.
fn custom_config() -> SubtitlesConfig {
    SubtitlesConfig {
        decoration: SubtitleDecorationPreference::Outline,
        text_size_percent: 150,
        text_color: "#FFCC00".to_owned(),
        background_color: "#102030".to_owned(),
        background_opacity_percent: 50,
        position_horizontal_percent: 20,
        position_vertical_percent: -15,
        ass_force_user_style: true,
        ..SubtitlesConfig::default()
    }
}

#[test]
fn default_config_reproduces_the_youtube_like_preset_exactly() {
    assert_eq!(
        UserSubtitleStyle::default().to_appearance(),
        SubtitleAppearance::youtube_like()
    );
}

#[test]
fn default_config_gives_zero_offset_because_it_equals_the_raster_preset_margin() {
    // Ключевая связка двух чисел: 6 % в конфиге и PRESET_BOTTOM_MARGIN в растеризаторе.
    let style = UserSubtitleStyle::from_config(&SubtitlesConfig::default());
    assert_eq!(style.vertical_offset_fraction(), 0.0);
    assert_eq!(style.horizontal_offset_fraction(), 0.0);
    assert_eq!(
        f32::from(DEFAULT_POSITION_VERTICAL_PERCENT) / 100.0,
        SubtitleAppearance::PRESET_BOTTOM_MARGIN
    );
}

#[test]
fn offsets_follow_the_chosen_position() {
    let style = UserSubtitleStyle::from_config(&custom_config());
    // -15 % против 6 % пресета: на 21 % высоты ниже стандартного места.
    assert!((style.vertical_offset_fraction() - (-0.21)).abs() < 1e-6);
    // 20 % против середины: на 30 % ширины левее.
    assert!((style.horizontal_offset_fraction() - (-0.30)).abs() < 1e-6);
}

#[test]
fn outline_decoration_starts_from_the_outline_preset() {
    let config = SubtitlesConfig {
        decoration: SubtitleDecorationPreference::Outline,
        ..SubtitlesConfig::default()
    };
    assert_eq!(
        UserSubtitleStyle::from_config(&config).to_appearance(),
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
    let appearance = UserSubtitleStyle::from_config(&config).to_appearance();
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
fn invalid_color_falls_back_to_the_default_instead_of_failing() {
    let config = SubtitlesConfig {
        text_color: "not-a-color".to_owned(),
        ..SubtitlesConfig::default()
    };
    assert_eq!(
        UserSubtitleStyle::from_config(&config)
            .to_appearance()
            .text_color,
        SubtitleAppearance::youtube_like().text_color
    );
}

#[test]
fn out_of_range_position_falls_back_to_defaults() {
    let config = SubtitlesConfig {
        position_horizontal_percent: 200,
        position_vertical_percent: 500,
        ..SubtitlesConfig::default()
    };
    let style = UserSubtitleStyle::from_config(&config);
    assert_eq!(style.position, UserSubtitleStyle::default().position);
    assert_eq!(style.vertical_offset_fraction(), 0.0);
}

#[test]
fn out_of_range_text_size_keeps_the_preset_font_size() {
    let config = SubtitlesConfig {
        text_size_percent: 0,
        ..SubtitlesConfig::default()
    };
    assert_eq!(
        UserSubtitleStyle::from_config(&config)
            .to_appearance()
            .font_size,
        SubtitleAppearance::youtube_like().font_size
    );
}

#[test]
fn opacity_percent_maps_to_alpha_with_rounding() {
    assert_eq!(alpha_from_percent(0), 0);
    assert_eq!(alpha_from_percent(75), 191);
    assert_eq!(alpha_from_percent(100), 255);
}

#[test]
fn default_color_constants_of_the_config_are_parseable() {
    assert_eq!(
        default_color(DEFAULT_TEXT_COLOR),
        SubtitleRgb {
            red: 255,
            green: 255,
            blue: 255
        }
    );
    assert_eq!(
        default_color(DEFAULT_BACKGROUND_COLOR),
        SubtitleRgb {
            red: 8,
            green: 8,
            blue: 8
        }
    );
}

/// Прогоняет `commit_requests` через настоящий реестр настроек (id и значения проверяются
/// схемой, включая отрицательную позицию) и собирает секцию обратно.
fn apply_requests_to_defaults(style: &UserSubtitleStyle) -> SubtitlesConfig {
    let registry = app_config_registry().expect("registry builds");
    let mut document = AppConfig::default();
    for request in style.commit_requests() {
        let setting_id: SettingId = request.setting_id.clone();
        registry
            .set_value(&mut document, &setting_id, request.value)
            .unwrap_or_else(|error| panic!("{setting_id:?} rejected: {error}"));
    }
    document.subtitles
}

#[test]
fn custom_style_roundtrips_config_to_style_to_setting_values_and_back() {
    let config = custom_config();
    let style = UserSubtitleStyle::from_config(&config);
    let restored = apply_requests_to_defaults(&style);
    // Цвета возвращаются в канонической записи `#RRGGBB` заглавными.
    assert_eq!(restored.text_color, "#FFCC00");
    assert_eq!(restored.background_color, "#102030");
    assert_eq!(restored, config);
    assert_eq!(UserSubtitleStyle::from_config(&restored), style);
}

#[test]
fn default_style_roundtrips_to_default_config() {
    assert_eq!(
        apply_requests_to_defaults(&UserSubtitleStyle::default()),
        SubtitlesConfig::default()
    );
}

#[test]
fn commit_requests_cover_each_style_setting_exactly_once() {
    let mut ids: Vec<String> = UserSubtitleStyle::default()
        .commit_requests()
        .iter()
        .map(|request| request.setting_id.as_str().to_owned())
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 8, "{ids:?}");
}
