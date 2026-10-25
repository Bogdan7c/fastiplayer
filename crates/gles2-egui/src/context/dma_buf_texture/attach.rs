//! Оркестрация «DMA-BUF → EGLImage → хранилище GL-текстуры».
//!
//! Здесь нет ни `unsafe`, ни `Gles2Context`: драйвер EGL приходит как [`EglDriver`], а сторона GL
//! — как [`TextureImageTarget`]. Поэтому порядок шагов, учёт счётчиков и «образ уничтожается
//! всегда» проверяются hermetic-тестами на fake-драйвере.
//!
//! Жизнь `EGLImage` ограничена этой функцией (EGL_KHR_image_base: после `eglDestroyImage`
//! созданные из образа текстуры остаются валидными и держат буфер сами), поэтому наружу образ
//! не выходит и в публичном API нет типа, который его бы владел.

use std::cell::Cell;

use super::attributes::DmaBufImageAttributes;
use super::{
    DmaBufMissingRequirement, DmaBufPlaneTexture, DmaBufTextureAttached, DmaBufTextureError,
    DmaBufTextureStats, ImageRelease,
};
use crate::capabilities::{DmaBufImportSupport, ExtensionSet, ModifierSampling};
use crate::context::driver::{DisplayId, EglDriver, ImageId};

/// Сторона GL операции: привязка текстуры и `glEGLImageTargetTexture2DOES`.
pub(crate) trait TextureImageTarget {
    /// Загружена ли `glEGLImageTargetTexture2DOES`.
    fn image_target_function_loaded(&self) -> bool;
    /// Привязывает текстуру к `GL_TEXTURE_2D` активного юнита (`None` — отвязка).
    fn bind_texture_2d(&self, texture: Option<glow::NativeTexture>);
    /// Делает образ хранилищем текстуры, привязанной к `GL_TEXTURE_2D`.
    fn target_bound_texture_from_image(&self, image: ImageId);
    /// `glGetError`: один накопленный флаг (`GL_NO_ERROR` — пусто).
    fn take_error_flag(&self) -> u32;
}

/// Расширения и список форматов драйвера, по которым принимается решение «можно ли».
pub(crate) struct ImportCapabilities<'a> {
    pub(crate) gl_extensions: &'a ExtensionSet,
    pub(crate) egl_extensions: &'a ExtensionSet,
    pub(crate) dma_buf: &'a DmaBufImportSupport,
}

/// EGL-сторона: драйвер и display, на котором создаются образы.
pub(crate) struct ImageEgl<'a> {
    pub(crate) driver: &'a dyn EglDriver,
    pub(crate) display: DisplayId,
}

/// `GL_NO_ERROR`.
const GL_NO_ERROR: u32 = 0;
/// Сколько накопленных флагов ошибок снять перед вызовом (у GL их набор по категориям).
const STALE_ERROR_DRAIN_LIMIT: usize = 16;

/// Расширение, без которого импорт невозможен, или `None`.
fn missing_requirement(
    capabilities: &ImportCapabilities<'_>,
    needs_modifiers: bool,
) -> Option<DmaBufMissingRequirement> {
    if !capabilities.gl_extensions.contains("GL_OES_EGL_image") {
        return Some(DmaBufMissingRequirement::GlOesEglImage);
    }
    if !capabilities
        .egl_extensions
        .contains("EGL_EXT_image_dma_buf_import")
    {
        return Some(DmaBufMissingRequirement::EglExtImageDmaBufImport);
    }
    if needs_modifiers
        && !capabilities
            .egl_extensions
            .contains("EGL_EXT_image_dma_buf_import_modifiers")
    {
        return Some(DmaBufMissingRequirement::EglExtImageDmaBufImportModifiers);
    }
    None
}

/// Делает плоскость `plane` хранилищем `texture`. Счётчики пишутся в `stats`.
pub(crate) fn attach_plane_to_texture(
    egl: &ImageEgl<'_>,
    capabilities: &ImportCapabilities<'_>,
    gl: &dyn TextureImageTarget,
    texture: glow::NativeTexture,
    plane: &DmaBufPlaneTexture<'_>,
    stats: &Cell<DmaBufTextureStats>,
) -> Result<DmaBufTextureAttached, DmaBufTextureError> {
    let outcome = attach_checked(egl, capabilities, gl, texture, plane, stats);
    if outcome.is_err() {
        update_stats(stats, |stats| stats.attach_failures += 1);
    }
    outcome
}

