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

pub use frame::{
    RenderFrameDropReason, RenderFrameFailure, RenderFrameInput, RenderFrameOutcome,
    RenderScreenDescriptor,
};
pub use shell::{GpuContext, Renderer};
