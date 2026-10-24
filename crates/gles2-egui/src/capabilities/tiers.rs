//! Уровни возможностей GLES2 (таблица «Уровни возможностей» в README плана).
//!
//! | Уровень | Требует |
//! |---|---|
//! | Базовый | GLES 2.0, EGL ≥ 1.5 (решение владельца 03; README называл 1.4) |
//! | Точность | `highp float` во фрагментном шейдере |
//! | Zero-copy | `EGL_EXT_image_dma_buf_import` + `_modifiers` + `GL_OES_EGL_image` + `R8` и `GR88` как `sampler2D` |
//! | Zero-copy 10-бит | zero-copy + точность + `GR88` и `ABGR8888` как `sampler2D` |
//! | Удобства | каждое отдельно: `unpack_subimage`, `element_index_uint`, VAO, `KHR_fence_sync`, `buffer_age` |
//!
//! Модификаторы обязательны для zero-copy: без них тайловые буферы импортируются тихим мусором
//! (разведка 00, 780M).

use super::DriverReport;
use super::dma_buf::{DrmFourcc, FormatSampling};
use crate::context::driver::EglVersion;
use crate::context::gles_version::GlesVersion;

/// Чего не хватило уровню.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissingRequirement {
    /// Версия ES ниже 2.0.
    GlesVersion {
        /// Что сообщил драйвер.
        found: GlesVersion,
    },
    /// Версия EGL ниже 1.5.
    EglVersion {
        /// Что сообщил display.
        found: EglVersion,
    },
    /// Нет `highp float` во фрагментном шейдере.
    HighPrecisionFragmentFloat,
    /// Нет расширения GL.
    GlExtension(&'static str),
    /// Нет расширения EGL display.
    EglExtension(&'static str),
    /// Формат DMA-BUF нельзя читать как `sampler2D`.
    DmaBufFormat {
        /// Формат.
        fourcc: DrmFourcc,
        /// Что ответил драйвер (`None` — формат не назван).
        sampling: Option<FormatSampling>,
    },
}

/// Состояние одного уровня.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TierStatus {
    /// Все требования выполнены.
    Available,
    /// Чего не хватает (не пусто).
    Missing(Vec<MissingRequirement>),
}

impl TierStatus {
    fn from_missing(missing: Vec<MissingRequirement>) -> Self {
        if missing.is_empty() {
            Self::Available
        } else {
            Self::Missing(missing)
        }
    }

    /// Уровень доступен.
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Удобства: без каждого есть описанный фолбэк, поэтому они не уровень «всё или ничего».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conveniences {
    /// `GL_EXT_unpack_subimage`: загрузка плоскости с шагом строк без перепаковки.
    /// Без него — текстура шириной в stride + обрезка в шейдере.
    pub unpack_subimage: bool,
    /// `GL_OES_element_index_uint`: 32-битные индексы мешей egui. Без него — батчи по u16.
    pub element_index_uint: bool,
    /// `GL_OES_vertex_array_object`: меньше вызовов на привязку атрибутов.
    pub vertex_array_object: bool,
    /// `EGL_KHR_fence_sync`: «GPU дочитал кадр» без `glFinish`.
    pub egl_fence_sync: bool,
    /// `EGL_EXT_buffer_age`: возраст заднего буфера (частичная перерисовка в будущем).
    pub buffer_age: bool,
}

/// Все уровни сразу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityTiers {
    /// Интерфейс, SW 8-бит, субтитры, углы.
    pub baseline: TierStatus,
    /// SW 10/12-бит, HDR→SDR.
    pub precision: TierStatus,
    /// VA-API NV12 без копий.
    pub zero_copy: TierStatus,
    /// VA-API P010 без копий.
    pub zero_copy_10bit: TierStatus,
    /// Удобства.
    pub conveniences: Conveniences,
}

/// Минимальная версия EGL (решение владельца, сессия 03).
const MINIMUM_EGL_VERSION: EglVersion = EglVersion { major: 1, minor: 5 };

impl CapabilityTiers {
    /// Уровни из ответов драйвера.
    pub(crate) fn derive(report: &DriverReport) -> Self {
        let baseline = TierStatus::from_missing(baseline_missing(report));
        let precision = TierStatus::from_missing(precision_missing(report));
        let zero_copy_missing = zero_copy_missing(report);
        let zero_copy_10bit =
            TierStatus::from_missing(zero_copy_10bit_missing(report, &zero_copy_missing));
        Self {
            baseline,
            precision,
            zero_copy: TierStatus::from_missing(zero_copy_missing),
            zero_copy_10bit,
            conveniences: conveniences(report),
        }
    }
}

fn baseline_missing(report: &DriverReport) -> Vec<MissingRequirement> {
    let mut missing = Vec::new();
    if report.reported_gles_version < GlesVersion::ES_2_0 {
        missing.push(MissingRequirement::GlesVersion {
            found: report.reported_gles_version,
        });
    }
    if report.egl_version < MINIMUM_EGL_VERSION {
        missing.push(MissingRequirement::EglVersion {
            found: report.egl_version,
        });
    }
    missing
}

fn precision_missing(report: &DriverReport) -> Vec<MissingRequirement> {
    if report.fragment_high_float_precision_bits > 0 {
        Vec::new()
    } else {
        vec![MissingRequirement::HighPrecisionFragmentFloat]
    }
}

fn zero_copy_missing(report: &DriverReport) -> Vec<MissingRequirement> {
    let mut missing = Vec::new();
    for extension in [
        "EGL_EXT_image_dma_buf_import",
        "EGL_EXT_image_dma_buf_import_modifiers",
    ] {
        if !report.egl_display_extensions.contains(extension) {
            missing.push(MissingRequirement::EglExtension(extension));
        }
    }
    if !report.gl_extensions.contains("GL_OES_EGL_image") {
        missing.push(MissingRequirement::GlExtension("GL_OES_EGL_image"));
    }
    push_missing_sampler_formats(report, &[DrmFourcc::R8, DrmFourcc::GR88], &mut missing);
    missing
}

fn zero_copy_10bit_missing(
    report: &DriverReport,
    zero_copy_missing: &[MissingRequirement],
) -> Vec<MissingRequirement> {
    let mut missing = zero_copy_missing.to_vec();
    missing.extend(precision_missing(report));
    // GR88 уже проверен уровнем zero-copy; добавляем то, что нужно только P010.
    push_missing_sampler_formats(report, &[DrmFourcc::ABGR8888], &mut missing);
    missing
}

fn push_missing_sampler_formats(
    report: &DriverReport,
    formats: &[DrmFourcc],
    missing: &mut Vec<MissingRequirement>,
) {
    for &fourcc in formats {
        let sampling = report.dma_buf.sampling_of(fourcc);
        if sampling != Some(FormatSampling::Sampler2D) {
            missing.push(MissingRequirement::DmaBufFormat { fourcc, sampling });
        }
    }
}

fn conveniences(report: &DriverReport) -> Conveniences {
    Conveniences {
        unpack_subimage: report.gl_extensions.contains("GL_EXT_unpack_subimage"),
        element_index_uint: report.gl_extensions.contains("GL_OES_element_index_uint"),
        vertex_array_object: report.gl_extensions.contains("GL_OES_vertex_array_object"),
        egl_fence_sync: report.egl_display_extensions.contains("EGL_KHR_fence_sync"),
        buffer_age: report.egl_display_extensions.contains("EGL_EXT_buffer_age"),
    }
}
