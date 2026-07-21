use serde::{Deserialize, Serialize};

use crate::{ConfigResult, frame_server::FrameServerConfig, validation};

mod default_document;
mod player;
mod playlist;
mod render;
mod services;
mod ui;
mod version;
mod video;
mod yt_dlp_settings;

#[cfg(test)]
mod metadata_tests;

pub use player::*;
pub use playlist::*;
pub use render::*;
pub use services::*;
pub use ui::*;
pub use version::CURRENT_SCHEMA_VERSION;
pub(crate) use version::{
    LEGACY_SCHEMA_VERSION_2, LEGACY_SCHEMA_VERSION_3, LEGACY_SCHEMA_VERSION_4,
    LEGACY_SCHEMA_VERSION_5, LEGACY_SCHEMA_VERSION_6,
};
pub use video::*;

/// Полная пользовательская конфигурация приложения.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// Версия TOML-схемы; обязательна для будущих миграций config.
    #[setting(
        id = "schema_version",
        path = "schema_version",
        section = "system",
        group = "schema",
        surface = "main-settings-window",
        label_id = "settings.system.schema_version.label",
        label_ru = "Версия схемы",
        description_id = "settings.system.schema_version.description",
        description_ru = "Read-only версия TOML-схемы для будущих миграций.",
        editor = "read_only",
        apply = "system.apply",
        read_only,
        default = "no_reset"
    )]
    pub schema_version: u32,

    /// Поведение playback state machine и выбора потоков.
    #[serde(default)]
    #[setting(nested)]
    pub player: PlayerConfig,

    /// Playlist discovery, traversal defaults и persistence policy.
    #[serde(default)]
    #[setting(nested)]
    pub playlist: PlaylistConfig,

    /// Decode-ограничения и backend preference.
    #[serde(default)]
    #[setting(nested)]
    pub video: VideoConfig,

    /// Persisted TOML-настройки Frame Server слоя.
    #[serde(default)]
    #[setting(nested)]
    pub frame_server: FrameServerConfig,

    /// Render-профиль и backend-specific настройки.
    #[serde(default)]
    #[setting(nested)]
    pub render: RenderConfig,

    /// Настройки аудиовыхода.
    #[serde(default)]
    #[setting(nested)]
    pub audio: AudioConfig,

    /// Настройки сетевого read-ahead/cache слоя.
    #[serde(default)]
    #[setting(nested)]
    pub network: NetworkConfig,

    /// Настройки YtDlp/service слоя.
    #[serde(default)]
    #[setting(nested)]
    pub yt_dlp: YtDlpConfig,

    /// Настройки shell UI.
    #[serde(default)]
    #[setting(nested)]
    pub ui: UiConfig,
}

impl AppConfig {
    /// Проверяет значения, которые Serde не может выразить типами.
    pub fn validate(&self) -> ConfigResult<()> {
        validation::validate_app_config(self)
    }

    /// Сериализует config в читаемый generated TOML для записи user config-файла.
    pub fn to_pretty_toml(&self) -> ConfigResult<String> {
        let mut toml_text = toml::to_string_pretty(self)
            .map_err(|source| crate::ConfigError::SerializeDefaultConfig { source })?;

        if !toml_text.ends_with('\n') {
            toml_text.push('\n');
        }
        default_document::document_current_schema_defaults(&mut toml_text);

        Ok(toml_text)
    }
}

impl Default for AppConfig {
    /// Возвращает production defaults для первого запуска без config-файла.
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            player: PlayerConfig::default(),
            playlist: PlaylistConfig::default(),
            video: VideoConfig::default(),
            frame_server: FrameServerConfig::default(),
            render: RenderConfig::default(),
            audio: AudioConfig::default(),
            network: NetworkConfig::default(),
            yt_dlp: YtDlpConfig::default(),
            ui: UiConfig::default(),
        }
    }
}
