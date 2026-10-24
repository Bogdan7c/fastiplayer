//! Где и как рисовать кадр: всё из `render-core` — того же источника, что у wgpu-рендера.
//!
//! Порядок решений повторяет `WgpuVideoRenderer::render_video_frame`:
//! 1. область видео зажимается к цели (`RenderViewport::clamp_to_surface`);
//! 2. части области без исключений (`RenderViewport::visible_parts`) — по draw-вызову на часть;
//! 3. letterbox — `PictureLayout::fit_frame` по размеру области (начало области не влияет на uv);
//! 4. цвет:
//!    - 8 бит — `ActiveColorPath::from_frame`; неизвестный, широкий или HDR вход рисуется как SDR
//!      BT.709 limited (так же подменяет wgpu `effective_shader_color_metadata`);
//!    - 10/12 бит — правила 16-битного пути wgpu ([`crate::high_bit_color`]): SDR BT.709 без
//!      цветокоррекции или HDR → SDR BT.2446-C, остальное — ошибка, как у wgpu.

use codec_core::{
    BitDepth, ColorRange, MatrixCoefficients, VideoColorMetadata, VideoDisplayOrientation,
};
use gles2_video::{
    ColorAdjustment, PictureMapping, PixelRect, QuarterTurn, VideoColorConversion,
    VideoDrawRequest, YuvColorEncoding, YuvMatrix, YuvRange,
};
use render_core::{
    ActiveColorPath, ColorPipelineSettings, PictureLayout, RenderLiveSettings, RenderViewport,
    RenderableFrame, SwapchainTransferMode,
};
use thiserror::Error;

use crate::high_bit_color::{HighBitColorError, high_bit_color_conversion};

/// Где лежит видео на цели.
#[derive(Debug, Clone, Copy)]
pub struct VideoPlacement<'a> {
    /// Размер цели (surface) в физических пикселях: `(ширина, высота)`.
    pub surface_size: (u32, u32),
    /// Область видео (обычно вся цель).
    pub video_viewport: RenderViewport,
    /// UI, который прячет видео (сайдбар настроек).
    pub exclusions: &'a [RenderViewport],
}

/// План рисования кадра: владеет прямоугольниками, отдаёт запрос видеослою.
#[derive(Debug, Clone)]
pub struct VideoDrawPlan {
    viewport: PixelRect,
    visible_rects: Vec<PixelRect>,
    picture: PictureMapping,
    orientation: QuarterTurn,
    color: VideoColorConversion,
    active_color_path: ActiveColorPath,
}

/// План не построить.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VideoDrawPlanError {
    /// У кадра нулевой размер показа (как и у wgpu — ошибка, а не чёрный кадр).
    #[error("renderable frame has invalid display size: {width}x{height}")]
    InvalidDisplaySize {
        /// Ширина показа.
        width: u32,
        /// Высота показа.
        height: u32,
    },
    /// Цвет кадра 10/12 бит вне пути wgpu или HDR без включённого HDR → SDR.
    #[error(transparent)]
    HighBitColor(#[from] HighBitColorError),
}

impl VideoDrawPlan {
    /// План для кадра `frame` в `placement` с живыми настройками рендера `settings` (цвет и
    /// HDR → SDR — тот же снимок, что получает wgpu).
    ///
    /// # Errors
    /// [`VideoDrawPlanError::InvalidDisplaySize`] — у кадра нулевой размер показа;
    /// [`VideoDrawPlanError::HighBitColor`] — цвет кадра 10/12 бит не рисуется (как у wgpu).
    pub fn for_frame(
        frame: &RenderableFrame,
        placement: VideoPlacement<'_>,
        settings: &RenderLiveSettings,
    ) -> Result<Self, VideoDrawPlanError> {
        if !frame.has_display_size() {
            return Err(VideoDrawPlanError::InvalidDisplaySize {
                width: frame.render_width,
                height: frame.render_height,
            });
        }
        let (surface_width, surface_height) = placement.surface_size;
        let viewport = placement
            .video_viewport
            .clamp_to_surface(surface_width, surface_height);
        let visible_rects = viewport
            .visible_parts(placement.exclusions)
            .into_iter()
            .map(pixel_rect)
            .collect();
        let (viewport_width, viewport_height) = viewport.size();
        let picture = PictureLayout::fit_frame(
            frame,
            RenderViewport::new(0, 0, viewport_width, viewport_height),
        )
        .map_or(PictureMapping::FILL, |layout| {
            let (uv_scale, uv_offset) = layout.uv_scale_and_offset();
            PictureMapping {
                uv_scale,
                uv_offset,
            }
        });
        let (color, active_color_path) = frame_color(frame, settings)?;
        Ok(Self {
            viewport: pixel_rect(viewport),
            visible_rects,
            picture,
            orientation: quarter_turn(frame.display_orientation),
            color,
            active_color_path,
        })
    }

