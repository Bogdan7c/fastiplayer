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

use fastiplayer_config::{
    AppConfig, FrameServerConfig, HdrToSdrOperatorConfig, NetworkConfig, OpenGlesConfig,
    PlayerDemuxConfig, PlaylistConfig, RenderProfile, ToneMappingMode as ConfigToneMappingMode,
    UiConfig, VideoBackendPreference, VideoCodec as ConfigVideoCodec, VulkanConfig, WebMediaConfig,
    YtDlpConfig, save_validated_atomic_at,
};
use player_core::{
    PlayerRuntimeSettingId, PlayerRuntimeSettingsUpdate, PlayerRuntimeVideoBackendPreference,
    PlayerTickConfig, PlayerWorkerConfig,
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

mod routing;
mod transaction;

pub use routing::*;
pub use transaction::*;
