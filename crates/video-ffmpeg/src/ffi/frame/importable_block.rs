//! Связь кадра FFmpeg с блоком нашего пула (cpu-parity 08b).
//!
//! Если кадр декодирован в блок `HostFrameBlockPool` (наш `get_buffer2`), кадр помнит
//! этот блок: владелец host-planar ресурса отдаёт его renderer-у для импорта в GPU без
//! CPU-копии. Кадр, выделенный обычным путём FFmpeg, блока не имеет.

use std::sync::Arc;

use super::OwnedAvFrame;
use crate::ffi::host_frame_memory::{HostFrameBlock, HostFrameBlockPool};

impl OwnedAvFrame {
    /// Блок нашего пула, в котором лежат plane-ы кадра, если он есть.
    ///
    /// Ссылка на кадр (эта или её `try_clone_ref`) держит содержимое блока: пока она
    /// жива, FFmpeg не отдаст блок под другой кадр.
    pub(crate) fn importable_block(&self) -> Option<&Arc<HostFrameBlock>> {
        self.importable_block.as_ref()
    }

    /// Находит блок кадра в пуле по адресу его первого буфера и запоминает его.
    ///
    /// Наш `get_buffer2` кладёт весь кадр в один `AVBufferRef` (`buf[0]`), чей `data`
    /// равен началу блока. Если `buf[0]` не наш (кадр выделен обычным путём FFmpeg),
    /// поиск ничего не находит и кадр остаётся без блока.
    pub(crate) fn attach_importable_block(&mut self, pool: &HostFrameBlockPool) {
        self.importable_block = self
            .first_buffer_address()
            .and_then(|address| pool.checked_out_block(address));
    }

    /// Адрес `buf[0]->data` как число, если у кадра есть первый буфер.
    fn first_buffer_address(&self) -> Option<usize> {
        // SAFETY: raw frame валиден, пока жив wrapper; `buf[0]` либо null, либо
        // указатель на живой `AVBufferRef`, которым владеет этот кадр. Читаем только
        // поле `data` (адрес), не разыменовывая сами данные.
        let first_buffer = unsafe { self.raw_frame.as_ref().buf[0] };
        if first_buffer.is_null() {
            return None;
        }
        // SAFETY: см. выше — `first_buffer` не null и живёт вместе с кадром.
        let data = unsafe { (*first_buffer).data };
        (!data.is_null()).then_some(data as usize)
    }
}
