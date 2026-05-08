//! Общие контракты render layer.
//!
//! Этот crate не создаёт GPU-ресурсы и не зависит от `wgpu`, `egui` или windowing.
//! Его задача — описать факты, которые нужны player/capability layer для выбора
//! воспроизводимого потока до запуска decode.

#![forbid(unsafe_code)]

use std::fmt;
use std::time::Duration;

use codec_core::{BitDepth, ChromaSubsampling, VideoDecodeRequirement};
use serde::{Deserialize, Serialize};

/// Стабильный идентификатор семейства renderer backend-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderBackendKind {
    /// WGPU backend: Vulkan-first на Linux, с будущими DX12/Metal target-ами.
    Wgpu,

    /// Future OpenGL ES fallback для старых X11/GLES2 систем.
    OpenGles,
}

impl RenderBackendKind {
    /// Возвращает короткое стабильное имя backend-а для diagnostics.
    #[must_use]
    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::Wgpu => "wgpu",
            Self::OpenGles => "opengles",
        }
    }

    /// Возвращает человекочитаемое имя backend-а для UI/report.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Wgpu => "WGPU",
            Self::OpenGles => "OpenGL ES",
        }
    }
}

impl fmt::Display for RenderBackendKind {
    /// Печатает стабильный id без дополнительного форматирования.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.stable_id())
    }
}

/// Формат decoded frame, который renderer может принять на вход.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoFrameFormat {
    /// 8-bit 4:2:0 NV12: отдельная Y plane и interleaved UV plane.
    Nv12,

    /// 10-bit 4:2:0 P010: будущий путь для HDR и 10-bit SDR.
    P010,

    /// 8-bit RGBA texture: software/upload fallback или готовый RGB path.
    Rgba8,
}

impl VideoFrameFormat {
    /// Выводит минимально ожидаемый renderer input format из stream requirement.
    ///
    /// Unknown bit depth трактуется как текущий MVP SDR/NV12 path. Если bitstream
    /// позже уточнит profile/bit depth до P010, повторная проверка capability layer
    /// отвергнет поток до отправки packet-а в hardware decoder.
    #[must_use]
    pub fn from_decode_requirement(requirement: &VideoDecodeRequirement) -> Option<Self> {
        if let Some(chroma) = requirement.chroma
            && chroma != ChromaSubsampling::Yuv420
        {
            return None;
        }

        match requirement.bit_depth {
            Some(BitDepth::Ten) => Some(Self::P010),
            Some(BitDepth::Twelve) => None,
            Some(BitDepth::Eight) | None => Some(Self::Nv12),
        }
    }
}

impl fmt::Display for VideoFrameFormat {
    /// Печатает формат кадра в привычной video-терминологии.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Nv12 => "NV12",
            Self::P010 => "P010",
            Self::Rgba8 => "RGBA8",
        };
        formatter.write_str(label)
    }
}

/// Цветовое пространство decoded frame на render boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderColorSpace {
    /// BT.709 limited range.
    Bt709Limited,

    /// BT.709 full range.
    Bt709Full,

    /// BT.601, чаще legacy SD-контент.
    Bt601,

    /// Metadata пока неизвестна или не проброшена.
    Unknown,
}

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
    pub format: VideoFrameFormat,

    /// Coded width из decoder-а.
    pub coded_width: u32,

    /// Coded height из decoder-а.
    pub coded_height: u32,

    /// Display width после crop/aspect handling.
    pub render_width: u32,

    /// Display height после crop/aspect handling.
    pub render_height: u32,

    /// Цветовое пространство кадра.
    pub color_space: RenderColorSpace,
}

impl RenderableFrame {
    /// Возвращает `true`, если frame содержит ненулевой display size.
    #[must_use]
    pub const fn has_display_size(&self) -> bool {
        self.render_width > 0 && self.render_height > 0
    }
}

