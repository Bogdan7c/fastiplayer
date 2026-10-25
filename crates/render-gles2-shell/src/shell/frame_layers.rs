//! Слои кадра GLES2-рендера: подложка (видео или ничего) и оверлеи (субтитры).
//!
//! Подложка обещает сборщику кадра, что сама покроет всю цель, когда рисует видео: видеослой
//! заливает чёрным всё, куда не рисует кадр (полосы letterbox, область под исключениями, область
//! вне видео), поэтому заливка фона сборщиком — лишняя работа GPU (на Intel Ironlake полная
//! заливка 1440×900 стоит ~2 мс на кадр, `results/05.md`). Без видео подложки нет и фон
//! заливается как обычно.

use gles2_egui::{ActiveFrame, FrameLayerError, FrameLayers, UnderlayCoverage};
use gles2_video::{VideoDrawOutcome, VideoUnderlay};

/// Подложка кадра: видеослой по плану кадра или ничего.
pub(super) enum ShellUnderlay<'video, 'request> {
    /// Кадра видео нет: сборщик заливает фон сам.
    NoVideo,
    /// Видео рисует и цель, и чёрные поля само.
    Video(VideoUnderlay<'video, 'request>),
}

impl ShellUnderlay<'_, '_> {
    /// Что сделал видеослой в этом кадре (`None` — видео нет или кадр не дошёл до слоя).
    pub(super) fn video_outcome(&self) -> Option<VideoDrawOutcome> {
        match self {
            Self::NoVideo => None,
            Self::Video(underlay) => underlay.outcome(),
        }
    }
}

impl FrameLayers for ShellUnderlay<'_, '_> {
    fn draw_underlay(&mut self, frame: &ActiveFrame<'_>) -> Result<(), FrameLayerError> {
        match self {
            Self::NoVideo => Ok(()),
            Self::Video(underlay) => underlay.draw_underlay(frame),
        }
    }

    fn underlay_coverage(&self) -> UnderlayCoverage {
        match self {
            Self::NoVideo => UnderlayCoverage::LeavesTargetPartlyUntouched,
            Self::Video(_) => UnderlayCoverage::PaintsWholeTarget,
        }
    }
}
