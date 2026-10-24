//! Видеослой своего рендера OpenGL ES 2.0 для старого железа (issue #6, план
//! `user/gles2-renderer/`, сессия 05).
//!
//! Переиспользуемый crate: подключается к любому приложению на `gles2-egui` и не знает ни одного
//! crate-а плеера. Свой словарь кадра ([`PlanarFrameUpload`], [`PlaneSource`]) — перевод
//! дескрипторов декодера плеера в него делает адаптер (`render-gles2-shell`).
//!
//! # Что здесь есть
//! - планарный YUV 8 бит 4:2:0 / 4:2:2 / 4:4:4 из памяти процесса с **ровно одной копией**
//!   (драйвер копирует строки в текстуру; без `GL_EXT_unpack_subimage` — текстура шириной в
//!   stride, хвост строки обрезает шейдер, без переупаковки);
//! - источник плоскостей сразу допускает 0 копий ([`PlaneSource::DmaBuf`], реализуют сессии 07
//!   и 12);
//! - кольцо слотов текстур ([`UploadSlotCount`], по умолчанию 3): запись только в слот, который
//!   GPU больше не читает (fence кадра); нет слота — [`UploadOutcome::Busy`], кольцо не растёт;
//! - перевод YUV → RGB (BT.601/BT.709, limited/full), цветокоррекция, поворот, letterbox и
//!   исключения (сайдбар) — строгий паритет с wgpu-рендером плеера;
//! - подложка кадра [`VideoUnderlay`] для `gles2_egui::FrameCompositor`.
//!
//! # Безопасность
//! Весь `unsafe` — в модуле `gl_calls`, каждый блок с `// SAFETY:`. До драйвера проверяется,
//! сколько байт прочитает каждая загрузка.
//!
//! # Границы зависимостей
//! Из workspace — только `gles2-egui`: это проверяет `scripts/check-refactor-guardrails.py`.

// `unsafe` запрещён везде, кроме `gl_calls` (там разрешён явно и с `// SAFETY:`).
#![deny(unsafe_code)]

mod color;
mod draw_uniforms;
mod geometry;
#[allow(
    unsafe_code,
    reason = "модуль безопасности вызовов GL видеослоя; каждый блок с // SAFETY:"
)]
mod gl_calls;
mod layer_api;
mod planes;
mod program;
mod renderer;
mod ring;
mod textures;
mod underlay;
mod upload_plan;

pub use color::{ColorAdjustment, YuvColorEncoding, YuvMatrix, YuvRange};
pub use geometry::{PictureMapping, PixelRect, QuarterTurn, RectOutOfRange};
pub use gl_calls::{PlaneTextureFormat, VideoGlError};
pub use layer_api::{
    PlaneFormatPreference, RowLengthPreference, UploadOutcome, UploadReport, VideoDrawError,
    VideoDrawOutcome, VideoDrawRequest, VideoLayerError, VideoLayerOptions, VideoLayerStats,
    VideoUploadError,
};
pub use planes::{
    ChromaSubsampling, DmaBufPlane, FrameLayoutError, HostRows, PlanarFrameUpload, PlanarLayout,
    PlaneExtent, PlaneRole, PlaneSource, PlaneSourceError, PlaneSourceKind, SampleDepth,
    VideoFrameKey,
};
pub use program::ShaderPrecision;
pub use renderer::VideoLayerRenderer;
pub use ring::{BusyReason, UploadSlotCount};
pub use underlay::VideoUnderlay;
