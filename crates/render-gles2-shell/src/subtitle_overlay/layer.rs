//! Субтитры как слой оверлеев сборщика кадра `gles2-egui` (между видео и egui — порядок wgpu).

use gles2_egui::{ActiveFrame, FrameLayerError, FrameLayers};
use render_core::RenderViewport;
use render_shell_api::SubtitleOverlayFrame;

use super::{Gles2SubtitleOverlayRenderer, SubtitleOverlayOutcome};

/// Оверлей кадра: субтитры `frame` внутри видимой области видео `visible_area`.
///
/// Видео (подложку) рисует другой слой; склеить их в один кадр — `gles2_egui::FrameLayerStack`.
pub struct SubtitleOverlayLayer<'renderer, 'frame> {
    renderer: &'renderer mut Gles2SubtitleOverlayRenderer,
    frame: Option<SubtitleOverlayFrame<'frame>>,
    visible_area: RenderViewport,
    outcome: Option<SubtitleOverlayOutcome>,
}

impl<'renderer, 'frame> SubtitleOverlayLayer<'renderer, 'frame> {
    /// Слой, который нарисует `frame` (или ничего, если субтитров нет).
    pub const fn new(
        renderer: &'renderer mut Gles2SubtitleOverlayRenderer,
        frame: Option<SubtitleOverlayFrame<'frame>>,
        visible_area: RenderViewport,
    ) -> Self {
        Self {
            renderer,
            frame,
            visible_area,
            outcome: None,
        }
    }

    /// Что сделал проход в этом кадре; `None` — сборщик кадра слой не звал (кадр пропущен или
    /// отклонён до рисования).
    #[must_use]
    pub const fn outcome(&self) -> Option<SubtitleOverlayOutcome> {
        self.outcome
    }
}

impl FrameLayers for SubtitleOverlayLayer<'_, '_> {
    fn draw_overlays(&mut self, frame: &ActiveFrame<'_>) -> Result<(), FrameLayerError> {
        let outcome = self
            .renderer
            .render(frame, self.frame.as_ref(), self.visible_area)
            .map_err(|error| FrameLayerError {
                message: format!("subtitle overlay: {error}"),
            })?;
        self.outcome = Some(outcome);
        Ok(())
    }
}
