//! [`FrameLayerStack`]: подложка от одного владельца, оверлеи от другого в одном кадре.
//!
//! У плеера видео (`gles2-video::VideoUnderlay`) и субтитры (адаптер шелла) — разные владельцы
//! состояния. Сборщик кадра принимает один [`FrameLayers`]; стек склеивает два, не смешивая их
//! ответственность: у первого вызывается только подложка, у второго — только оверлеи.

use crate::composition::{FrameLayerError, FrameLayers};
use crate::frame::ActiveFrame;

/// Подложка `underlay` + оверлеи `overlays`.
pub struct FrameLayerStack<'underlay, 'overlays> {
    underlay: &'underlay mut dyn FrameLayers,
    overlays: &'overlays mut dyn FrameLayers,
}

impl<'underlay, 'overlays> FrameLayerStack<'underlay, 'overlays> {
    /// Стек: подложку рисует `underlay`, оверлеи — `overlays`.
    pub fn new(
        underlay: &'underlay mut dyn FrameLayers,
        overlays: &'overlays mut dyn FrameLayers,
    ) -> Self {
        Self { underlay, overlays }
    }
}

impl FrameLayers for FrameLayerStack<'_, '_> {
    fn draw_underlay(&mut self, frame: &ActiveFrame<'_>) -> Result<(), FrameLayerError> {
        self.underlay.draw_underlay(frame)
    }

    fn draw_overlays(&mut self, frame: &ActiveFrame<'_>) -> Result<(), FrameLayerError> {
        self.overlays.draw_overlays(frame)
    }
}
