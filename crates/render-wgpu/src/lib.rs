//! WGPU render backend.
//!
//! Crate сохраняет прежние public exports, но теперь владеет только shell:
//! - surface/swapchain lifecycle;
//! - egui composition поверх video pass-а;
//! - re-export video boundary из `render-wgpu-video`.

#![forbid(unsafe_code)]

mod egui_compositor;
mod frame;
mod shell;

pub use frame::{
    RenderFrameDropReason, RenderFrameFailure, RenderFrameInput, RenderFrameOutcome,
    RenderScreenDescriptor,
};
pub use render_wgpu_video::{
    WgpuFramePlanes, WgpuFrameTextureViewLookup, WgpuFrameTextureViewMaterializer,
    WgpuFrameTextureViews, WgpuRenderableFrame, WgpuVideoRenderer, clear_to_black,
    required_wgpu_video_texture_features,
};
pub use shell::{GpuContext, Renderer};
