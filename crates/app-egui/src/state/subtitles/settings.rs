//! Настройки субтитров в виде, удобном владельцу: стиль, лимиты, языки и «включено».
//!
//! Конфиг (`SubtitlesConfig`) приходит из двух мест: при создании `AppState` (стартовый
//! committed snapshot) и при каждом committed изменении настроек (`apply_config`). Здесь он
//! один раз переводится в готовые типы подсистем, а сравнение двух результатов подсказывает
//! владельцу, что именно изменилось (стиль — сразу, лимиты — со следующей загрузки).

use std::time::Duration;

use fastiplayer_config::SubtitlesConfig;
use subtitle_raster::RasterLimits;

use super::intents::SubtitleVisibility;
use super::limits::{EmbeddedLimits, LoadLimits, StreamLimits, raster_limits_from_config};
use super::owner_placement::AssLayerPositioning;
use super::presentation::PresenterSettings;
use super::user_style::UserSubtitleStyle;

/// Бюджет ожидания сети для ленты потоковых субтитров: тот же `network.reconnect_wait_ms`,
/// что у основного источника, — дорожка не должна сдаваться раньше, чем видео перестало бы
/// ждать сеть. Живёт вне секции `[subtitles]`, поэтому читается из всего конфига.
pub(in crate::state) fn network_reconnect_wait_of(
    config: &fastiplayer_config::AppConfig,
) -> Duration {
    Duration::from_millis(config.network.reconnect_wait_ms)
}

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
    /// На сколько кадров вперёд рисовать (действует с создания потока-рисовальщика).
    pub(super) prerender_frames: u32,
    /// Сколько готовых картинок держать (действует с создания потока-рисовальщика).
    pub(super) prepared_overlays: u32,
    /// Лимиты встроенных дорожек (читаются владельцем в момент использования).
    pub(super) embedded_limits: EmbeddedLimits,
    /// Лимиты потоковых субтитров (захватываются лентой при её запуске).
    pub(super) stream_limits: StreamLimits,
    /// Как размещать слой ASS/SSA (режим «ASS position»); читает размещение слоя.
    pub(super) ass_positioning: AssLayerPositioning,
}

impl SubtitleSettings {
    /// Настройки потока-рисовальщика: лимиты растеризатора и размеры подготовки наперёд.
    pub(super) fn presenter_settings(&self) -> PresenterSettings {
        PresenterSettings {
            raster_limits: self.raster_limits,
            prerender_frames: self.prerender_frames,
            prepared_overlays: self.prepared_overlays,
        }
    }

    /// Разбирает секцию конфига. `network_reconnect_wait` - `network.reconnect_wait_ms`
    /// (бюджет ожидания сети для ленты потоковых субтитров, см. `StreamLimits`).
    pub(super) fn from_config(config: &SubtitlesConfig, network_reconnect_wait: Duration) -> Self {
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
            prerender_frames: config.prerender_frames,
            prepared_overlays: config.prepared_overlays,
            embedded_limits: EmbeddedLimits::from_config(config),
            stream_limits: StreamLimits::from_config(config, network_reconnect_wait),
            ass_positioning: AssLayerPositioning::from_config(config.ass_position_mode),
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
        let settings = SubtitleSettings::from_config(&config, Duration::from_secs(60));
        assert_eq!(settings.default_visibility, SubtitleVisibility::Shown);
        assert_eq!(settings.preferred_language_codes, vec!["uk", "ru"]);
        assert_eq!(
            SubtitleSettings::from_config(&SubtitlesConfig::default(), Duration::from_secs(60))
                .default_visibility,
            SubtitleVisibility::Hidden,
            "subtitles are off by default"
        );
    }
}
