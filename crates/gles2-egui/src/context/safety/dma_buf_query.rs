//! Запросы форматов и модификаторов импорта DMA-BUF (`EGL_EXT_image_dma_buf_import_modifiers`).
//!
//! Часть модуля безопасности (вынесено из `safety.rs` без изменений поведения): каждый
//! `unsafe`-блок с `// SAFETY:`. Дочерний модуль видит приватные поля `SystemEgl`.

use khronos_egl as egl;

use super::{
    DisplayId, DmaBufModifier, EglCallError, EglErrorCode, EglOperation, SystemEgl, display_handle,
};

/// `eglQueryDmaBufFormatsEXT` (`EGL_EXT_image_dma_buf_import_modifiers`).
type QueryDmaBufFormatsFn = unsafe extern "system" fn(
    egl::EGLDisplay,
    egl::Int,
    *mut egl::Int,
    *mut egl::Int,
) -> egl::Boolean;
/// `eglQueryDmaBufModifiersEXT`.
type QueryDmaBufModifiersFn = unsafe extern "system" fn(
    egl::EGLDisplay,
    egl::Int,
    egl::Int,
    *mut u64,
    *mut egl::Boolean,
    *mut egl::Int,
) -> egl::Boolean;

impl SystemEgl {
    pub(super) fn query_dma_buf_formats_impl(
        &self,
        display: DisplayId,
    ) -> Result<Vec<u32>, EglCallError> {
        let failure = EglCallError::new(EglOperation::QueryDmaBuf, EglErrorCode::Unreported);
        let function = self
            .instance
            .get_proc_address("eglQueryDmaBufFormatsEXT")
            .ok_or(failure)?;
        // SAFETY: сигнатура из `EGL_EXT_image_dma_buf_import_modifiers` (раздел «New Procedures»),
        // адрес выдан `eglGetProcAddress` той же библиотеки.
        let query: QueryDmaBufFormatsFn = unsafe { std::mem::transmute(function) };
        let raw_display = display_handle(display).as_ptr();
        let mut count: egl::Int = 0;
        // SAFETY: первый вызов с `max_formats = 0` и нулевым буфером только сообщает число
        // форматов в `count` (так определено расширением).
        let counted = unsafe { query(raw_display, 0, std::ptr::null_mut(), &mut count) };
        if counted != egl::TRUE || count < 0 {
            return Err(failure);
        }
        let mut formats: Vec<egl::Int> = vec![0; usize::try_from(count).unwrap_or_default()];
        let mut written: egl::Int = 0;
        // SAFETY: буфер `formats` длиной `count` элементов, драйвер пишет не больше `count`
        // и сообщает число записанных в `written`.
        let filled = unsafe { query(raw_display, count, formats.as_mut_ptr(), &mut written) };
        if filled != egl::TRUE {
            return Err(failure);
        }
        formats.truncate(usize::try_from(written).unwrap_or_default());
        Ok(formats
            .into_iter()
            .map(|fourcc| u32::from_ne_bytes(fourcc.to_ne_bytes()))
            .collect())
    }

    pub(super) fn query_dma_buf_modifiers_impl(
        &self,
        display: DisplayId,
        fourcc: u32,
    ) -> Result<Vec<DmaBufModifier>, EglCallError> {
        let failure = EglCallError::new(EglOperation::QueryDmaBuf, EglErrorCode::Unreported);
        let function = self
            .instance
            .get_proc_address("eglQueryDmaBufModifiersEXT")
            .ok_or(failure)?;
        // SAFETY: сигнатура из `EGL_EXT_image_dma_buf_import_modifiers`, адрес той же библиотеки.
        let query: QueryDmaBufModifiersFn = unsafe { std::mem::transmute(function) };
        let raw_display = display_handle(display).as_ptr();
        let format = egl::Int::from_ne_bytes(fourcc.to_ne_bytes());
        let mut count: egl::Int = 0;
        // SAFETY: запрос числа модификаторов: `max_modifiers = 0`, оба буфера нулевые.
        let counted = unsafe {
            query(
                raw_display,
                format,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut count,
            )
        };
        if counted != egl::TRUE || count < 0 {
            return Err(failure);
        }
        let capacity = usize::try_from(count).unwrap_or_default();
        let mut modifiers = vec![0_u64; capacity];
        let mut external_only = vec![egl::FALSE; capacity];
        let mut written: egl::Int = 0;
        // SAFETY: оба буфера длиной `count`; драйвер пишет не больше `count` элементов в каждый.
        let filled = unsafe {
            query(
                raw_display,
                format,
                count,
                modifiers.as_mut_ptr(),
                external_only.as_mut_ptr(),
                &mut written,
            )
        };
        if filled != egl::TRUE {
            return Err(failure);
        }
        let written = usize::try_from(written).unwrap_or_default().min(capacity);
        Ok(modifiers
            .into_iter()
            .zip(external_only)
            .take(written)
            .map(|(modifier, external_only)| DmaBufModifier {
                modifier,
                external_only: external_only == egl::TRUE,
            })
            .collect())
    }
}
