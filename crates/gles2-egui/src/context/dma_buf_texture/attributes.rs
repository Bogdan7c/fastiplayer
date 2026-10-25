//! Чистая сборка списка атрибутов `eglCreateImage` для одной плоскости DMA-BUF.
//!
//! Без драйвера и без GL: вход — описание плоскости, выход — список `EGLAttrib`, оканчивающийся
//! `EGL_NONE`. Поэтому раскладку проверяют hermetic-тесты.
//!
//! Спецификации: `EGL_EXT_image_dma_buf_import` (атрибуты плоскости 0, значения — `EGLint`) и
//! `EGL_EXT_image_dma_buf_import_modifiers` (модификатор — парой LO/HI по 32 бита; одиночный
//! LO или HI даёт `EGL_BAD_PARAMETER`). Значения констант сверены с `eglext.h` Khronos.

use std::os::fd::AsRawFd;

use super::{DmaBufAttribute, DmaBufPlaneTexture, ModifierAttribute};

/// `EGL_WIDTH` (EGL 1.5, таблица 3.x «атрибуты eglCreateImage»).
const EGL_WIDTH: usize = 0x3057;
/// `EGL_HEIGHT`.
const EGL_HEIGHT: usize = 0x3056;
/// `EGL_LINUX_DRM_FOURCC_EXT`.
const EGL_LINUX_DRM_FOURCC_EXT: usize = 0x3271;
/// `EGL_DMA_BUF_PLANE0_FD_EXT`.
const EGL_DMA_BUF_PLANE0_FD_EXT: usize = 0x3272;
/// `EGL_DMA_BUF_PLANE0_OFFSET_EXT`.
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: usize = 0x3273;
/// `EGL_DMA_BUF_PLANE0_PITCH_EXT`.
const EGL_DMA_BUF_PLANE0_PITCH_EXT: usize = 0x3274;
/// `EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT`.
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: usize = 0x3443;
/// `EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT`.
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: usize = 0x3444;
/// `EGL_NONE` как `EGLAttrib` — терминатор списка.
const EGL_NONE_ATTRIB: usize = 0x3038;

/// Готовый список атрибутов `eglCreateImage` (пары «имя, значение» и `EGL_NONE` в конце).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DmaBufImageAttributes {
    attributes: Vec<usize>,
    modifier: ModifierAttribute,
}

impl DmaBufImageAttributes {
    /// Сырой список для `eglCreateImage`.
    pub(crate) fn as_slice(&self) -> &[usize] {
        &self.attributes
    }

    /// Передан ли модификатор буфера драйверу.
    pub(crate) fn modifier(&self) -> ModifierAttribute {
        self.modifier
    }

    /// Собирает список. Значения, не помещающиеся в `EGLint`, — типизированная ошибка ДО
    /// обращения к драйверу: молчаливое усечение дало бы чужой размер или смещение.
    pub(crate) fn build(plane: &DmaBufPlaneTexture<'_>) -> Result<Self, DmaBufAttribute> {
        let width = egl_int(plane.width, DmaBufAttribute::Width)?;
        let height = egl_int(plane.height, DmaBufAttribute::Height)?;
        let offset = egl_int(plane.offset, DmaBufAttribute::Offset)?;
        let pitch = egl_int(plane.pitch, DmaBufAttribute::Pitch)?;
        // Дескриптор открытого файла неотрицателен, но проверяем явно, а не приводим «на веру».
        let fd = usize::try_from(plane.fd.as_raw_fd()).map_err(|_| DmaBufAttribute::Fd)?;

        let mut attributes = vec![
            EGL_WIDTH,
            width,
            EGL_HEIGHT,
            height,
            EGL_LINUX_DRM_FOURCC_EXT,
            // fourcc — битовый шаблон `uint32`; EGL читает его как `EGLint`, но берёт те же 32 бита.
            plane.fourcc.0 as usize,
            EGL_DMA_BUF_PLANE0_FD_EXT,
            fd,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT,
            offset,
            EGL_DMA_BUF_PLANE0_PITCH_EXT,
            pitch,
        ];
        // Правило проекта: модификатор передаётся ВСЕГДА, когда он известен. Без него драйвер
        // выбирает раскладку сам, и на тайловых буферах (AMD) это тихий мусор вместо ошибки.
        let modifier = if plane.modifier.is_invalid() {
            ModifierAttribute::DriverChosen
        } else {
            let raw = plane.modifier.0;
            attributes.extend([
                EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
                // Младшие 32 бита: усечение здесь намеренное и точное (маска).
                (raw & 0xffff_ffff) as usize,
                EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
                (raw >> 32) as usize,
            ]);
            ModifierAttribute::Explicit(plane.modifier)
        };
        attributes.push(EGL_NONE_ATTRIB);
        Ok(Self {
            attributes,
            modifier,
        })
    }
}

/// `u32` → значение `EGLint` (`≤ i32::MAX`) в виде `EGLAttrib`.
fn egl_int(value: u32, attribute: DmaBufAttribute) -> Result<usize, DmaBufAttribute> {
    i32::try_from(value)
        .ok()
        .and_then(|checked| usize::try_from(checked).ok())
        .ok_or(attribute)
}
