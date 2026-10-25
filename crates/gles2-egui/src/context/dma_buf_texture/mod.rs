//! Импорт одной плоскости DMA-BUF как хранилища GL-текстуры (сессия 07 плана gles2-renderer).
//!
//! Цепочка: `EGL_EXT_image_dma_buf_import` → `EGLImage` → `glEGLImageTargetTexture2DOES`
//! (`GL_OES_EGL_image`). Это «zero-copy»: текстура ссылается на память буфера, копии пикселей нет.
//!
//! Модули:
//! - `attributes` — чистая сборка списка атрибутов `eglCreateImage`;
//! - `attach` — оркестрация шагов над трейтами драйвера EGL и GL (hermetic-тесты);
//! - этот файл — публичные типы и метод [`Gles2Context::attach_dma_buf_plane_to_texture`].
//!
//! Почему ОДНА плоскость = ОДИН `EGLImage`: NV12 приходит двумя плоскостями разного формата
//! (R8 для яркости, GR88 для цветности). Каждая читается своей `sampler2D`-текстурой без
//! `GL_TEXTURE_EXTERNAL_OES`, а перевод YUV→RGB делает наш шейдер. Многоплоскостной образ NV12
//! драйверы отдают только как external-текстуру, которая нам не подходит.

mod attach;
mod attributes;
#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::os::fd::BorrowedFd;

use self::attach::{ImageEgl, ImportCapabilities, TextureImageTarget, attach_plane_to_texture};
pub(crate) use self::attributes::DmaBufImageAttributes;
use super::driver::{EglCallError, ImageId};
use super::safety::egl_image::ImageTargetTexture;
use super::safety::gl::CurrentGl;
use super::{GlAccessError, Gles2Context};
use crate::capabilities::{DrmFourcc, DrmModifier, ModifierSampling};
use crate::loss::ContextHealth;

/// Плоскость DMA-BUF, которую нужно сделать хранилищем текстуры.
///
/// Владение fd: структура только ЗАИМСТВУЕТ его (`BorrowedFd`). EGL fd не забирает
/// (`EGL_EXT_image_dma_buf_import`, issue 3), поэтому закрывает его вызывающий — и при успехе,
/// и при ошибке. После успешного вызова fd можно закрыть сразу: буфер держит сама текстура.
#[derive(Debug, Clone, Copy)]
pub struct DmaBufPlaneTexture<'fd> {
    /// Формат плоскости: [`DrmFourcc::R8`] или [`DrmFourcc::GR88`] для NV12.
    pub fourcc: DrmFourcc,
    /// Ширина плоскости в пикселях (≤ `i32::MAX`).
    pub width: u32,
    /// Высота плоскости в пикселях (≤ `i32::MAX`).
    pub height: u32,
    /// DMA-BUF fd буфера.
    pub fd: BorrowedFd<'fd>,
    /// Смещение плоскости от начала буфера в байтах (≤ `i32::MAX`).
    pub offset: u32,
    /// Шаг строки в байтах (≤ `i32::MAX`).
    pub pitch: u32,
    /// Модификатор буфера. Не [`DrmModifier::INVALID`] — передаётся драйверу обязательно
    /// (нужно `EGL_EXT_image_dma_buf_import_modifiers`); `INVALID` — модификатор выберет
    /// реализация, и атрибуты модификатора не передаются.
    pub modifier: DrmModifier,
}

/// Какой атрибут не удалось передать драйверу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaBufAttribute {
    /// Ширина не помещается в `EGLint`.
    Width,
    /// Высота не помещается в `EGLint`.
    Height,
    /// Смещение не помещается в `EGLint`.
    Offset,
    /// Шаг строки не помещается в `EGLint`.
    Pitch,
    /// Дескриптор fd отрицателен.
    Fd,
}

/// Чего не хватает драйверу для импорта DMA-BUF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaBufMissingRequirement {
    /// Нет `GL_OES_EGL_image`.
    GlOesEglImage,
    /// Нет `EGL_EXT_image_dma_buf_import`.
    EglExtImageDmaBufImport,
    /// Нет `EGL_EXT_image_dma_buf_import_modifiers`, а модификатор буфера известен.
    EglExtImageDmaBufImportModifiers,
}

