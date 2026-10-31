//! Binding между neutral `settings-core` и пользовательским `AppConfig`.
//!
//! Crate не владеет renderer/player/media runtime. Он строит registry, валидирует
//! и сохраняет `AppConfig`, а также переводит diff в typed owner-level routes.

#![forbid(unsafe_code)]

mod application_contract;

pub use application_contract::{
    RendererRecreationApplyError, RendererRecreationApplyErrorKind, RendererRecreationFailure,
    RendererRecreationRollbackError, RendererRecreationRollbackErrorKind,
    SettingApplicationContract, SettingApplyMechanism, SettingApplyTestScenario, SettingStateOwner,
    SettingsApplyFailure, SettingsApplyOutcome, SettingsBoundaryActivity,
    setting_application_contract,
};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fastiplayer_config::{
    AppConfig, ConfigSaveTarget, FrameServerConfig, HdrToSdrOperatorConfig, NetworkConfig,
    PlayerDemuxConfig, PlaylistConfig, ToneMappingMode as ConfigToneMappingMode, UiConfig,
    VideoBackendPreference, VideoCodec as ConfigVideoCodec, VulkanConfig, WebMediaConfig,
    YtDlpConfig,
};
use player_core::{
    AutomaticSurfacePool, PlayerRuntimeSettingId, PlayerRuntimeSettingsUpdate,
    PlayerRuntimeVideoBackendPreference, PlayerTickConfig, PlayerWorkerConfig,
};
use render_core::{
    ColorAdjustment, ColorPipelineSettings, HdrOutputMode, HdrToSdrSettings,
    HdrToneMappingOperator, RenderLiveSettings, RenderLiveSettingsUpdate, SwapchainTransferMode,
};
use settings_core::{
    ApplyMechanism, ApplyRouteReport, ApplyRouteResult, CommittedApplyRequest,
    CommittedFinalizeRequest, CommittedRollbackRequest, CommittedSettingsApplier, PersistReport,
    PersistRequest, RollbackReport, RollbackResult, RouteApplyUpdate, RouteGeneration,
    SettingApplyMode, SettingChange, SettingDescriptor, SettingId, SettingRouteId, SettingsDiff,
    SettingsError, SettingsPersister, SettingsRegistry, SettingsResult, SettingsSchema,
    SettingsValidator, ValidationReport, ValidationRequest,
};

mod peer_adoption;
mod routing;
#[cfg(test)]
mod routing_audio_muted_tests;
mod shared_persist;
mod transaction;

pub use peer_adoption::{PeerCopyAdoption, adoptable_peer_changes, peer_copy_adoption};
pub use routing::*;
pub use shared_persist::{
    SETTINGS_FILE_LOCK_WAIT, SettingsFileDurability, SettingsFilePersistOutcome,
    SettingsFileSaveDeferral, UnsavedChangesSave, settings_file_persist_outcome,
};
use shared_persist::{UnsavedSettingChanges, persist_changed_settings, save_unsaved_settings};
pub use transaction::*;
