//! Renderer-neutral contracts shared by capability probing, render backends and the app shell.
//!
//! The concrete invariant owners are private modules. Public compatibility intentionally stays at
//! the crate root through the explicit re-exports below.

#![forbid(unsafe_code)]

mod capabilities;
mod color;
mod diagnostics;
mod frame;
mod live_settings;
mod shader_parameters;
mod viewport;

pub use capabilities::{P010RenderReadiness, RenderBackendKind, RenderCapabilities};
pub use color::{
    ActiveColorPath, ActiveColorPathFallback, ColorAdjustment, ColorPipelineSettings,
    HdrOutputMode, HdrToSdrSettings, HdrToneMappingOperator, RenderColorMetadata,
    RenderOutputColorSpace, SwapchainTransferMode, ToneMappingMode,
};
pub use diagnostics::{
    HdrMetadataDiagnosticMarker, HdrReferenceDefaultDiagnostics, RenderDiagnostics,
    RenderFrameContractRejection, RenderTextureDimension, RenderVideoOutputRejection,
};
pub use frame::{RenderableFrame, UiCompositionMode};
pub use live_settings::{
    RenderLiveApplyOutcome, RenderLiveApplyPhase, RenderLiveApplyReport, RenderLiveSettingId,
    RenderLiveSettings, RenderLiveSettingsAdapter, RenderLiveSettingsError,
    RenderLiveSettingsErrorKind, RenderLiveSettingsUpdate,
};
pub use shader_parameters::{
    ShaderNumericRange, ShaderParameter, ShaderParameterDescriptor, ShaderParameterId,
    ShaderParameterOptionId, ShaderParameterSet, ShaderParameterValue, ShaderParameterValueType,
};
pub use viewport::RenderViewport;
