//! Фундамент своего рендера OpenGL ES 2.0 для старого железа (issue #6, план
//! `user/gles2-renderer/`, сессия 03).
//!
//! Переиспользуемый crate: подключается к любому приложению (плеер Fastiplayer, редактор
//! Fasticut) и не знает ни одного crate-а плеера. Из чужого — только тонкие «провода» к драйверу:
//! `glow` (таблица функций GL) и `khronos-egl` (EGL), плюс `wayland-sys` для `wl_egl_window`.
//!
//! # Что здесь есть
//! - [`Gles2Context`] — контекст GLES 2.0 на окне (нативный Wayland и X11/XWayland, прозрачность
//!   для скруглённых углов) или без окна ([`Gles2Context::offscreen`], стенд пиксельных тестов);
//! - [`Gles2Capabilities`] — что реально умеет драйвер, и уровни из таблицы README плана
//!   ([`CapabilityTiers`]). Строго ES 2.0, даже если драйвер отдал ES 3.2;
//! - кадр: [`Gles2Context::begin_frame`] → [`ActiveFrame`] → [`ActiveFrame::present`] с
//!   таймингами [`Gles2FrameTimings`];
//! - показ: [`PresentationSettings`] (vsync, прозрачность), перенастройка на месте, отпуск и
//!   возврат окна (одно окно — одна EGLSurface);
//! - «GPU закончил»: [`GpuFence`] (ждать можно из любого потока) и [`GpuCompletionQueue`]
//!   (освобождение ровно один раз);
//! - потеря контекста: [`ContextLoss`] — процесс живёт, вызывающий пересоздаёт контекст;
//! - шейдеры только `#version 100` ([`Gles2Context::compile_es100_program`]).
//!
//! # Безопасность
//! Весь `unsafe` FFI — в модуле `context::safety` (EGL, libwayland-egl, загрузка GL) и
//! `context::safety::gl` (вызовы GL), каждый блок с комментарием `// SAFETY:`. Владение
//! `EGLSurface`/`EGLContext`/`EGLDisplay`/`EGLSync` — RAII-типы, порядок разрушения
//! surface → context → display закреплён тестами на fake-драйвере.
//!
//! # Границы зависимостей
//! Ни одной зависимости от crate-ов workspace (кроме будущих `gles2-*`): это проверяет
//! `scripts/check-refactor-guardrails.py`.

// `unsafe` запрещён везде, кроме `context::safety` (там разрешён явно и с `// SAFETY:`).
#![deny(unsafe_code)]

mod capabilities;
mod context;
mod error;
mod fence;
mod frame;
mod loss;
mod offscreen;
mod presentation;
mod shader;
#[cfg(test)]
mod test_support;

pub use capabilities::{
    CapabilityTiers, Conveniences, DmaBufFormatSupport, DmaBufImportSupport, DrmFourcc,
    ExtensionSet, FormatSampling, Gles2Capabilities, MissingRequirement, TierStatus,
};
pub use context::driver::{
    DmaBufModifier, EglCallError, EglErrorCode, EglOperation, EglVersion, SurfaceSize,
};
pub use context::gles_version::GlesVersion;
pub use context::platform::UnsupportedWindowSystem;
pub use context::{GlAccessError, Gles2Context, PixelExtent, ReadbackError, WindowSource};
pub use error::{CreateError, Gles2Error};
pub use fence::{
    CompletedPayloads, FenceStats, FenceStatus, FenceStrategy, FenceWaitError, GpuCompletionQueue,
    GpuFence,
};
pub use frame::{ActiveFrame, FrameSkipReason, FrameStart, Gles2FrameTimings, PresentedFrame};
pub use loss::{ContextLoss, ContextLossDetection, ContextLossReason};
pub use offscreen::OffscreenPixels;
pub use presentation::{
    DetachOutcome, OpaqueWindowReason, PresentationError, PresentationReconfigured,
    PresentationSettings, PresentationState, ReattachOutcome, SwapIntervalOutcome, VsyncMode,
    WindowAlphaMode, WindowAlphaRequest, WindowSystem,
};
pub use shader::{Es100ProgramSources, ShaderError, ShaderStage};
