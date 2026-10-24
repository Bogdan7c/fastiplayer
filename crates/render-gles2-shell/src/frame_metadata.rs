//! Нейтральные метаданные кадра для `render-core` (`RenderableFrame`) из кадра декодера.
//!
//! Те же поля, что берёт wgpu-рендер (`renderable_metadata_from_decoded`), но без паники:
//! раскладка без битности или прореживания (RGBA) — типизированная ошибка.

use render_core::RenderableFrame;
use thiserror::Error;
use video_core::DecodedFrame;
use video_frame_contract::VideoFramePixelLayout;

/// У раскладки кадра нет YUV-метаданных (битности или прореживания цветности).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("pixel layout {0:?} has no YUV bit depth or chroma subsampling")]
pub struct NotYuvFrame(pub VideoFramePixelLayout);

/// Метаданные кадра `frame` для правил `render-core` (letterbox, цветовой путь).
///
/// # Errors
/// [`NotYuvFrame`] — раскладка без YUV-метаданных.
pub fn renderable_frame(frame: &DecodedFrame) -> Result<RenderableFrame, NotYuvFrame> {
    let format = frame.format();
    let (Some(bit_depth), Some(chroma)) = (frame.bit_depth(), frame.chroma()) else {
        return Err(NotYuvFrame(format));
    };
    Ok(RenderableFrame {
        handle: frame.resource_handle.0,
        pts: frame.pts,
        format,
        bit_depth,
        chroma,
        coded_width: frame.width,
        coded_height: frame.height,
        render_width: frame.render_width,
        render_height: frame.render_height,
        display_orientation: frame.display_orientation,
        color: frame.color.clone(),
    })
}
