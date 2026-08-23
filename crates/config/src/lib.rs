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
    AppConfig, AudioConfig, CURRENT_SCHEMA_VERSION, DEFAULT_SIDEBAR_WIDTH_POINTS, HdrToSdrConfig,
    HdrToSdrOperatorConfig, MAX_PREFERRED_VIDEO_HEIGHT, MAX_SIDEBAR_WIDTH_POINTS,
    MIN_SIDEBAR_WIDTH_POINTS, NetworkConfig, OpenGlesConfig, PausedCommitBehavior, PlayerConfig,
    PlayerDemuxConfig, PlayerSeekConfig, PlaylistConfig, PlaylistErrorBehavior,
    PlaylistPlaybackBehavior, PlaylistSiblingMediaFilter, PreferredVideoHeight,
    PreferredVideoHeightError, RenderColorAdjustmentConfig, RenderConfig, RenderProfile,
    ToneMappingMode, UiAnimationsConfig, UiConfig, UiSettingsConfig, UiSidebarConfig,
    UiWindowConfig, VideoBackendPreference, VideoCodec, VideoConfig, VideoSchedulerConfig,
    VulkanConfig, VulkanPresentMode, YtDlpConfig, YtDlpHdrSelection,
};
pub(crate) use schema::{
    LEGACY_SCHEMA_VERSION_2, LEGACY_SCHEMA_VERSION_3, LEGACY_SCHEMA_VERSION_4,
    LEGACY_SCHEMA_VERSION_5, LEGACY_SCHEMA_VERSION_6, LEGACY_SCHEMA_VERSION_7,
};
pub use store::{
    LoadedConfig, load_from_path, load_or_create, load_or_create_at, save_validated_atomic_at,
};
