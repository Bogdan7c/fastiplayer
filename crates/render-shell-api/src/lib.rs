//! Нейтральный контракт «приложение ↔ рендер» (issue #6, план `user/gles2-renderer/`).
//!
//! Приложение (`app-egui`) знает только этот crate и держит `Box<dyn RenderShell>`. Конкретные
//! рендеры — wgpu/Vulkan сегодня, свой GLES2 для старого железа, в будущем wgpu на Windows и
//! свой DirectX 10 — реализуют его и сами владеют импортом кадров декодера и их освобождением
//! после GPU. Поэтому новый рендер добавляется строкой каталога и своим crate-ом, а не правкой
//! приложения (решение владельца 3).
//!
//! # Что здесь есть
//!
//! - [`RenderShell`] — что приложение может попросить у рендера;
//! - [`PresentationReconfigured`] и соседи — жизненный цикл показа на окне («одно окно —
//!   одна surface», сессия 02a);
//! - [`VideoFrameImport`] — склад кадров: подключение декодера, ключи кадров, освобождение;
//! - [`UiTextureSync`] — синхронизация текстур интерфейса egui с рендером (полный набор для
//!   нового рендера, недоставленные изменения не теряются; сессия 02a);
//! - словарь кадра: исходы, причины пропуска, тайминги стадий, интерфейс, субтитры, маска углов;
//! - словарь каталога: [`RendererFamily`], [`RendererProbe`], [`RendererCreateError`];
//! - `conformance` (feature) — одинаковые для всех рендеров проверки контракта.
//!
//! # Границы зависимостей
//!
//! Ни одного типа wgpu/GL/EGL/Vulkan/winit/Linux. Это проверяет
//! `scripts/check-refactor-guardrails.py`: crate может зависеть только от нейтральных crate-ов
//! плеера (`render-core`, `video-*`-контракты, `subtitle-raster`) и от `egui` (только типы
//! мешей и текстур интерфейса).

#![forbid(unsafe_code)]

mod catalog;
#[cfg(any(test, feature = "conformance"))]
pub mod conformance;
mod frame;
mod frame_input;
mod identity;
mod present_settings;
mod presentation;
mod shell;
mod subtitle_frame;
mod surface_ticket;
mod ui_textures;
mod video_import;

pub use catalog::{
    RendererCreateError, RendererCreateStage, RendererFamily, RendererProbe,
    RendererUnusableReason, RequiredRendererFeature,
};
pub use frame::{
    EguiFrameContent, RenderFrameDropReason, RenderFrameFailure, RenderFrameFailureKind,
    RenderFrameOutcome, RenderFrameSlowestStage, RenderFrameStageTimings, RenderFrameTiming,
    RenderScreenDescriptor,
};
pub use frame_input::RenderFrameInput;
pub use identity::RendererInstanceId;
pub use present_settings::{ShellPresentMode, SurfacePresentSettings};
pub use presentation::{
    PresentationDetachOutcome, PresentationReattachError, PresentationReattachOutcome,
    PresentationReconfigureError, PresentationReconfigured,
};
pub use shell::{
    FollowUpFrame, RenderHotPathStats, RenderShell, RenderSurfaceSize, RendererDeviceLossReason,
    RendererDeviceLost, RendererGpuDrainError,
};
pub use subtitle_frame::{
    SubtitleCanvasOrigins, SubtitleLayerOffset, SubtitleLayerOffsets, SubtitleOverlayFrame,
};
pub use surface_ticket::{
    AcquiredSurfaceFrame, ForeignSurfaceFrame, OpenedSurfaceFrame, SurfaceFrameAcquire,
};
pub use ui_textures::{UiTextureFullSetReason, UiTextureSync};
pub use video_import::{
    DecoderSurfacePoolSlots, FrameImportCacheSnapshot, FrameImportError, FrameImportStats,
    FrameImportUnsupportedReason, FrameMaterialization, FrameMaterializationOutcome,
    HardwareFrameRefusal, PendingVideoImport, PreparedVideoImport, ReleasedFrameCount,
    RenderableFrameKey, VideoFrameImport, VideoImportHandoff, VideoImportInstallError,
    VideoImportPath, VideoImportPlan, VideoImportPrepareError, VideoImportPreparer,
    VideoImportReleaseError,
};

#[cfg(test)]
mod tests;
