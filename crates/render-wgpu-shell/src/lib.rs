//! WGPU shell/composition backend.
//!
//! Crate владеет только shell-слоем:
//! - surface/swapchain lifecycle;
//! - subtitle overlay pass между video и egui (контракт — в `subtitle_overlay`);
//! - egui composition поверх video pass-а;
//! - submit/present полного кадра;
//! - склад кадров декодера (`video_import`): материализаторы `render-wgpu-video`, обёртка
//!   отложенных освобождений и их перепривязка при пересоздании.
//!
//! Приложение видит этот рендер только как `render_shell_api::RenderShell` (сессия 02 плана
//! `user/gles2-renderer/`); конкретный [`Renderer`] создаёт фабрика каталога рендеров.
//! Video renderer/materializer boundary остаётся в `render-wgpu-video`.

#![forbid(unsafe_code)]

mod egui_compositor;
mod frame;
mod shell;
mod subtitle_overlay;
mod surface_alpha;
mod surface_settings;
#[cfg(test)]
mod test_support;
mod video_import;
mod vulkan_device;
mod window_corner_mask;

// Нейтральный словарь кадра — из контракта `render-shell-api`; внутри crate-а он доступен
// по коротким путям `crate::X`. Наружу не реэкспортируется: приложение берёт его из контракта.
pub(crate) use render_shell_api::{
    EguiFrameContent, RenderFrameDropReason, RenderFrameFailure, RenderFrameOutcome,
    RenderFrameStageTimings, RenderFrameTiming, RenderScreenDescriptor, ShellPresentMode,
    SurfaceAlphaPreference, SurfacePresentSettings, WindowCornerMask,
};
// Сдвиги слоёв субтитров нужны только GPU-тестам subtitle overlay.
#[cfg(test)]
pub(crate) use render_shell_api::{SubtitleLayerOffset, SubtitleLayerOffsets};
pub use shell::Renderer;
pub use subtitle_overlay::{
    SubtitleOverlayOutcome, SubtitleOverlayRenderer, SubtitleOverlayTarget,
    SubtitleOverlayUploadStats,
};
pub use vulkan_device::ForeignQueueFamilySupport;
