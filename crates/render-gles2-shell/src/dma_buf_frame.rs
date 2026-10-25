//! Аппаратный кадр NV12 из DMA-BUF: перевод словаря плеера в словарь `gles2-video` и показ.
//!
//! # Путь кадра
//! `DecodedFrame` + провайдер ресурсов декодера → [`materialize_dma_buf_frame`]:
//! идентичность буфера ([`dma_buf_buffer_identity`]) → кэш импортов видеослоя → на промахе
//! дескриптор с dup fd → [`nv12_dma_buf_frame`] (разбор слоёв) → импорт (две текстуры на
//! `EGLImage`) → кадр текущий → рисование → fence кадра показа → освобождение кадра декодеру
//! (`frame_release`). Копий пикселей нет нигде.
//!
//! # Где закрываются fd
//! Dup fd дескриптора закрывает материализатор в конце вызова, после импорта (EGL fd не
//! забирает). Исходный fd декодера остаётся у декодера, кадр возвращается ему по fence.
//!
//! # Почему P010 не объявлен
//! Решение владельца 33: GPU с 10-битным аппаратным декодом всегда ведёт wgpu, поэтому в
//! GLES2 нет ни реализации, ни объявления `dma_buf_p010` (см. `capabilities`). P010-кадр
//! получает отдельный отказ [`Nv12DmaBufError::TenBitHardwareNotDeclared`].
//!
//! # Модули
//! - `error` — типизированные отказы разбора;
//! - `passport` — идентичность кадра → паспорт буфера;
//! - `frame` — дескриптор → кадр NV12 видеослоя;
//! - `outcome` — исходы показа и их соответствие исходам приложения (сессия 09);
//! - `materialize` — порядок «идентичность → кэш → дескриптор → импорт».

mod error;
mod frame;
mod materialize;
mod outcome;
mod passport;

#[cfg(test)]
mod tests;

pub use error::Nv12DmaBufError;
pub use frame::{dma_buf_frame_key, ensure_nv12_frame_contract, nv12_dma_buf_frame};
pub use materialize::{dma_buf_import_capacity_for, materialize_dma_buf_frame};
pub use outcome::{
    DmaBufMaterialization, DmaBufMaterializeOutcome, DmaBufShownFrom, DmaBufUnsupportedReason,
};
pub use passport::dma_buf_buffer_identity;
