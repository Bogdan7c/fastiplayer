//! Адаптер плеера к своему рендеру OpenGL ES 2.0 (issue #6, план `user/gles2-renderer/`).
//!
//! Переиспользуемые `gles2-egui` и `gles2-video` не знают плеера. Здесь — перевод словаря
//! плеера в их словарь:
//! - кадр программного декодера (`video-core::HostPlanarFrameDescriptor`) → плоскости
//!   [`gles2_video::PlanarFrameUpload`] без копии ([`host_planar_frame_upload`]);
//! - где и как рисовать: letterbox, исключения, поворот и цветовой путь — из `render-core`, того
//!   же источника, что у wgpu ([`VideoDrawPlan`]);
//! - метаданные кадра для правил `render-core` ([`renderable_frame`]);
//! - что умеет рендер на этом драйвере ([`gles2_render_capabilities`]).
//!
//! Сессия 05 — начало crate-а: программный путь 8 бит. Встраивание в приложение за контрактом
//! `RenderShell` (импорт кадров, освобождение кадра декодера по fence) — сессия 09.
//!
//! Правило освобождения кадра декодера (решение владельца сессии 05): кадр возвращается
//! декодеру только после fence кадра показа, в котором GPU его последний раз трогал, — одно
//! правило для копии, HW zero-copy (07) и SW zero-copy (12). Механизм — `gles2_egui::GpuCompletionQueue`.

mod capabilities;
mod draw_plan;
mod frame_metadata;
mod host_planar;
#[cfg(test)]
mod test_support;

pub use capabilities::{GLES2_SOFTWARE_FRAME_CONTRACTS, gles2_render_capabilities};
pub use draw_plan::{VideoDrawPlan, VideoDrawPlanError, VideoPlacement};
pub use frame_metadata::{NotYuvFrame, renderable_frame};
pub use host_planar::{HostPlanarFrameError, host_planar_frame_upload};
