//! Настройки субтитров в виде, удобном владельцу: стиль, лимиты, языки и «включено».
//!
//! Конфиг (`SubtitlesConfig`) приходит из двух мест: при создании `AppState` (стартовый
//! committed snapshot) и при каждом committed изменении настроек (`apply_config`). Здесь он
//! один раз переводится в готовые типы подсистем, а сравнение двух результатов подсказывает
//! владельцу, что именно изменилось (стиль — сразу, лимиты — со следующей загрузки).

use fastiplayer_config::SubtitlesConfig;
use subtitle_raster::RasterLimits;

use super::intents::SubtitleVisibility;
use super::limits::{LoadLimits, raster_limits_from_config};
use super::user_style::UserSubtitleStyle;

/// Настройки субтитров, разобранные из конфига.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SubtitleSettings {
    /// Показывать ли субтитры по умолчанию для следующего media.
    pub(super) default_visibility: SubtitleVisibility,
    /// Коды предпочитаемых языков в порядке приоритета (проверка кодов — при автовыборе).
    pub(super) preferred_language_codes: Vec<String>,
    /// Пользовательский стиль (позиция, размер, цвета, оформление).
    pub(super) style: UserSubtitleStyle,
    /// Лимиты фоновой загрузки (захватываются задачей при запуске).
    pub(super) load_limits: LoadLimits,
    /// Лимиты растеризатора (действуют с создания растеризатора).
    pub(super) raster_limits: RasterLimits,
}

impl SubtitleSettings {
    /// Разбирает секцию конфига.
    pub(super) fn from_config(config: &SubtitlesConfig) -> Self {
        Self {
            default_visibility: SubtitleVisibility::from_enabled(config.enabled),
            preferred_language_codes: config
                .preferred_language_codes()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            style: UserSubtitleStyle::from_config(config),
            load_limits: LoadLimits::from_config(config),
            raster_limits: raster_limits_from_config(config),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_flag_and_language_list_are_parsed_from_the_config() {
        let config = SubtitlesConfig {
            enabled: true,
            preferred_languages: "uk, ru ,".to_owned(),
            ..SubtitlesConfig::default()
        };
        let settings = SubtitleSettings::from_config(&config);
        assert_eq!(settings.default_visibility, SubtitleVisibility::Shown);
        assert_eq!(settings.preferred_language_codes, vec!["uk", "ru"]);
        assert_eq!(
            SubtitleSettings::from_config(&SubtitlesConfig::default()).default_visibility,
            SubtitleVisibility::Hidden,
            "subtitles are off by default"
        );
    }
}