/// Возможности одного renderer backend-а.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderCapabilities {
    /// Семейство backend-а.
    pub backend: RenderBackendKind,

    /// Человекочитаемое имя backend-а.
    pub display_name: String,

    /// Форматы decoded frame, которые backend может принять.
    pub supported_frame_formats: Vec<VideoFrameFormat>,

    /// Может ли backend выполнить HDR-to-SDR tone mapping.
    pub supports_hdr_to_sdr: bool,

    /// Может ли backend отдать native HDR output в swapchain/display.
    pub supports_native_hdr_output: bool,

    /// Максимальный размер 2D texture, если backend его сообщил.
    pub max_texture_size: Option<u32>,

    /// Поддерживает ли backend расширенный UI overlay.
    pub advanced_ui: bool,

    /// Как backend композитит UI.
    pub ui_composition_mode: UiCompositionMode,

    /// Есть ли у backend-а метрики present/frame pacing.
    pub present_timing_metrics: bool,
}

impl RenderCapabilities {
    /// Создаёт capabilities для текущего WGPU MVP backend-а.
    #[must_use]
    pub fn wgpu_nv12(max_texture_size: Option<u32>) -> Self {
        Self {
            backend: RenderBackendKind::Wgpu,
            display_name: "WGPU NV12 renderer".to_string(),
            supported_frame_formats: vec![VideoFrameFormat::Nv12],
            supports_hdr_to_sdr: false,
            supports_native_hdr_output: false,
            max_texture_size,
            advanced_ui: true,
            ui_composition_mode: UiCompositionMode::Overlay,
            present_timing_metrics: true,
        }
    }

    /// Проверяет прямую поддержку входного frame format.
    #[must_use]
    pub fn supports_frame_format(&self, format: VideoFrameFormat) -> bool {
        self.supported_frame_formats.contains(&format)
    }

    /// Проверяет, сможет ли renderer показать stream с указанными требованиями.
    #[must_use]
    pub fn supports_decode_requirement(&self, requirement: &VideoDecodeRequirement) -> bool {
        if requirement.hdr && !(self.supports_hdr_to_sdr || self.supports_native_hdr_output) {
            return false;
        }

        let Some(frame_format) = VideoFrameFormat::from_decode_requirement(requirement) else {
            return false;
        };

        if !self.supports_frame_format(frame_format) {
            return false;
        }

        if let (Some(width), Some(max_texture_size)) = (requirement.width, self.max_texture_size)
            && width > max_texture_size
        {
            return false;
        }

        if let (Some(height), Some(max_texture_size)) = (requirement.height, self.max_texture_size)
            && height > max_texture_size
        {
            return false;
        }

        true
    }

    /// Формирует одну строку diagnostics для capability report.
    #[must_use]
    pub fn summary_text(&self) -> String {
        let frame_formats = self
            .supported_frame_formats
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");

        let hdr_label = if self.supports_hdr_to_sdr || self.supports_native_hdr_output {
            "HDR supported"
        } else {
            "SDR only"
        };

        format!(
            "{}: {}, formats: {}, max texture: {}",
            self.display_name,
            hdr_label,
            frame_formats,
            self.max_texture_size
                .map(|size| size.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        )
    }
}

#[cfg(test)]
mod tests {
    use codec_core::{BitDepth, ChromaSubsampling, VideoCodec, VideoDecodeRequirement};

    use super::*;

    #[test]
    fn eight_bit_yuv420_requirement_maps_to_nv12() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_bit_depth(BitDepth::Eight)
            .with_chroma(ChromaSubsampling::Yuv420);

        assert_eq!(
            VideoFrameFormat::from_decode_requirement(&requirement),
            Some(VideoFrameFormat::Nv12)
        );
    }

    #[test]
    fn ten_bit_requirement_maps_to_p010() {
        let requirement =
            VideoDecodeRequirement::new(VideoCodec::Vp9).with_bit_depth(BitDepth::Ten);

        assert_eq!(
            VideoFrameFormat::from_decode_requirement(&requirement),
            Some(VideoFrameFormat::P010)
        );
    }

    #[test]
    fn current_wgpu_capabilities_reject_p010_until_hdr_phase() {
        let capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
        let requirement =
            VideoDecodeRequirement::new(VideoCodec::Vp9).with_bit_depth(BitDepth::Ten);

        assert!(!capabilities.supports_decode_requirement(&requirement));
    }
}