fn update_stats(stats: &Cell<DmaBufTextureStats>, change: impl FnOnce(&mut DmaBufTextureStats)) {
    let mut current = stats.get();
    change(&mut current);
    stats.set(current);
}

fn attach_checked(
    egl: &ImageEgl<'_>,
    capabilities: &ImportCapabilities<'_>,
    gl: &dyn TextureImageTarget,
    texture: glow::NativeTexture,
    plane: &DmaBufPlaneTexture<'_>,
    stats: &Cell<DmaBufTextureStats>,
) -> Result<DmaBufTextureAttached, DmaBufTextureError> {
    // 1. Расширения: без них не трогаем ни драйвер, ни GL.
    if let Some(missing) = missing_requirement(capabilities, !plane.modifier.is_invalid()) {
        return Err(DmaBufTextureError::ZeroCopyUnsupported { missing });
    }
    if !gl.image_target_function_loaded() {
        return Err(DmaBufTextureError::ImageTargetFunctionMissing);
    }
    // 2. Значения должны влезать в EGLint — до драйвера.
    let attributes = DmaBufImageAttributes::build(plane)
        .map_err(|attribute| DmaBufTextureError::AttributeOutOfRange { attribute })?;
    // 3. Пара «формат + модификатор» должна читаться обычной текстурой. Если форматы спросить
    // нельзя, пробуем: решает eglCreateImage.
    let sampling = capabilities
        .dma_buf
        .sampling_of_modifier(plane.fourcc, plane.modifier);
    match sampling {
        ModifierSampling::Sampler2D | ModifierSampling::FormatQueryUnavailable => {}
        ModifierSampling::ExternalOnly
        | ModifierSampling::NotListed
        | ModifierSampling::ImportUnavailable => {
            return Err(DmaBufTextureError::FormatModifierNotSampleable {
                fourcc: plane.fourcc,
                modifier: plane.modifier,
                sampling,
            });
        }
    }
    // 4. EGLImage. fd остаётся у вызывающего: EGL его не закрывает (issue 3 расширения).
    let image = egl
        .driver
        .create_dma_buf_image(egl.display, &attributes)
        .map_err(DmaBufTextureError::ImageCreation)?;
    update_stats(stats, |stats| stats.images_created += 1);

    // 5. Привязка к текстуре. Результат запоминаем, но образ уничтожаем В ЛЮБОМ случае.
    let target_result = target_texture(gl, texture, image);
    let release = destroy_image(egl, image, stats);

    target_result?;
    update_stats(stats, |stats| stats.attaches_succeeded += 1);
    Ok(DmaBufTextureAttached {
        modifier: attributes.modifier(),
        image_release: release,
    })
}

/// `bind` → `glEGLImageTargetTexture2DOES` → `glGetError` → отвязка.
fn target_texture(
    gl: &dyn TextureImageTarget,
    texture: glow::NativeTexture,
    image: ImageId,
) -> Result<(), DmaBufTextureError> {
    gl.bind_texture_2d(Some(texture));
    // Старые флаги ошибок не наши: без снятия мы бы приписали их этому вызову.
    for _ in 0..STALE_ERROR_DRAIN_LIMIT {
        if gl.take_error_flag() == GL_NO_ERROR {
            break;
        }
    }
    gl.target_bound_texture_from_image(image);
    let gl_error = gl.take_error_flag();
    // Побочный эффект документирован в API: привязка TEXTURE_2D активного юнита сбрасывается.
    gl.bind_texture_2d(None);
    if gl_error == GL_NO_ERROR {
        Ok(())
    } else {
        Err(DmaBufTextureError::TextureTarget { gl_error })
    }
}

/// `eglDestroyImage` с учётом счётчиков. Отказ не теряется: он попадает в лог и в отчёт.
fn destroy_image(
    egl: &ImageEgl<'_>,
    image: ImageId,
    stats: &Cell<DmaBufTextureStats>,
) -> ImageRelease {
    match egl.driver.destroy_image(egl.display, image) {
        Ok(()) => {
            update_stats(stats, |stats| stats.images_destroyed += 1);
            ImageRelease::Released
        }
        Err(error) => {
            log::warn!("eglDestroyImage failed, EGLImage may leak: {error}");
            update_stats(stats, |stats| stats.image_destroy_failures += 1);
            ImageRelease::DestroyFailed(error)
        }
    }
}
