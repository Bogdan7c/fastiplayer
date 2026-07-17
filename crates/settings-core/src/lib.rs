//! Project-agnostic contracts for settings metadata, registry access and diffs.
//!
//! `settings-core` does not know about rustiplayer config structs, UI widgets,
//! render backends, GPU resources or operating-system option providers. It owns
//! the neutral vocabulary that project-specific crates can bind to real storage.

#![forbid(unsafe_code)]

mod controller;
mod diff;
mod error;
mod metadata;
mod options;
mod registry;

#[cfg(test)]
mod tests;

pub use controller::{
    ApplyFinalState, ApplyMechanism, ApplyReport, ApplyRouteReport, ApplyRouteResult, CancelReport,
    CommittedApplyRequest, CommittedFinalizeRequest, CommittedRollbackRequest,
    CommittedSettingsApplier, PendingPreviewUpdate, PersistOutcome, PersistReport, PersistRequest,
    PreviewApplyReport, PreviewApplyRequest, PreviewApplyResult, PreviewRollbackRequest,
    PreviewRollbacker, PreviewRouteState, PreviewSettingsApplier, PreviewTransactionState,
    ResetReport, ResetScope, RollbackReport, RollbackResult, RouteApplyUpdate, RouteGeneration,
    RouteGenerationConflict, RuntimeSettingCommitRequest, SetValueReport, SettingsController,
    SettingsPersister, SettingsValidator, ValidationReport, ValidationRequest,
};
pub use diff::{SettingChange, SettingsDiff};
pub use error::{
    ListLengthLimitKind, SettingValueError, SettingsError, SettingsResult, TextLengthLimitKind,
    ValueRangeLimit,
};
pub use metadata::{
    AutoFixedPositiveIntegerDescriptor, DefaultBehavior, NumericDescriptor, NumericRange,
    NumericStep, SelectDescriptor, SelectListDescriptor, SettingAccess, SettingApplyMode,
    SettingDescriptor, SettingDescriptorText, SettingEditor, SettingGroupId, SettingId,
    SettingPath, SettingPlacement, SettingRouteId, SettingSectionId, SettingText, SettingTextId,
    SettingUnit, SettingValue, SettingValueKind, SettingValueType, SettingsSurfaceId,
    TextDescriptor, TextFormat, VectorDescriptor,
};
pub use options::{
    OptionProviderId, SettingOption, SettingOptionCurrentValue, SettingOptionId,
    SettingOptionProvider, SettingOptions, SettingOptionsError, SettingOptionsRequest,
    SettingOptionsStatus,
};
pub use registry::{RegisteredSetting, SettingAccessor, SettingsRegistry, SettingsSchema};
