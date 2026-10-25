//! Хуки видеослоя для кадров из DMA-BUF (сессия 07): тонкие методы над [`DmaBufImports`].
//!
//! Логика кэша и импорта — в `dma_buf_cache` и `dma_buf_import`; здесь только проверка
//! контекста-владельца и согласование «текущего кадра» с кольцом загрузок.

use gles2_egui::Gles2Context;

use super::VideoLayerRenderer;
use crate::dma_buf_api::{CachedDmaBufFrame, DmaBufImportOutcome, VideoImportError};
use crate::dma_buf_cache::{DmaBufImportCacheCapacity, DmaBufImportCacheSnapshot};
use crate::dma_buf_frame::{DmaBufBufferIdentity, DmaBufNv12Frame};
use crate::gl_calls::VideoGl;
use crate::planes::VideoFrameKey;

impl VideoLayerRenderer {
    /// Показать кадр по паспорту буфера БЕЗ GL и без дескрипторов: путь «буфер уже импортирован».
    ///
    /// Вызывающий спрашивает кэш первым, и дескрипторы (dup fd) нужны ему только на
    /// [`CachedDmaBufFrame::NotCached`]. `Shown` делает кадр текущим и снимает «текущий» с кольца
    /// загрузок. Содержимое буфера сегодняшнего кадра видно при рисовании без повторного импорта
    /// (см. [`crate::DmaBufSharingContract`]).
    pub fn show_cached_dma_buf_frame(
        &mut self,
        buffer: &DmaBufBufferIdentity,
        key: VideoFrameKey,
    ) -> CachedDmaBufFrame {
        if self.current_key() == Some(key) {
            self.stats.repeated_frames += 1;
            return CachedDmaBufFrame::AlreadyCurrent;
        }
        let shown = self.dma_buf_imports.show_cached(buffer, key);
        if shown == CachedDmaBufFrame::Shown {
            self.ring.clear_current();
        }
        shown
    }

    /// Импортировать кадр NV12 из DMA-BUF без копии и сделать его текущим.
    ///
    /// Две текстуры: яркость `R8`, цветность `GR88` (`GL_OES_EGL_image`), фильтр `LINEAR` и
    /// `CLAMP_TO_EDGE` как у wgpu. Если буфера ещё нет в кэше, место под него освобождается ДО
    /// импорта (смена формата потока уничтожает все старые записи). Дескрипторы кадра можно
    /// закрыть сразу после вызова.
    ///
    /// # Errors
    /// [`VideoImportError`]: чужой или недоступный контекст, текстура больше лимита драйвера,
    /// отказ драйвера по плоскости (с причиной от `gles2-egui`). Копии вместо импорта нет никогда.
    pub fn import_dma_buf_frame(
        &mut self,
        context: &Gles2Context,
        frame: &DmaBufNv12Frame<'_>,
    ) -> Result<DmaBufImportOutcome, VideoImportError> {
        if context.id() != self.owner {
            return Err(VideoImportError::ForeignContext);
        }
        if self.current_key() == Some(frame.key()) {
            self.stats.repeated_frames += 1;
            return Ok(DmaBufImportOutcome::AlreadyCurrent);
        }
        let gl = VideoGl::for_context(context).map_err(VideoImportError::GlAccess)?;
        let outcome = self
            .dma_buf_imports
            .import(context, gl, frame, self.max_texture_size)?;
        // Импортированный кадр стал текущим: слот кольца больше не защищён званием «текущий».
        self.ring.clear_current();
        Ok(outcome)
    }

    /// Новая ёмкость кэша импортов (обычно — число слотов пула поверхностей декодера); лишние
    /// записи удаляются сразу.
    ///
    /// # Errors
    /// [`VideoImportError::ForeignContext`] / [`VideoImportError::GlAccess`]; кэш при ошибке не
    /// тронут.
    pub fn set_dma_buf_import_capacity(
        &mut self,
        context: &Gles2Context,
        capacity: DmaBufImportCacheCapacity,
    ) -> Result<(), VideoImportError> {
        let gl = self.gl_for_import_management(context)?;
        self.dma_buf_imports.set_capacity(gl, capacity);
        Ok(())
    }

    /// Удалить все импорты (смена декодера): текстуры держат память старых поверхностей. Текущий
    /// импортированный кадр забывается.
    ///
    /// # Errors
    /// Как у [`Self::set_dma_buf_import_capacity`].
    pub fn purge_dma_buf_imports(
        &mut self,
        context: &Gles2Context,
    ) -> Result<(), VideoImportError> {
        let gl = self.gl_for_import_management(context)?;
        self.dma_buf_imports.purge(gl);
        Ok(())
    }

    /// Состояние и счётчики кэша импортов.
    #[must_use]
    pub fn dma_buf_import_snapshot(&self) -> DmaBufImportCacheSnapshot {
        self.dma_buf_imports.snapshot()
    }

    fn gl_for_import_management<'a>(
        &self,
        context: &'a Gles2Context,
    ) -> Result<VideoGl<'a>, VideoImportError> {
        if context.id() != self.owner {
            return Err(VideoImportError::ForeignContext);
        }
        VideoGl::for_context(context).map_err(VideoImportError::GlAccess)
    }
}
