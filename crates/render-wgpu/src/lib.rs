//! WGPU render backend.
//!
//! Crate сохраняет прежние public exports, но внутренне разделяет:
//! - фасад video renderer-а и NV12/P010 dispatch;
//! - shell для surface/swapchain lifecycle;
//! - egui composition поверх video pass-а;
//! - mapping backend capabilities.

#![forbid(unsafe_code)]

mod capabilities;
mod color_pipeline;
mod egui_compositor;
mod frame;
mod shell;
mod video;

#[cfg(test)]
mod bt2446c_reference;

pub use frame::{
    RenderFrameDropReason, RenderFrameFailure, RenderFrameInput, RenderFrameOutcome,
    RenderScreenDescriptor,
};
pub use shell::{GpuContext, Renderer};
pub use video::{WgpuFramePlanes, WgpuRenderableFrame, WgpuVideoRenderer, clear_to_black};
