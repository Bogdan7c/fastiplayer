//! Размер пула поверхностей hardware decoder-а, который подставляется вместо «авто» (0 в config).
//!
//! Конфиг (`video.decoder_surface_pool_frames`, `video.zero_copy_surface_pool_slots`) использует
//! `0` как «автоматически». Нейтральный `PlayerVideoDecoderThreadConfig` нулей не принимает,
//! поэтому «авто» раскрывается ровно в одной точке — на границе config -> decoder config
//! (`decoder_thread_config_from_app_config`). Само число зависит от активного рендерера и
//! известно только composition layer (app), поэтому оно приходит сюда явным параметром.

use std::num::NonZeroUsize;

/// Размер пула, который нужен активному рендереру, когда пользователь оставил настройку «авто».
///
/// Newtype над `NonZeroUsize`: ноль невыразим, а на месте вызова видно смысл аргумента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutomaticSurfacePool(NonZeroUsize);

impl AutomaticSurfacePool {
    /// Создаёт значение из положительного числа кадров/слотов.
    #[must_use]
    pub const fn new(frames: NonZeroUsize) -> Self {
        Self(frames)
    }

    /// Возвращает размер пула в кадрах (он же число import-слотов).
    #[must_use]
    pub const fn frames(self) -> usize {
        self.0.get()
    }

    /// Раскрывает значение из config: `0` -> автоматический размер, остальное без изменений.
    #[must_use]
    pub(super) const fn resolve(self, configured_frames: usize) -> usize {
        if configured_frames == 0 {
            self.0.get()
        } else {
            configured_frames
        }
    }
}
