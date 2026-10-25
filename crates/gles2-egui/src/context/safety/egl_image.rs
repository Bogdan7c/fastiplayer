//! FFI импорта DMA-BUF в текстуру GL: `eglCreateImage` / `eglDestroyImage` и
//! `glEGLImageTargetTexture2DOES`.
//!
//! Спецификации (registry.khronos.org):
//! - `EGL_EXT_image_dma_buf_import` — цель `EGL_LINUX_DMA_BUF_EXT`, атрибуты плоскости; контекст
//!   обязан быть `EGL_NO_CONTEXT`, буфер клиента — NULL; fd EGL не забирает (issue 3);
//! - `EGL_KHR_image_base` — `eglCreateImage`/`eglDestroyImage`, текстуры-siblings переживают
//!   уничтожение образа;
//! - `GL_OES_EGL_image` — `glEGLImageTargetTexture2DOES` (в `glow` её нет, грузим через
//!   `eglGetProcAddress`).

use std::ffi::c_void;
use std::os::raw::c_uint;

use khronos_egl as egl;

use super::gl::CurrentGl;
use super::{SystemEgl, display_handle, error_code};
use crate::context::dma_buf_texture::DmaBufImageAttributes;
use crate::context::driver::{DisplayId, EglCallError, EglOperation, ImageId};

/// `EGL_LINUX_DMA_BUF_EXT` (`eglext.h`, `EGL_EXT_image_dma_buf_import`).
const EGL_LINUX_DMA_BUF_EXT: egl::Enum = 0x3270;
/// `GL_TEXTURE_2D`.
const GL_TEXTURE_2D: c_uint = 0x0DE1;

/// `glEGLImageTargetTexture2DOES(GLenum target, GLeglImageOES image)` (`GL_OES_EGL_image`).
type ImageTargetTexture2dFn = unsafe extern "system" fn(c_uint, *mut c_void);

/// Загруженная `glEGLImageTargetTexture2DOES` текущего контекста.
#[derive(Clone, Copy)]
pub(crate) struct ImageTargetTexture {
    function: ImageTargetTexture2dFn,
}

impl ImageTargetTexture {
    /// Делает образ хранилищем текстуры, привязанной к `GL_TEXTURE_2D` активного юнита.
    ///
    /// `CurrentGl` — доказательство, что контекст текущий в этом потоке.
    pub(crate) fn target_bound_texture_2d(&self, _current: CurrentGl<'_>, image: ImageId) {
        // SAFETY: адрес выдан `eglGetProcAddress` для функции с сигнатурой из `GL_OES_EGL_image`;
        // контекст текущий (`CurrentGl`); `image` создан `eglCreateImage` на display этого
        // контекста и ещё не уничтожен — оркестратор уничтожает образ только после вызова.
        // Ошибки формата драйвер сообщает через `glGetError`, а не UB.
        unsafe { (self.function)(GL_TEXTURE_2D, image.0 as *mut c_void) }
    }
}

impl SystemEgl {
    /// Загружает `glEGLImageTargetTexture2DOES`; `None` — драйвер адрес не дал (NULL).
    pub(crate) fn load_image_target_texture(&self) -> Option<ImageTargetTexture> {
        let address = self.proc_address("glEGLImageTargetTexture2DOES");
        if address.is_null() {
            return None;
        }
        // SAFETY: адрес не NULL и принадлежит функции `glEGLImageTargetTexture2DOES` из
        // `GL_OES_EGL_image`; сигнатура `void (GLenum, GLeglImageOES)`.
        let function: ImageTargetTexture2dFn = unsafe { std::mem::transmute(address) };
        Some(ImageTargetTexture { function })
    }

    /// `eglCreateImage(EGL_LINUX_DMA_BUF_EXT)`: контекст `EGL_NO_CONTEXT`, буфер NULL.
    pub(super) fn create_dma_buf_image_checked(
        &self,
        display: DisplayId,
        attributes: &DmaBufImageAttributes,
    ) -> Result<ImageId, EglCallError> {
        // SAFETY: `EGL_NO_CONTEXT` и NULL-буфер — единственные допустимые значения для цели
        // `EGL_LINUX_DMA_BUF_EXT` (`EGL_EXT_image_dma_buf_import`, «Description»).
        let (no_context, no_buffer) = unsafe {
            (
                egl::Context::from_ptr(egl::NO_CONTEXT),
                egl::ClientBuffer::from_ptr(std::ptr::null_mut()),
            )
        };
        // Список атрибутов завершён `EGL_NONE`; fd внутри него живы, пока вызывающий держит
        // `BorrowedFd` (граница API), и EGL их не закрывает.
        let image = self
            .instance
            .create_image(
                display_handle(display),
                no_context,
                EGL_LINUX_DMA_BUF_EXT,
                no_buffer,
                attributes.as_slice(),
            )
            .map_err(|error| EglCallError::new(EglOperation::CreateImage, error_code(error)))?;
        Ok(ImageId(image.as_ptr() as usize))
    }

    /// `eglDestroyImage`.
    pub(super) fn destroy_image_checked(
        &self,
        display: DisplayId,
        image: ImageId,
    ) -> Result<(), EglCallError> {
        // SAFETY: `ImageId` получен из `eglCreateImage` на этом display и уничтожается ровно
        // один раз (оркестратор `attach_plane_to_texture`).
        let image = unsafe { egl::Image::from_ptr(image.0 as egl::EGLImage) };
        self.instance
            .destroy_image(display_handle(display), image)
            .map_err(|error| EglCallError::new(EglOperation::DestroyImage, error_code(error)))
    }
}