/// Как модификатор попал к драйверу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierAttribute {
    /// Передан явно (атрибуты `MODIFIER_LO/HI`).
    Explicit(DrmModifier),
    /// Не передан (`DRM_FORMAT_MOD_INVALID`): раскладку выбрал драйвер.
    DriverChosen,
}

/// Что стало с `EGLImage` после привязки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageRelease {
    /// `eglDestroyImage` успешен.
    Released,
    /// `eglDestroyImage` отказал. Текстура при этом валидна (привязка уже удалась), поэтому это
    /// не ошибка вызова, а сигнал возможной утечки: он же пишется в лог и в
    /// [`DmaBufTextureStats::image_destroy_failures`].
    DestroyFailed(EglCallError),
}

/// Итог успешной привязки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaBufTextureAttached {
    /// Как был передан модификатор.
    pub modifier: ModifierAttribute,
    /// Судьба `EGLImage`.
    pub image_release: ImageRelease,
}

/// Почему плоскость не стала хранилищем текстуры.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DmaBufTextureError {
    /// GL недоступен (потеря контекста, окно отпущено).
    #[error(transparent)]
    ContextUnavailable(#[from] GlAccessError),
    /// Драйвер не умеет импорт DMA-BUF: честный отказ, копии нет.
    #[error("DMA-BUF import is unsupported: missing {missing:?}")]
    ZeroCopyUnsupported {
        /// Чего не хватает.
        missing: DmaBufMissingRequirement,
    },
    /// `glEGLImageTargetTexture2DOES` не загрузилась, хотя `GL_OES_EGL_image` заявлено.
    #[error("glEGLImageTargetTexture2DOES is not available in the driver")]
    ImageTargetFunctionMissing,
    /// Драйвер не объявлял пару «формат + модификатор» как `sampler2D`.
    #[error("{fourcc:?} with {modifier:?} is not sampleable as sampler2D: {sampling:?}")]
    FormatModifierNotSampleable {
        /// Формат плоскости.
        fourcc: DrmFourcc,
        /// Модификатор буфера.
        modifier: DrmModifier,
        /// Что сказал драйвер.
        sampling: ModifierSampling,
    },
    /// Значение не помещается в `EGLint`.
    #[error("{attribute:?} does not fit into EGLint")]
    AttributeOutOfRange {
        /// Какой атрибут.
        attribute: DmaBufAttribute,
    },
    /// `eglCreateImage` отказал (в том числе `EGL_BAD_MATCH` на неподходящем формате или
    /// `EGL_BAD_ALLOC`). Потеря контекста запоминается: следующие вызовы вернут
    /// [`Self::ContextUnavailable`].
    #[error("eglCreateImage failed: {0}")]
    ImageCreation(EglCallError),
    /// `glEGLImageTargetTexture2DOES` оставил флаг ошибки GL (обычно `GL_INVALID_OPERATION`:
    /// формат образа не подходит текстуре).
    #[error("glEGLImageTargetTexture2DOES left GL error {gl_error:#06x}")]
    TextureTarget {
        /// Код `glGetError`.
        gl_error: u32,
    },
}

/// Счётчики импорта DMA-BUF контекстом.
///
/// Инвариант при исправном драйвере: `images_created == images_destroyed +
/// image_destroy_failures` после каждого вызова (ни один образ не живёт между вызовами).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DmaBufTextureStats {
    /// Сколько `EGLImage` создано.
    pub images_created: u64,
    /// Сколько уничтожено успешно.
    pub images_destroyed: u64,
    /// Сколько `eglDestroyImage` отказали.
    pub image_destroy_failures: u64,
    /// Сколько вызовов привязки вернули ошибку (любую).
    pub attach_failures: u64,
    /// Сколько вызовов привязки удались.
    pub attaches_succeeded: u64,
}

/// Сторона GL реального контекста.
struct ContextImageTarget<'a> {
    gl: CurrentGl<'a>,
    function: Option<&'a ImageTargetTexture>,
}

