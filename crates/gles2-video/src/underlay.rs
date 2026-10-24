//! Видеослой как подложка кадра `gles2-egui`: связка [`VideoLayerRenderer`] с
//! [`FrameLayers`].
//!
//! `FrameCompositor` зовёт подложку после заливки фона и до оверлеев и egui — тот же порядок
//! проходов, что у wgpu-шелла. Видео заливает цель чёрным и рисует кадр с альфой 1.

use gles2_egui::{ActiveFrame, FrameLayerError, FrameLayers};

use crate::layer_api::{VideoDrawOutcome, VideoDrawRequest};
use crate::renderer::VideoLayerRenderer;

/// Подложка кадра: видео по запросу `request`.
pub struct VideoUnderlay<'renderer, 'request> {
    renderer: &'renderer mut VideoLayerRenderer,
    request: VideoDrawRequest<'request>,
    outcome: Option<VideoDrawOutcome>,
}

impl<'renderer, 'request> VideoUnderlay<'renderer, 'request> {
    /// Подложка, которая нарисует текущий кадр `renderer` по `request`.
    pub const fn new(
        renderer: &'renderer mut VideoLayerRenderer,
        request: VideoDrawRequest<'request>,
    ) -> Self {
        Self {
            renderer,
            request,
            outcome: None,
        }
    }

    /// Что сделала подложка в этом кадре; `None` — сборщик кадра её не звал (кадр пропущен или
    /// отклонён до рисования).
    #[must_use]
    pub const fn outcome(&self) -> Option<VideoDrawOutcome> {
        self.outcome
    }
}

impl FrameLayers for VideoUnderlay<'_, '_> {
    fn draw_underlay(&mut self, frame: &ActiveFrame<'_>) -> Result<(), FrameLayerError> {
        let outcome =
            self.renderer
                .draw(frame, &self.request)
                .map_err(|error| FrameLayerError {
                    message: format!("video underlay: {error}"),
                })?;
        self.outcome = Some(outcome);
        Ok(())
    }
}
