//! Фундамент своего рендера OpenGL ES 2.0 для старого железа (issue #6, план
//! `user/gles2-renderer/`, сессия 03).
//!
//! Переиспользуемый crate: подключается к любому приложению (плеер Fastiplayer, редактор
//! Fasticut) и не знает ни одного crate-а плеера. Из чужого — только тонкие «провода» к драйверу:
//! `glow` (таблица функций GL) и `khronos-egl` (EGL), плюс `wayland-sys` для `wl_egl_window`.
//!
//! # Что здесь есть
//! - [`Gles2Context`] — контекст GLES 2.0 на окне (нативный Wayland и X11/XWayland, непрозрачное окно
//!   всегда) или без окна ([`Gles2Context::offscreen`], стенд пиксельных тестов);
//! - [`Gles2Capabilities`] — что реально умеет драйвер, и уровни из таблицы README плана
//!   ([`CapabilityTiers`]). Строго ES 2.0, даже если драйвер отдал ES 3.2;
//! - кадр: [`Gles2Context::begin_frame`] → [`ActiveFrame`] → [`ActiveFrame::present`] с
//!   таймингами [`Gles2FrameTimings`];
//! - показ: [`PresentationSettings`] (vsync), перенастройка на месте, отпуск и
//!   возврат окна (одно окно — одна EGLSurface);
//! - «GPU закончил»: [`GpuFence`] (ждать можно из любого потока) и [`GpuCompletionQueue`]
//!   (освобождение ровно один раз);
//! - потеря контекста: [`ContextLoss`] — процесс живёт, вызывающий пересоздаёт контекст;
//! - шейдеры только `#version 100` ([`Gles2Context::compile_es100_program`]);
//! - художник egui ([`EguiPainter`], сессия 04): текстуры, меши, отсечение, удержание UI-кадра
//!   для повтора без прохода egui — пиксель в пиксель с egui-wgpu;
//! - сборка кадра ([`FrameCompositor`]): фон → подложка/видео ([`FrameLayers`]) → оверлеи →
//!   egui → показ, в том же порядке, что у wgpu-шелла.
//!
//! # Безопасность
//! Весь `unsafe` FFI — в модуле `context::safety` (EGL, libwayland-egl, загрузка GL),
//! `context::safety::gl` (вызовы GL фундамента) и `draw_gl` (вызовы GL художника), каждый
//! блок с комментарием `// SAFETY:`. Владение
//! `EGLSurface`/`EGLContext`/`EGLDisplay`/`EGLSync` — RAII-типы, порядок разрушения
//! surface → context → display закреплён тестами на fake-драйвере.
//!
//! # Границы зависимостей
//! Ни одной зависимости от crate-ов workspace (кроме будущих `gles2-*`): это проверяет
//! `scripts/check-refactor-guardrails.py`.

// `unsafe` запрещён везде, кроме `context::safety` и `draw_gl` (там разрешён явно и с
// `// SAFETY:`).
#![deny(unsafe_code)]

mod capabilities;
mod composition;
mod context;
mod damage;
#[allow(
    unsafe_code,
    reason = "модуль безопасности вызовов GL художника и маски; каждый блок с // SAFETY:"
)]
mod draw_gl;
mod error;
mod fence;
mod frame;
mod frame_layer_stack;
mod loss;
mod offscreen;
mod painter;
mod presentation;
mod shader;
#[cfg(test)]
mod test_support;

pub use capabilities::{
    CapabilityTiers, Conveniences, DmaBufFormatSupport, DmaBufImportSupport, DrmFourcc,
    DrmModifier, ExtensionSet, FormatSampling, Gles2Capabilities, MissingRequirement,
    ModifierSampling, TierStatus,
};
pub use composition::{
    BackgroundFillOutcome, FrameCompositor, FrameLayerError, FrameLayers, NoFrameLayers,
    PresentedUiFrame, UiContent, UiFrameDropReason, UiFrameError, UiFrameOutcome, UiFrameRequest,
    UiFrameTimings, UiFrameWork, UnderlayCoverage,
};
pub use context::dma_buf_texture::{
    DmaBufAttribute, DmaBufMissingRequirement, DmaBufPlaneTexture, DmaBufTextureAttached,
    DmaBufTextureError, DmaBufTextureStats, ImageRelease, ModifierAttribute,
};
pub use context::driver::{
    DmaBufModifier, EglCallError, EglErrorCode, EglOperation, EglVersion, SurfaceSize,
};
pub use context::gles_version::GlesVersion;
pub use context::platform::UnsupportedWindowSystem;
pub use context::{
    ContextId, GlAccessError, Gles2Context, PixelExtent, ReadbackError, WindowSource,
};
pub use damage::{
    BufferAge, DAMAGE_HISTORY_FRAMES, DamageHistory, FullRepaintReason, LayersDamage,
    PartialPresentStats, PixelRect, RepaintPlan, TargetArea, describe_partial_present_support,
};
pub use draw_gl::DrawGlError;
pub use error::{CreateError, Gles2Error};
pub use fence::{
    CompletedPayloads, FenceStats, FenceStatus, FenceStrategy, FenceWaitError, GpuCompletionQueue,
    GpuFence,
};
pub use frame::{
    ActiveFrame, FrameSkipReason, FrameStart, Gles2FrameTimings, PresentKind, PresentedFrame,
};
pub use frame_layer_stack::FrameLayerStack;
pub use loss::{ContextLoss, ContextLossDetection, ContextLossReason};
pub use offscreen::OffscreenPixels;
pub use painter::{
    EffectiveDithering, EguiPainter, EguiPainterOptions, EguiPainterStats, IndexWidthPreference,
    PainterError, RetainedUiUnavailable, TextureApplyReport, TextureFreeOutcome, UiDamageTracking,
    UiDithering, UiPaintWork, UiPrepareWork, UiScreen, UiTextureUpload, VertexArrayPreference,
};
pub use presentation::{
    DetachOutcome, FramebufferAlpha, PresentationError, PresentationReconfigured,
    PresentationSettings, PresentationState, ReattachOutcome, SwapIntervalOutcome, VsyncMode,
    WindowSystem,
};
pub use shader::{Es100ProgramSources, ShaderError, ShaderStage};