    /// Запрос видеослою (заимствует прямоугольники плана).
    #[must_use]
    pub fn request(&self) -> VideoDrawRequest<'_> {
        VideoDrawRequest {
            viewport: self.viewport,
            visible_rects: &self.visible_rects,
            picture: self.picture,
            orientation: self.orientation,
            color: self.color,
        }
    }

    /// Выбранный цветовой путь (для диагностики, как у wgpu).
    #[must_use]
    pub const fn active_color_path(&self) -> &ActiveColorPath {
        &self.active_color_path
    }
}

fn pixel_rect(viewport: RenderViewport) -> PixelRect {
    PixelRect {
        x: viewport.x,
        y: viewport.y,
        width: viewport.width,
        height: viewport.height,
    }
}

/// Перевод цвета кадра и цветовой путь для диагностики — по разрядности, как разводит wgpu
/// (8 бит — `render_unorm8_frame`, 10/12 — `render_uint16_frame`).
fn frame_color(
    frame: &RenderableFrame,
    settings: &RenderLiveSettings,
) -> Result<(VideoColorConversion, ActiveColorPath), VideoDrawPlanError> {
    let pipeline = &settings.color_pipeline;
    if frame.bit_depth == BitDepth::Eight {
        let active_color_path = ActiveColorPath::from_frame(frame, pipeline);
        let conversion = VideoColorConversion::Sdr {
            encoding: yuv_encoding(&effective_shader_color(&active_color_path)),
            adjustment: color_adjustment(pipeline),
        };
        return Ok((conversion, active_color_path));
    }
    let conversion = high_bit_color_conversion(&frame.color, &settings.hdr_to_sdr)?;
    // Диагностика пути — как `active_color_path_for_host_yuv420` wgpu.
    let active_color_path = match conversion {
        VideoColorConversion::Sdr { .. } => ActiveColorPath::from_frame(frame, pipeline),
        VideoColorConversion::HdrToSdr(_) => ActiveColorPath::from_frame_with_hdr_to_sdr(
            frame,
            &ColorPipelineSettings {
                swapchain_transfer: SwapchainTransferMode::ExplicitShaderOetf,
                ..*pipeline
            },
            Some(settings.hdr_to_sdr),
        ),
    };
    Ok((conversion, active_color_path))
}

/// Цвет, который реально уходит в шейдер 8-битного SDR-пути: подмена на SDR BT.709 limited при
/// любом fallback-е цветового пути (неизвестный вход, широкий охват, HDR).
fn effective_shader_color(path: &ActiveColorPath) -> VideoColorMetadata {
    match path.fallback {
        None => path.input_color.clone(),
        Some(_) => VideoColorMetadata::sdr_bt709_limited(),
    }
}

/// Матрица и диапазон, как `coefficients_for_metadata` wgpu: BT.2020 и неизвестная матрица —
/// BT.709, неизвестный диапазон — limited.
fn yuv_encoding(color: &VideoColorMetadata) -> YuvColorEncoding {
    let matrix = match color.matrix {
        MatrixCoefficients::Bt601 => YuvMatrix::Bt601,
        MatrixCoefficients::Bt709 | MatrixCoefficients::Bt2020 | MatrixCoefficients::Unknown => {
            YuvMatrix::Bt709
        }
    };
    let range = match color.range {
        ColorRange::Full => YuvRange::Full,
        ColorRange::Limited | ColorRange::Unknown => YuvRange::Limited,
    };
    YuvColorEncoding { matrix, range }
}

fn color_adjustment(settings: &ColorPipelineSettings) -> ColorAdjustment {
    let adjustment = settings.adjustment;
    ColorAdjustment {
        brightness: adjustment.brightness,
        contrast: adjustment.contrast,
        saturation: adjustment.saturation,
        exposure: adjustment.exposure,
        rgb_gain: adjustment.rgb_gain,
        rgb_offset: adjustment.rgb_offset,
    }
}

const fn quarter_turn(orientation: VideoDisplayOrientation) -> QuarterTurn {
    match orientation {
        VideoDisplayOrientation::Identity => QuarterTurn::Identity,
        VideoDisplayOrientation::Rotate90Clockwise => QuarterTurn::Clockwise90,
        VideoDisplayOrientation::Rotate180 => QuarterTurn::Half,
        VideoDisplayOrientation::Rotate270Clockwise => QuarterTurn::Clockwise270,
    }
}

#[cfg(test)]
mod tests;
