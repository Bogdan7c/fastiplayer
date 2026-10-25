//! GL-объекты рендера: сборщик кадра, видеослой и слой субтитров.
//!
//! Создаются и уничтожаются вместе, в одном порядке: при отказе создания очередного объекта уже
//! созданные уничтожаются, чтобы не оставлять GL-мусор в контексте.

use gles2_egui::{EguiPainterOptions, FrameCompositor, Gles2Context};
use gles2_video::{OverlayLayerOptions, VideoLayerOptions, VideoLayerRenderer};
use render_shell_api::RendererCreateError;

use super::create_error::pipeline_creation_error;
use crate::Gles2SubtitleOverlayRenderer;

/// Три GL-владельца рендера.
pub(super) struct GlLayers {
    /// Художник egui + маска углов + порядок проходов кадра.
    pub(super) compositor: FrameCompositor,
    /// Видеослой: плоскости, кольцо слотов, импорт DMA-BUF, цвет.
    pub(super) video: VideoLayerRenderer,
    /// Субтитры между видео и egui.
    pub(super) subtitles: Gles2SubtitleOverlayRenderer,
}

impl GlLayers {
    /// Создаёт объекты в `context`; при отказе чистит созданное.
    pub(super) fn create(context: &Gles2Context) -> Result<Self, RendererCreateError> {
        let compositor = FrameCompositor::new(context, EguiPainterOptions::default())
            .map_err(|error| pipeline_creation_error("egui frame compositor", &error))?;
        let video = match VideoLayerRenderer::new(context, VideoLayerOptions::default()) {
            Ok(video) => video,
            Err(error) => {
                destroy_compositor(compositor, context);
                return Err(pipeline_creation_error("video layer", &error));
            }
        };
        let subtitles =
            match Gles2SubtitleOverlayRenderer::new(context, OverlayLayerOptions::default()) {
                Ok(subtitles) => subtitles,
                Err(error) => {
                    destroy_video(video, context);
                    destroy_compositor(compositor, context);
                    return Err(pipeline_creation_error("subtitle overlay layer", &error));
                }
            };
        Ok(Self {
            compositor,
            video,
            subtitles,
        })
    }

    /// Уничтожает объекты (контекст жив) или забывает их (контекст потерян). Порядок обратен
    /// созданию; отказы пишутся в лог, потому что уничтожение не может быть отклонено.
    pub(super) fn destroy(self, context: &Gles2Context) {
        let Self {
            compositor,
            video,
            subtitles,
        } = self;
        if let Err(error) = subtitles.destroy(context) {
            log::warn!("GLES2: слой субтитров не уничтожен чисто: {error}");
        }
        destroy_video(video, context);
        destroy_compositor(compositor, context);
    }
}

fn destroy_video(video: VideoLayerRenderer, context: &Gles2Context) {
    if let Err(error) = video.destroy(context) {
        log::warn!("GLES2: видеослой не уничтожен чисто: {error}");
    }
}

fn destroy_compositor(compositor: FrameCompositor, context: &Gles2Context) {
    if let Err(error) = compositor.destroy(context) {
        log::warn!("GLES2: сборщик кадра не уничтожен чисто: {error}");
    }
}
