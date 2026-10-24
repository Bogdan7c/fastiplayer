//! Импорт DMA-BUF: какие форматы и модификаторы драйвер принимает в `EGLImage`.

use std::fmt;

use super::ExtensionSet;
use crate::context::driver::{DisplayId, DmaBufModifier, EglDriver};

/// Код формата DRM (fourcc, `drm_fourcc.h`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DrmFourcc(pub u32);

impl DrmFourcc {
    /// `DRM_FORMAT_R8` — одна 8-битная плоскость (яркость NV12).
    pub const R8: Self = Self::from_code(*b"R8  ");
    /// `DRM_FORMAT_GR88` — две 8-битные компоненты (цветность NV12, яркость P010 парами байтов).
    pub const GR88: Self = Self::from_code(*b"GR88");
    /// `DRM_FORMAT_ABGR8888` — четыре 8-битные компоненты (цветность P010 парами байтов).
    pub const ABGR8888: Self = Self::from_code(*b"AB24");

    /// Fourcc из четырёх символов (младший байт — первый символ, как `fourcc_code` в DRM).
    pub const fn from_code(code: [u8; 4]) -> Self {
        Self(u32::from_le_bytes(code))
    }
}

impl fmt::Debug for DrmFourcc {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.0.to_le_bytes();
        if bytes
            .iter()
            .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
        {
            let text: String = bytes.iter().map(|byte| char::from(*byte)).collect();
            write!(formatter, "DrmFourcc({:?})", text.trim_end())
        } else {
            write!(formatter, "DrmFourcc({:#010x})", self.0)
        }
    }
}

/// Как формат можно читать в шейдере после импорта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatSampling {
    /// Хотя бы один модификатор импортируется как обычная текстура `sampler2D`.
    Sampler2D,
    /// Все модификаторы только `GL_TEXTURE_EXTERNAL_OES` (так у NV12/P010 целиком на T410).
    ExternalOnly,
    /// Драйвер не назвал ни одного модификатора: возможен только импорт без атрибутов
    /// модификатора, а на тайловых буферах это тихий мусор (разведка 00). Не считаем годным.
    ImplicitModifierOnly,
}

/// Один формат импорта и его модификаторы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmaBufFormatSupport {
    /// Формат.
    pub fourcc: DrmFourcc,
    /// Модификаторы (`eglQueryDmaBufModifiersEXT`).
    pub modifiers: Vec<DmaBufModifier>,
}

impl DmaBufFormatSupport {
    /// Как этот формат читается в шейдере.
    pub fn sampling(&self) -> FormatSampling {
        if self.modifiers.is_empty() {
            FormatSampling::ImplicitModifierOnly
        } else if self
            .modifiers
            .iter()
            .any(|modifier| !modifier.external_only)
        {
            FormatSampling::Sampler2D
        } else {
            FormatSampling::ExternalOnly
        }
    }
}

/// Импорт DMA-BUF у драйвера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmaBufImportSupport {
    /// Нет `EGL_EXT_image_dma_buf_import`.
    Unavailable,
    /// Импорт есть, но форматы и модификаторы спросить нельзя
    /// (нет `EGL_EXT_image_dma_buf_import_modifiers`).
    WithoutFormatQuery,
    /// Форматы и модификаторы запрошены у драйвера.
    Queried {
        /// Форматы в порядке ответа драйвера.
        formats: Vec<DmaBufFormatSupport>,
    },
    /// Расширения есть, но запрос форматов отказал — честно сообщаем, а не считаем «пусто».
    QueryFailed {
        /// Текст отказа драйвера.
        message: String,
    },
}

impl DmaBufImportSupport {
    /// Как читается формат: `None` — драйвер его не назвал (или форматы неизвестны).
    pub fn sampling_of(&self, fourcc: DrmFourcc) -> Option<FormatSampling> {
        match self {
            Self::Queried { formats } => formats
                .iter()
                .find(|format| format.fourcc == fourcc)
                .map(DmaBufFormatSupport::sampling),
            Self::Unavailable | Self::WithoutFormatQuery | Self::QueryFailed { .. } => None,
        }
    }
}

/// Форматы и модификаторы импорта DMA-BUF.
pub(crate) fn query_support(
    driver: &dyn EglDriver,
    display: DisplayId,
    display_extensions: &ExtensionSet,
) -> DmaBufImportSupport {
    if !display_extensions.contains("EGL_EXT_image_dma_buf_import") {
        return DmaBufImportSupport::Unavailable;
    }
    if !display_extensions.contains("EGL_EXT_image_dma_buf_import_modifiers") {
        return DmaBufImportSupport::WithoutFormatQuery;
    }
    let formats = match driver.query_dma_buf_formats(display) {
        Ok(formats) => formats,
        Err(error) => {
            return DmaBufImportSupport::QueryFailed {
                message: error.to_string(),
            };
        }
    };
    let mut supported = Vec::with_capacity(formats.len());
    for fourcc in formats {
        match driver.query_dma_buf_modifiers(display, fourcc) {
            Ok(modifiers) => supported.push(DmaBufFormatSupport {
                fourcc: DrmFourcc(fourcc),
                modifiers,
            }),
            Err(error) => {
                return DmaBufImportSupport::QueryFailed {
                    message: format!("{:?}: {error}", DrmFourcc(fourcc)),
                };
            }
        }
    }
    DmaBufImportSupport::Queried { formats: supported }
}
