//! Адаптер плеера к своему рендеру OpenGL ES 2.0 (issue #6, план `user/gles2-renderer/`).
//!
//! Переиспользуемые `gles2-egui` и `gles2-video` не знают плеера. Здесь — перевод словаря
//! плеера в их словарь:
//! - кадр программного декодера (`video-core::HostPlanarFrameDescriptor`) → плоскости
//!   [`gles2_video::PlanarFrameUpload`] без копии ([`host_planar_frame_upload`]);
//! - где и как рисовать: letterbox, исключения, поворот и цветовой путь — из `render-core`, того
//!   же источника, что у wgpu ([`VideoDrawPlan`]);
//! - метаданные кадра для правил `render-core` ([`renderable_frame`]);
//! - аппаратный кадр NV12 из DMA-BUF без копии ([`materialize_dma_buf_frame`]);
//! - что умеет рендер на этом драйвере ([`gles2_render_capabilities`]);
//! - диагностика кадра в нейтральных типах контракта: стадии, цветовой путь, счётчики импорта
//!   ([`render_frame_timing`], [`video_pass_diagnostics`], [`frame_import_stats`]) — те же имена,
//!   что у wgpu, для трасс и инструментов замера (сессия 08);
//! - субтитры кадра поверх видео ([`Gles2SubtitleOverlayRenderer`], слой кадра
//!   [`SubtitleOverlayLayer`]) — паритет с wgpu-проходом субтитров (сессия 08);
//! - полный рендер за контрактом приложения ([`Gles2Renderer`]: `RenderShell` + склад кадров
//!   `VideoFrameImport` + переносимый подготовщик импорта [`Gles2VideoImportPreparer`]) и проба
//!   для каталога рендеров ([`probe_gles2_renderer`]) — сессия 09.
//!
//! Сессия 05 — начало crate-а: программный путь 8 бит; сессия 06 — 10/12 бит и HDR → SDR;
//! сессия 07 — аппаратный NV12 (P010 не объявляется, решение владельца 33); сессия 09 —
//! встраивание в приложение за контрактом `RenderShell`: кадр, показ, живые настройки, потеря
//! контекста, импорт кадров декодера и их освобождение по fence.
//!
//! Правило освобождения кадра декодера (решение владельца сессии 05): кадр возвращается
//! декодеру только после fence кадра показа, в котором GPU его последний раз трогал, — одно
//! правило для копии, HW zero-copy (07) и SW zero-copy (12). Механизм — `gles2_egui::GpuCompletionQueue`.

mod capabilities;
mod diagnostics;
mod dma_buf_frame;
mod draw_plan;
mod frame_metadata;
mod frame_release;
mod high_bit_color;
mod host_planar;
mod shell;
mod subtitle_overlay;
#[cfg(test)]
mod test_support;
mod video_import;

pub use capabilities::{
    GLES2_HARDWARE_FRAME_CONTRACTS, GLES2_HIGH_BIT_SOFTWARE_FRAME_CONTRACTS,
    GLES2_SOFTWARE_FRAME_CONTRACTS, HardwareZeroCopy, gles2_render_capabilities,
    gles2_render_capabilities_for,
};
pub use diagnostics::{
    frame_import_stats, render_frame_stage_timings, render_frame_timing, video_pass_diagnostics,
};
pub use dma_buf_frame::{
    DmaBufMaterialization, DmaBufMaterializeOutcome, DmaBufShownFrom, DmaBufUnsupportedReason,
    Nv12DmaBufError, dma_buf_buffer_identity, dma_buf_frame_key, dma_buf_import_capacity_for,
    ensure_nv12_frame_contract, materialize_dma_buf_frame, nv12_dma_buf_frame,
};
pub use draw_plan::{VideoDrawPlan, VideoDrawPlanError, VideoPlacement};
pub use frame_metadata::{NotYuvFrame, renderable_frame};
pub use frame_release::{
    FrameReleaseStats, Gles2FrameReleaseBinding, PendingReleaseDrain, PresentedWorkWait,
    RendererDetachGrace, RendererDetachReport, wrap_video_backend_for_gles2_release,
};
pub use high_bit_color::{HdrColorRejection, HighBitColorError, high_bit_color_conversion};
pub use host_planar::{HostPlanarFrameError, host_planar_frame_upload};
pub use shell::{Gles2FrameCounters, Gles2Renderer, probe_gles2_renderer};
pub use subtitle_overlay::{
    Gles2SubtitleOverlayRenderer, SubtitleOverlayError, SubtitleOverlayLayer,
    SubtitleOverlayOutcome, SubtitleOverlayUploadStats,
};
pub use video_import::Gles2VideoImportPreparer;
