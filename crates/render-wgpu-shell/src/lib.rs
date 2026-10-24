//! WGPU shell/composition backend.
//!
//! Crate владеет только shell-слоем:
//! - surface/swapchain lifecycle;
//! - subtitle overlay pass между video и egui (контракт — в `subtitle_overlay`);
//! - egui composition поверх video pass-а;
//! - submit/present полного кадра.
//!
//! Video renderer/materializer boundary остаётся в `render-wgpu-video`.

#![forbid(unsafe_code)]

mod egui_compositor;
mod frame;
mod shell;
mod subtitle_overlay;
mod surface_alpha;
mod surface_settings;
mod vulkan_device;
mod window_corner_mask;

pub use frame::{AcquiredSurfaceFrame, RenderFrameInput, SurfaceFrameAcquire};
// Нейтральный словарь кадра и настроек показа — из контракта `render-shell-api` (сессия 01
// плана `user/gles2-renderer/`); пути `render_wgpu_shell::...` для app-egui не меняются.
pub use render_shell_api::{
    EguiFrameContent, RenderFrameDropReason, RenderFrameFailure, RenderFrameOutcome,
    RenderFrameSlowestStage, RenderFrameStageTimings, RenderFrameTiming, RenderScreenDescriptor,
    ShellPresentMode, SubtitleLayerOffset, SubtitleLayerOffsets, SubtitleOverlayFrame,
    SurfaceAlphaPreference, SurfacePresentSettings, WindowCornerMask,
};
pub use shell::{GpuContext, GpuDeviceLost, Renderer, RendererGpuDrainError};
pub use subtitle_overlay::{
    SubtitleOverlayOutcome, SubtitleOverlayRenderer, SubtitleOverlayTarget,
    SubtitleOverlayUploadStats,
};
pub use vulkan_device::ForeignQueueFamilySupport;
