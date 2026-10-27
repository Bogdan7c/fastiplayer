//! Хуки видеослоя для программных планарных кадров (8 бит) из DMA-BUF (сессия 12): тонкие
//! методы над [`DmaBufPlanarImports`](crate::dma_buf_planar_import::DmaBufPlanarImports).
//!
//! Логика кэша и импорта — в `dma_buf_cache` и `dma_buf_planar_import`; здесь только проверка
//! контекста-владельца и согласование «текущего кадра» с кольцом загрузок и NV12-импортом.
//! Ёмкость и очистка кэша общие с NV12 (`set_dma_buf_import_capacity`,
//! `purge_dma_buf_imports` в `renderer/dma_buf.rs`).

use gles2_egui::Gles2Context;

use super::VideoLayerRenderer;
use crate::dma_buf_api::{CachedDmaBufFrame, PlanarDmaBufImportOutcome, VideoImportError};
use crate::dma_buf_cache::DmaBufImportCacheSnapshot;
use crate::dma_buf_planar_frame::{DmaBufPlanarFrame, DmaBufPlanarIdentity};
use crate::planes::VideoFrameKey;

impl VideoLayerRenderer {
    /// Показать планарный кадр по паспорту буфера БЕЗ GL и без дескрипторов: путь «блок уже
    /// импортирован».
    ///
    /// Вызывающий спрашивает кэш первым, и dup fd нужен ему только на
    /// [`CachedDmaBufFrame::NotCached`]. `Shown` делает кадр текущим и снимает «текущий» с кольца
    /// загрузок и NV12-импорта. Содержимое блока сегодняшнего кадра видно при рисовании без
    /// повторного импорта (см. [`crate::DmaBufSharingContract`]; кэш CPU сбрасывает
    /// производитель, если этого требует [`crate::probe_cpu_write_visibility`]).
    pub fn show_cached_planar_dma_buf_frame(
        &mut self,
        identity: &DmaBufPlanarIdentity,
        key: VideoFrameKey,
    ) -> CachedDmaBufFrame {
        if self.current_key() == Some(key) {
            self.stats.repeated_frames += 1;
            return CachedDmaBufFrame::AlreadyCurrent;
        }
        let shown = self.planar_imports.show_cached(identity, key);
        if shown == CachedDmaBufFrame::Shown {
            self.ring.clear_current();
            self.dma_buf_imports.clear_current();
        }
        shown
    }

    /// Импортировать планарный кадр 8 бит из DMA-BUF без копии и сделать его текущим.
    ///
    /// Три текстуры `R8` ровно размера плоскостей (шаг строки учитывает `EGLImage` по pitch),
    /// фильтр `LINEAR` и `CLAMP_TO_EDGE`; рисуется той же программой, что кадр после `upload`.
    /// Если паспорта ещё нет в кэше, место под него освобождается ДО импорта. Дескриптор кадра
    /// можно закрыть сразу после вызова.
    ///
    /// # Errors
    /// [`VideoImportError`]: чужой или недоступный контекст, плоскость больше лимита драйвера,
    /// отказ драйвера по плоскости (с причиной от `gles2-egui`). Копии вместо импорта нет никогда:
    /// вызывающий при ошибке сам загружает кадр через `upload`. После ошибки в кэш ничего не
    /// добавлено, созданные текстуры удалены, а кадр, бывший текущим до вызова, остаётся
    /// текущим — кроме случая, когда его текстуры вытеснила подготовка места под импорт.
    pub fn import_planar_dma_buf_frame(
        &mut self,
        context: &Gles2Context,
        frame: &DmaBufPlanarFrame<'_>,
    ) -> Result<PlanarDmaBufImportOutcome, VideoImportError> {
        let gl = self.gl_for_import_management(context)?;
        if self.current_key() == Some(frame.key()) {
            self.stats.repeated_frames += 1;
            return Ok(PlanarDmaBufImportOutcome::AlreadyCurrent);
        }
        let outcome = self
            .planar_imports
            .import(context, gl, frame, self.max_texture_size)?;
        // Импортированный планарный кадр стал текущим: остальные источники больше не текущие.
        self.ring.clear_current();
        self.dma_buf_imports.clear_current();
        Ok(outcome)
    }

    /// Состояние и счётчики кэша планарных импортов (отдельно от кэша NV12).
    #[must_use]
    pub fn planar_dma_buf_import_snapshot(&self) -> DmaBufImportCacheSnapshot {
        self.planar_imports.snapshot()
    }
}
