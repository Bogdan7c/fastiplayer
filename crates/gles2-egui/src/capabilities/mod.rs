//! Что реально умеет драйвер: [`Gles2Capabilities`] и уровни из таблицы README плана.
//!
//! Строгий ES 2.0 (разведка 00): radeonsi и llvmpipe на запрос «GLES 2» отдают контекст ES 3.2.
//! Возможности ES 3 (целочисленные текстуры, `texelFetch`, `GL_RED` без `GL_EXT_texture_rg`,
//! `UNPACK_ROW_LENGTH` без `GL_EXT_unpack_subimage`, `#version 300 es`) НЕ учитываются нигде:
//! всё выводится из ES 2.0 и строк расширений. Иначе тесты на 780M и в CI зелёные, а на T410
//! чёрный экран. Эвристик по имени GPU нет.

pub(crate) mod dma_buf;
mod extensions;
mod tiers;

#[cfg(test)]
mod tests;

pub use dma_buf::{DmaBufFormatSupport, DmaBufImportSupport, DrmFourcc, FormatSampling};
pub use extensions::ExtensionSet;
pub use tiers::{CapabilityTiers, Conveniences, MissingRequirement, TierStatus};

use crate::context::driver::EglVersion;
use crate::context::gles_version::GlesVersion;

/// Сырые ответы драйвера, из которых строятся возможности. Собирает `Gles2Context` на живом
/// контексте; тесты подставляют строки нужной машины.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DriverReport {
    /// `GL_VERSION`.
    pub(crate) gl_version_string: String,
    /// Разобранная версия ES (у radeonsi — 3.2 на запрос 2.0).
    pub(crate) reported_gles_version: GlesVersion,
    /// `GL_VENDOR`.
    pub(crate) gl_vendor: String,
    /// `GL_RENDERER`.
    pub(crate) gl_renderer: String,
    /// `GL_SHADING_LANGUAGE_VERSION`.
    pub(crate) shading_language_version: String,
    /// Расширения GL.
    pub(crate) gl_extensions: ExtensionSet,
    /// Версия EGL display.
    pub(crate) egl_version: EglVersion,
    /// `EGL_VENDOR`.
    pub(crate) egl_vendor: String,
    /// Клиентские расширения EGL (платформы).
    pub(crate) egl_client_extensions: ExtensionSet,
    /// Расширения EGL display.
    pub(crate) egl_display_extensions: ExtensionSet,
    /// `GL_MAX_TEXTURE_SIZE`.
    pub(crate) max_texture_size: u32,
    /// Точность `highp float` во фрагментном шейдере (`glGetShaderPrecisionFormat`), 0 — нет.
    pub(crate) fragment_high_float_precision_bits: u32,
    /// Импорт DMA-BUF по `eglQueryDmaBuf*EXT`.
    pub(crate) dma_buf: DmaBufImportSupport,
}

/// Что умеет драйвер текущего контекста.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gles2Capabilities {
    report: DriverReport,
    tiers: CapabilityTiers,
}

impl Gles2Capabilities {
    /// Возможности из ответов драйвера.
    pub(crate) fn from_report(report: DriverReport) -> Self {
        let tiers = CapabilityTiers::derive(&report);
        Self { report, tiers }
    }

    /// Уровни возможностей (таблица README плана).
    pub fn tiers(&self) -> &CapabilityTiers {
        &self.tiers
    }

    /// Версия ES, которую сообщил драйвер. Используется только для диагностики: рендер
    /// всегда работает в объёме ES 2.0.
    pub fn reported_gles_version(&self) -> GlesVersion {
        self.report.reported_gles_version
    }

    /// `GL_VERSION`.
    pub fn gl_version_string(&self) -> &str {
        &self.report.gl_version_string
    }

    /// `GL_VENDOR`.
    pub fn gl_vendor(&self) -> &str {
        &self.report.gl_vendor
    }

    /// `GL_RENDERER`.
    pub fn gl_renderer(&self) -> &str {
        &self.report.gl_renderer
    }

    /// `GL_SHADING_LANGUAGE_VERSION`.
    pub fn shading_language_version(&self) -> &str {
        &self.report.shading_language_version
    }

    /// Расширения GL.
    pub fn gl_extensions(&self) -> &ExtensionSet {
        &self.report.gl_extensions
    }

    /// Версия EGL display.
    pub fn egl_version(&self) -> EglVersion {
        self.report.egl_version
    }

    /// `EGL_VENDOR`.
    pub fn egl_vendor(&self) -> &str {
        &self.report.egl_vendor
    }

    /// Клиентские расширения EGL.
    pub fn egl_client_extensions(&self) -> &ExtensionSet {
        &self.report.egl_client_extensions
    }

    /// Расширения EGL display.
    pub fn egl_display_extensions(&self) -> &ExtensionSet {
        &self.report.egl_display_extensions
    }

    /// `GL_MAX_TEXTURE_SIZE`.
    pub fn max_texture_size(&self) -> u32 {
        self.report.max_texture_size
    }

    /// Бит точности `highp float` во фрагментном шейдере (0 — highp нет).
    pub fn fragment_high_float_precision_bits(&self) -> u32 {
        self.report.fragment_high_float_precision_bits
    }

    /// Импорт DMA-BUF.
    pub fn dma_buf(&self) -> &DmaBufImportSupport {
        &self.report.dma_buf
    }
}
