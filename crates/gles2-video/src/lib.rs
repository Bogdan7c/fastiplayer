//! Видеослой своего рендера OpenGL ES 2.0 для старого железа (issue #6, план
//! `user/gles2-renderer/`, сессия 05).
//!
//! Переиспользуемый crate: подключается к любому приложению на `gles2-egui` и не знает ни одного
//! crate-а плеера. Свой словарь кадра ([`PlanarFrameUpload`], [`PlaneSource`]) — перевод
//! дескрипторов декодера плеера в него делает адаптер (`render-gles2-shell`).
//!
//! # Что здесь есть
//! - планарный YUV 8 бит 4:2:0 / 4:2:2 / 4:4:4 и 10/12 бит (4:4:4 — 10 бит, матрица v1 wgpu)
//!   из памяти процесса с **ровно одной копией**
//!   (драйвер копирует строки в текстуру; без `GL_EXT_unpack_subimage` — текстура шириной в
//!   stride, хвост строки обрезает шейдер, без переупаковки; 10/12 бит — пара байтов LE в
//!   `GL_LUMINANCE_ALPHA` как есть, код собирает шейдер, нужен `highp`);
//! - источник плоскостей сразу допускает 0 копий ([`PlaneSource::DmaBuf`], реализуют сессии 07
//!   и 12);
//! - аппаратный NV12 из DMA-BUF без копии (сессия 07): [`DmaBufNv12Frame`] →
//!   [`VideoLayerRenderer::import_dma_buf_frame`], кэш импортов по [`DmaBufBufferIdentity`],
//!   контракт с производителем — [`DmaBufSharingContract`]; только 8 бит (решение владельца 33);
//! - кольцо слотов текстур ([`UploadSlotCount`], по умолчанию 3): запись только в слот, который
//!   GPU больше не читает (fence кадра); нет слота — [`UploadOutcome::Busy`], кольцо не растёт;
//! - перевод YUV → RGB (BT.601/BT.709, limited/full), цветокоррекция, поворот, letterbox и
//!   исключения (сайдбар) — строгий паритет с wgpu-рендером плеера;
//! - HDR → SDR BT.2446-C для 10/12 бит ([`HdrToSdrConversion`]): формула wgpu в `f64` на CPU в
//!   узлах таблицы 65³, шейдер смешивает узлы (решение владельца сессии 06 ради старого железа;
//!   допуск и замер — модуль `hdr`);
//! - подложка кадра [`VideoUnderlay`] для `gles2_egui::FrameCompositor`;
//! - слой bitmap-оверлеев поверх видео (сессия 08): маски покрытия + цвет и RGBA-картинки с
//!   премультиплицированной альфой, два атласа, две группы сдвига ([`BitmapOverlayRenderer`],
//!   трейт картинки [`OverlayBitmap`]) — субтитры плеера, титры редактора.
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
mod cpu_write_probe;
mod dma_buf_api;
mod dma_buf_cache;
mod dma_buf_frame;
mod dma_buf_import;
mod dma_buf_planar_frame;
mod dma_buf_planar_import;
mod draw_uniforms;
mod frame_color;
mod geometry;
#[allow(
    unsafe_code,
    reason = "модуль безопасности вызовов GL видеослоя; каждый блок с // SAFETY:"
)]
mod gl_calls;
mod hdr;
mod hdr_table_builder;
mod hdr_table_texture;
mod layer_api;
mod overlay;
mod planes;
mod program;
mod renderer;
mod ring;
mod textures;
mod underlay;
mod upload_plan;

pub use color::{ColorAdjustment, YuvColorEncoding, YuvMatrix, YuvRange};
pub use cpu_write_probe::{
    CPU_WRITE_PROBE_MIN_BUFFER_BYTES, CpuWriteProbeError, CpuWriteProbeProducer,
    CpuWriteVisibility, probe_cpu_write_visibility,
};
pub use dma_buf_api::{
    CachedDmaBufFrame, DmaBufImportOutcome, DmaBufImportReport, PlanarDmaBufImportOutcome,
    PlanarDmaBufImportReport, VideoImportError,
};
pub use dma_buf_cache::{DmaBufImportCacheCapacity, DmaBufImportCacheSnapshot};
pub use dma_buf_frame::{
    DmaBufBufferIdentity, DmaBufFrameError, DmaBufFrameFormat, DmaBufNv12Frame,
    DmaBufObjectIdentity, DmaBufPlaneArrangement, DmaBufPlaneRole, DmaBufSharingContract,
};
pub use dma_buf_planar_frame::{
    DmaBufPlanarFrame, DmaBufPlanarFrameError, DmaBufPlanarIdentity, DmaBufPlaneLocation,
};
pub use geometry::{PictureMapping, PixelRect, QuarterTurn, RectOutOfRange};
pub use gl_calls::{PlaneTextureFormat, VideoGlError};
pub use hdr::{
    HdrLevelsError, HdrReferenceLevels, HdrToSdrConversion, HdrTransfer, bt2446c_to_sdr,
};
pub use layer_api::{
    PlaneFormatPreference, RowLengthPreference, UploadOutcome, UploadReport, VideoColorConversion,
    VideoDrawError, VideoDrawOutcome, VideoDrawRequest, VideoLayerError, VideoLayerOptions,
    VideoLayerStats, VideoUploadError,
};
/// Константы формулы BT.2446-C (тот же набор, что в шейдере wgpu плеера): для сверки констант
/// и для своих CPU-расчётов вызывающего.
pub mod bt2446c_constants {
    pub use crate::hdr::{
        BT1886_REFERENCE_GAMMA, BT2020_KB, BT2020_KG, BT2020_KR, BT2020_LINEAR_RGB_TO_XYZ,
        BT2446C_CROSSTALK_ALPHA, BT2446C_K1, BT2446C_K2, BT2446C_K3, BT2446C_K4,
        BT2446C_SDR_INFLECTION_NITS, HLG_A, HLG_B, HLG_C, HLG_REFERENCE_SYSTEM_GAMMA, PQ_C1, PQ_C2,
        PQ_C3, PQ_M1, PQ_M2, PQ_REFERENCE_PEAK_NITS, XYZ_TO_BT709_LINEAR_RGB,
    };
}
pub use overlay::{
    AtlasSideLimit, BitmapOverlayRenderer, OverlayAtlasOutcome, OverlayAtlasStats, OverlayBitmap,
    OverlayBitmapRect, OverlayCanvasOrigins, OverlayDrawError, OverlayDrawOutcome,
    OverlayDrawRequest, OverlayLayerError, OverlayLayerOptions, OverlayLayerStats,
    OverlayPixelFormat, OverlayPixels, OverlayPlacementGroup, OverlayPrepareError,
    OverlayPrepareReport, OverlayRowUploadPreference, OverlayTint,
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
