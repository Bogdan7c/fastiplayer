//! TOML-конфигурация rustiplayer.
//!
//! Crate отвечает только за пользовательский config: схему, defaults,
//! validation и чтение/создание файла на платформенном config-пути.
//! Playback и UI намеренно не живут здесь.

#![forbid(unsafe_code)]

mod error;
mod frame_server;
mod paths;
mod schema;
mod store;
mod validation;

pub use error::{ConfigError, ConfigResult};
pub use frame_server::{FrameServerConfig, FrameServerLiveScrubDecodeModeConfig};
pub use paths::{CONFIG_FILE_NAME, ConfigPaths};
pub use schema::{
    AppConfig, AudioConfig, CURRENT_SCHEMA_VERSION, HdrToSdrConfig, HdrToSdrOperatorConfig,
    NetworkConfig, OpenGlesConfig, PausedCommitBehavior, PlayerConfig, PlayerDemuxConfig,
    PlayerSeekConfig, RenderColorAdjustmentConfig, RenderConfig, RenderProfile, ToneMappingMode,
    UiAnimationsConfig, UiConfig, UiSettingsConfig, UiWindowConfig, VideoBackendPreference,
    VideoCodec, VideoConfig, VideoSchedulerConfig, VulkanConfig, VulkanPresentMode, YoutubeConfig,
};
pub(crate) use schema::{
    LEGACY_SCHEMA_VERSION_2, LEGACY_SCHEMA_VERSION_3, LEGACY_SCHEMA_VERSION_4,
};
pub use store::{
    LoadedConfig, load_from_path, load_or_create, load_or_create_at, save_validated_atomic_at,
};
