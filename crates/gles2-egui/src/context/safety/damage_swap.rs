//! Возраст заднего буфера и показ с повреждениями (`EGL_EXT_buffer_age`,
//! `EGL_KHR_swap_buffers_with_damage`, `EGL_EXT_swap_buffers_with_damage`).
//!
//! Часть модуля безопасности: каждый `unsafe`-блок с `// SAFETY:`. Вынесено из `safety.rs`,
//! чтобы тот не рос (правило размера файла). Дочерний модуль видит приватные поля `SystemEgl`.
//!
//! Спецификации (Khronos EGL Registry):
//! - `EGL_EXT_buffer_age`: `eglQuerySurface(EGL_BUFFER_AGE_EXT = 0x313D)`; 0 — содержимое не
//!   определено; запрос делается, когда surface текущая, и до рисования кадра;
//! - `EGL_KHR_swap_buffers_with_damage` / `EGL_EXT_swap_buffers_with_damage`:
//!   `EGLBoolean eglSwapBuffersWithDamage{KHR,EXT}(dpy, surface, const EGLint *rects, EGLint
//!   n_rects)`; прямоугольники {x, y, width, height} от НИЖНЕГО левого угла surface; весь буфер
//!   всё равно становится передним, прямоугольники — подсказка композитору; `n_rects = 0` —
//!   повреждена вся surface.

use std::sync::OnceLock;

use khronos_egl as egl;

use super::{
    DamageRect, DamageSwapExtension, DisplayId, EglCallError, EglErrorCode, EglOperation,
    SurfaceId, SystemEgl, call_error, display_handle, error_code, surface_handle,
};

/// `EGL_BUFFER_AGE_EXT` (`EGL_EXT_buffer_age`).
const EGL_BUFFER_AGE_EXT: egl::Int = 0x313D;

/// `eglSwapBuffersWithDamageKHR` / `eglSwapBuffersWithDamageEXT`: сигнатуры обоих расширений
/// совпадают.
type SwapBuffersWithDamageFn = unsafe extern "system" fn(
    egl::EGLDisplay,
    egl::EGLSurface,
    *const egl::Int,
    egl::Int,
) -> egl::Boolean;

/// Адреса функций показа с повреждениями. Ищутся при первом использовании и дальше берутся из
/// кэша (`None` внутри — драйвер адрес не дал: ответ «функции нет» тоже кэшируется).
#[derive(Default)]
pub(super) struct DamageSwapFunctions {
    khr: OnceLock<Option<SwapBuffersWithDamageFn>>,
    ext: OnceLock<Option<SwapBuffersWithDamageFn>>,
}

impl SystemEgl {
    /// `eglQuerySurface(EGL_BUFFER_AGE_EXT)`; 0 — содержимое буфера не определено.
    pub(super) fn query_buffer_age_of_surface(
        &self,
        display: DisplayId,
        surface: SurfaceId,
    ) -> Result<u32, EglCallError> {
        // Безопасная обёртка khronos-egl: указатели display/surface проверяет драйвер
        // (EGL_BAD_SURFACE), значение возвращается по значению.
        let age = self
            .instance
            .query_surface(
                display_handle(display),
                surface_handle(surface),
                EGL_BUFFER_AGE_EXT,
            )
            .map_err(call_error(EglOperation::QueryBufferAge))?;
        // Спецификация не допускает отрицательный возраст; чужое значение считаем «не определено».
        Ok(u32::try_from(age).unwrap_or(0))
    }

    /// Показ с повреждениями через `extension`.
    pub(super) fn swap_surface_buffers_with_damage(
        &self,
        display: DisplayId,
        surface: SurfaceId,
        extension: DamageSwapExtension,
        rects: &[DamageRect],
    ) -> Result<(), EglCallError> {
        let unreported = EglCallError::new(
            EglOperation::SwapBuffersWithDamage,
            EglErrorCode::Unreported,
        );
        let swap = self.damage_swap_function(extension).ok_or(unreported)?;
        // Прямоугольники — группы по четыре `EGLint`: {x, y, width, height}, начало снизу слева.
        let flat: Vec<egl::Int> = rects
            .iter()
            .flat_map(|rect| [rect.x, rect.y, rect.width, rect.height])
            .collect();
        let rect_count = egl::Int::try_from(rects.len()).map_err(|_| unreported)?;
        // SAFETY: `flat` живёт до конца вызова и содержит ровно `4 * rect_count` целых; драйвер
        // только читает массив (`const EGLint *`). display и surface — дескрипторы этого
        // драйвера из одной сессии. Адрес `swap` получен для расширения, которое заявил
        // дисплей (вызывающий выбирает `extension` по его списку расширений).
        let swapped = unsafe {
            swap(
                display_handle(display).as_ptr(),
                surface_handle(surface).as_ptr(),
                flat.as_ptr(),
                rect_count,
            )
        };
        if swapped == egl::TRUE {
            return Ok(());
        }
        // Код ошибки копит eglGetError; тот же разбор, что у `eglSwapBuffers`.
        Err(match self.instance.get_error() {
            Some(error) => {
                EglCallError::new(EglOperation::SwapBuffersWithDamage, error_code(error))
            }
            None => unreported,
        })
    }

    /// Адрес функции расширения из кэша (первый вызов спрашивает `eglGetProcAddress`).
    fn damage_swap_function(
        &self,
        extension: DamageSwapExtension,
    ) -> Option<SwapBuffersWithDamageFn> {
        let (cache, name) = match extension {
            DamageSwapExtension::Khr => (
                &self.damage_swap_functions.khr,
                "eglSwapBuffersWithDamageKHR",
            ),
            DamageSwapExtension::Ext => (
                &self.damage_swap_functions.ext,
                "eglSwapBuffersWithDamageEXT",
            ),
        };
        *cache.get_or_init(|| {
            let function = self.instance.get_proc_address(name)?;
            // SAFETY: сигнатура из раздела «New Procedures» расширений
            // `EGL_KHR_swap_buffers_with_damage` / `EGL_EXT_swap_buffers_with_damage`; адрес
            // выдан `eglGetProcAddress` этой же библиотеки.
            Some(unsafe {
                std::mem::transmute::<extern "system" fn(), SwapBuffersWithDamageFn>(function)
            })
        })
    }
}