impl TextureImageTarget for ContextImageTarget<'_> {
    fn image_target_function_loaded(&self) -> bool {
        self.function.is_some()
    }

    fn bind_texture_2d(&self, texture: Option<glow::NativeTexture>) {
        self.gl.bind_texture_2d(texture);
    }

    fn target_bound_texture_from_image(&self, image: ImageId) {
        if let Some(function) = self.function {
            function.target_bound_texture_2d(self.gl, image);
        }
    }

    fn take_error_flag(&self) -> u32 {
        self.gl.take_error_flag()
    }
}

impl Gles2Context {
    /// Делает одну плоскость DMA-BUF хранилищем GL-текстуры `texture` (без копии пикселей).
    ///
    /// Шаги: контекст жив → расширения (`GL_OES_EGL_image`, `EGL_EXT_image_dma_buf_import`,
    /// `_modifiers` для явного модификатора) → пара «формат + модификатор» объявлена драйвером
    /// как `sampler2D` → `eglCreateImage` → `glEGLImageTargetTexture2DOES` → `eglDestroyImage`.
    ///
    /// `EGLImage` живёт только внутри вызова и уничтожается ВСЕГДА, в том числе при ошибке
    /// привязки: после `eglDestroyImage` текстура остаётся валидной и сама держит буфер
    /// (`EGL_KHR_image_base`), а наружу не утекает тип, который пришлось бы кому-то освобождать.
    ///
    /// Владение: fd только заимствуется и НЕ закрывается (см. [`DmaBufPlaneTexture`]).
    ///
    /// Побочные эффекты на состояние GL: после вызова `GL_TEXTURE_2D` активного юнита ничем не
    /// привязан (прежняя привязка потеряна), а накопленные флаги `glGetError` сняты. Фильтры и
    /// wrap текстуры не трогаются — их задаёт вызывающий.
    ///
    /// `texture` должна быть создана ЭТИМ контекстом (`glGenTextures`); её прежнее содержимое
    /// заменяется. Содержимое памяти буфера видно текстуре «вживую»: перезапись буфера между
    /// отрисовками видна без повторного вызова.
    pub fn attach_dma_buf_plane_to_texture(
        &self,
        texture: glow::NativeTexture,
        plane: &DmaBufPlaneTexture<'_>,
    ) -> Result<DmaBufTextureAttached, DmaBufTextureError> {
        let gl = match self.current_gl() {
            Ok(gl) => gl,
            Err(error) => {
                self.record_attach_failure();
                return Err(DmaBufTextureError::ContextUnavailable(error));
            }
        };
        let lease = self.session.lease();
        let capabilities = ImportCapabilities {
            gl_extensions: self.capabilities.gl_extensions(),
            egl_extensions: self.capabilities.egl_display_extensions(),
            dma_buf: self.capabilities.dma_buf(),
        };
        let outcome = attach_plane_to_texture(
            &ImageEgl {
                driver: lease.driver(),
                display: lease.display(),
            },
            &capabilities,
            &ContextImageTarget {
                gl,
                function: self.image_target_texture.as_ref(),
            },
            texture,
            plane,
            &self.dma_buf_stats,
        );
        if let Err(DmaBufTextureError::ImageCreation(error)) = &outcome
            && error.is_context_lost()
        {
            self.health
                .set(ContextHealth::Lost(crate::loss::ContextLoss::from_egl()));
        }
        outcome
    }

    /// Счётчики импорта DMA-BUF этим контекстом.
    pub fn dma_buf_texture_stats(&self) -> DmaBufTextureStats {
        self.dma_buf_stats.get()
    }

    /// Ошибка до начала оркестрации (контекст недоступен) тоже считается отказом привязки.
    fn record_attach_failure(&self) {
        let mut stats = self.dma_buf_stats.get();
        stats.attach_failures += 1;
        self.dma_buf_stats.set(stats);
    }
}

/// Стартовые счётчики нового контекста.
pub(super) fn initial_stats() -> Cell<DmaBufTextureStats> {
    Cell::new(DmaBufTextureStats::default())
}
