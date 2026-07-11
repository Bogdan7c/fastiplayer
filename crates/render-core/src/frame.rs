use std::time::Duration;

use codec_core::{BitDepth, ChromaSubsampling, VideoDisplayOrientation};
use serde::{Deserialize, Serialize};
use video_frame_contract::VideoFramePixelLayout;

use crate::RenderColorMetadata;

/// Способ композиции UI относительно video pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiCompositionMode {
    /// UI рисуется поверх video pass в тот же swapchain frame.
    Overlay,

    /// Backend не занимается UI; shell использует отдельный путь.
    External,
}

/// Renderer-neutral описание кадра, готового к presentation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderableFrame {
    /// Opaque handle исходного decoded frame для связи с decoder texture pool.
    pub handle: u64,

    /// Presentation timestamp кадра.
    pub pts: Duration,

    /// Формат входных texture planes или готового RGB image.
    pub format: VideoFramePixelLayout,

    /// Bit depth decoded frame на render boundary.
    pub bit_depth: BitDepth,

    /// Chroma subsampling decoded frame на render boundary.
    pub chroma: ChromaSubsampling,

    /// Coded width из decoder-а.
    pub coded_width: u32,

    /// Coded height из decoder-а.
    pub coded_height: u32,

    /// Display width после crop/aspect handling.
    pub render_width: u32,

    /// Display height после crop/aspect handling.
    pub render_height: u32,

    /// Display orientation, которую renderer применяет при sampling.
    #[serde(default)]
    pub display_orientation: VideoDisplayOrientation,

    /// Typed color metadata кадра.
    pub color: RenderColorMetadata,
}

impl RenderableFrame {
    /// Возвращает `true`, если frame содержит ненулевой display size.
    #[must_use]
    pub const fn has_display_size(&self) -> bool {
        self.render_width > 0 && self.render_height > 0
    }

    /// Возвращает display width после применения quarter-turn orientation.
    #[must_use]
    pub const fn oriented_display_width(&self) -> u32 {
        if self.display_orientation.swaps_axes() {
            self.render_height
        } else {
            self.render_width
        }
    }

    /// Возвращает display height после применения quarter-turn orientation.
    #[must_use]
    pub const fn oriented_display_height(&self) -> u32 {
        if self.display_orientation.swaps_axes() {
            self.render_width
        } else {
            self.render_height
        }
    }
}
