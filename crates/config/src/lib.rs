//! TOML-конфигурация rustiplayer.
//!
//! Crate отвечает только за пользовательский config: схему, defaults,
//! validation и чтение/создание файла на платформенном config-пути.
//! Playback, UI и storage намеренно не живут здесь.

#![forbid(unsafe_code)]

mod error;
mod paths;
mod schema;
mod store;
mod validation;

pub use error::{ConfigError, ConfigResult};
pub use paths::{CONFIG_FILE_NAME, ConfigPaths};
pub use schema::{
    AppConfig, AudioConfig, CURRENT_SCHEMA_VERSION, NetworkConfig, OpenGlesConfig, PlayerConfig,
    RenderConfig, RenderProfile, ToneMappingMode, UiConfig, VideoBackendPreference, VideoCodec,
    VideoConfig, VulkanConfig, VulkanPresentMode, YoutubeConfig,
};
pub use store::{LoadedConfig, load_from_path, load_or_create, load_or_create_at};
