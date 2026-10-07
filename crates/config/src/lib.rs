//! TOML-конфигурация fastiplayer.
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

pub use error::{ConfigError, ConfigResult, ConfigWriteFailure};
pub use frame_server::{FrameServerConfig, FrameServerLiveScrubDecodeModeConfig};
pub use paths::{CONFIG_FILE_NAME, ConfigPaths};
pub use schema::{
    AppConfig, AudioConfig, CURRENT_SCHEMA_VERSION, DEFAULT_DROPPED_FOLDER_MAX_DEPTH,
    DEFAULT_DROPPED_FOLDER_MAX_FILES, DEFAULT_SIDEBAR_WIDTH_POINTS, DroppedPlaylistFileAction,
    HdrToSdrConfig, HdrToSdrOperatorConfig, MAX_DROPPED_FOLDER_MAX_DEPTH,
    MAX_DROPPED_FOLDER_MAX_FILES, MAX_PREFERRED_VIDEO_HEIGHT, MAX_SIDEBAR_WIDTH_POINTS,
    MAX_WINDOW_CORNER_RADIUS_PX, MIN_DROPPED_FOLDER_MAX_DEPTH, MIN_DROPPED_FOLDER_MAX_FILES,
    MIN_SIDEBAR_WIDTH_POINTS, MIN_WINDOW_CORNER_RADIUS_PX, NetworkConfig, OpenGlesConfig,
    PausedCommitBehavior, PlayerConfig, PlayerDemuxConfig, PlayerSeekConfig, PlaylistConfig,
    PlaylistErrorBehavior, PlaylistPlaybackBehavior, PlaylistSiblingMediaFilter,
    PreferredVideoHeight, PreferredVideoHeightError, RenderColorAdjustmentConfig, RenderConfig,
    RenderProfile, ToneMappingMode, UiAnimationsConfig, UiConfig, UiSettingsConfig,
    UiSidebarConfig, UiWindowConfig, VideoBackendPreference, VideoCodec, VideoConfig,
    VideoSchedulerConfig, VulkanConfig, VulkanPresentMode, WebMediaConfig, WebMediaHdrSelection,
    YtDlpConfig,
};
pub(crate) use schema::{
    LEGACY_SCHEMA_VERSION_2, LEGACY_SCHEMA_VERSION_3, LEGACY_SCHEMA_VERSION_4,
    LEGACY_SCHEMA_VERSION_5, LEGACY_SCHEMA_VERSION_6, LEGACY_SCHEMA_VERSION_7,
    LEGACY_SCHEMA_VERSION_8, LEGACY_SCHEMA_VERSION_9, LEGACY_SCHEMA_VERSION_10,
    LEGACY_SCHEMA_VERSION_11,
};
pub use store::{
    BrokenConfigProblem, BrokenConfigRecovery, ConfigLoadOrigin, ConfigSaveOutcome,
    ConfigSavePolicy, ConfigSaveTarget, ConfigSessionOnlyReason, ConfigWriteProblem, LoadedConfig,
    load_from_path, load_or_create, load_or_create_at, load_or_recover_at,
    save_validated_atomic_at,
};
