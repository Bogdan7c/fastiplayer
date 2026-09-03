//! WGPU shell/composition backend.
//!
//! Crate владеет только shell-слоем:
//! - surface/swapchain lifecycle;
//! - egui composition поверх video pass-а;
//! - submit/present полного кадра.
//!
//! Video renderer/materializer boundary остаётся в `render-wgpu-video`.

#![forbid(unsafe_code)]

mod egui_compositor;
mod frame;
mod shell;
mod surface_alpha;
mod surface_settings;
mod window_corner_mask;

pub use frame::{
    RenderFrameDropReason, RenderFrameFailure, RenderFrameInput, RenderFrameOutcome,
    RenderFrameSlowestStage, RenderFrameStageTimings, RenderFrameTiming, RenderScreenDescriptor,
};
pub use shell::{GpuContext, GpuDeviceLost, Renderer, RendererGpuDrainError};
pub use surface_alpha::SurfaceAlphaPreference;
pub use surface_settings::{ShellPresentMode, SurfacePresentSettings};
pub use window_corner_mask::WindowCornerMask;
