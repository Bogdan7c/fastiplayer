//! Общий стенд пиксельных тестов видеослоя на настоящем EGL без окна (llvmpipe в CI).
//!
//! Без EGL тест пропускается с причиной в stderr; `GLES2_EGUI_REQUIRE_EGL=1` превращает пропуск
//! в падение (CI ставит Mesa EGL, решение владельца 27).
//!
//! Эталон цвета — независимая CPU-транскрипция формулы wgpu-рендера плеера
//! (`render-wgpu-video/src/color_pipeline.rs` + `host_yuv420_8bit_to_rgba.wgsl`), а не константы
//! crate-а: опечатка в матрице видеослоя уронит тест.

#![allow(
    dead_code,
    reason = "каждый тестовый бинарь использует свою часть стенда"
)]

use gles2_egui::{
    CreateError, FrameStart, Gles2Context, OffscreenAlpha, OffscreenPixels, SurfaceSize,
};
use gles2_video::{
    ChromaSubsampling, ColorAdjustment, HostRows, PictureMapping, PixelRect, PlanarFrameUpload,
    PlanarLayout, PlaneExtent, PlaneSource, QuarterTurn, SampleDepth, VideoDrawOutcome,
    VideoDrawRequest, VideoFrameKey, VideoLayerOptions, VideoLayerRenderer, YuvColorEncoding,
    YuvMatrix, YuvRange,
};

pub const REQUIRE_EGL_VARIABLE: &str = "GLES2_EGUI_REQUIRE_EGL";

/// Байт, которым заполнен хвост строки: если шейдер его прочитает, цвет уедет далеко.
pub const ROW_TAIL_GARBAGE: u8 = 0xFF;

pub fn size(width: u32, height: u32) -> SurfaceSize {
    SurfaceSize::new(width, height).expect("test sizes are non-zero")
}

/// Контекст без окна или пропуск теста с причиной.
pub fn offscreen_or_skip(test_name: &str, target_size: SurfaceSize) -> Option<Gles2Context> {
    match Gles2Context::offscreen_with_alpha(target_size, OffscreenAlpha::Opaque) {
        Ok(context) => Some(context),
        Err(
            error @ (CreateError::EglUnavailable { .. }
            | CreateError::PlatformExtensionMissing { .. }
            | CreateError::DisplayOpen(_)),
        ) => {
            assert!(
                std::env::var_os(REQUIRE_EGL_VARIABLE).is_none(),
                "{test_name}: EGL is required ({REQUIRE_EGL_VARIABLE}) but unavailable: {error}"
            );
            eprintln!("skip {test_name}: no EGL surfaceless GLES2 context: {error}");
            None
        }
        Err(error) => panic!("{test_name}: offscreen GLES2 context creation failed: {error}"),
    }
}

/// Плоскость в памяти теста — как отдаёт её FFmpeg: блок кончается на видимой части последней
/// строки, хвосты остальных строк заполнены мусором.
#[derive(Debug, Clone)]
pub struct OwnedPlane {
    pub bytes: Vec<u8>,
    pub stride: usize,
    pub visible_row_bytes: usize,
    pub rows: u32,
}

impl OwnedPlane {
    fn from_fn(extent: PlaneExtent, row_tail: usize, sample: impl Fn(u32, u32) -> u8) -> Self {
        let visible_row_bytes = extent.width as usize;
        let stride = visible_row_bytes + row_tail;
        let rows = extent.height;
        let length = (rows as usize - 1) * stride + visible_row_bytes;
        let mut bytes = vec![ROW_TAIL_GARBAGE; length];
        for y in 0..rows {
            for x in 0..extent.width {
                bytes[y as usize * stride + x as usize] = sample(x, y);
            }
        }
        Self {
            bytes,
            stride,
            visible_row_bytes,
            rows,
        }
    }

    pub fn source(&self) -> PlaneSource<'_> {
        PlaneSource::HostRows(
            HostRows::new(&self.bytes, self.stride, self.visible_row_bytes, self.rows)
                .expect("test plane rows are consistent"),
        )
    }
}

/// Кадр в памяти теста.
#[derive(Debug, Clone)]
pub struct OwnedFrame {
    pub chroma: ChromaSubsampling,
    pub luma_extent: PlaneExtent,
    pub luma: OwnedPlane,
    pub chroma_u: OwnedPlane,
    pub chroma_v: OwnedPlane,
}

/// Значения отсчётов кадра: по роли и координатам в своей плоскости.
pub struct FramePattern<'a> {
    pub luma: &'a dyn Fn(u32, u32) -> u8,
    pub chroma_u: &'a dyn Fn(u32, u32) -> u8,
    pub chroma_v: &'a dyn Fn(u32, u32) -> u8,
}

impl OwnedFrame {
    /// Кадр из функций отсчётов; `row_tail` — байт мусора после видимой части каждой строки.
    pub fn from_pattern(
        chroma: ChromaSubsampling,
        luma_extent: PlaneExtent,
        row_tail: usize,
        pattern: &FramePattern<'_>,
    ) -> Self {
        let chroma_extent = chroma.chroma_extent(luma_extent);
        Self {
            chroma,
            luma_extent,
            luma: OwnedPlane::from_fn(luma_extent, row_tail, pattern.luma),
            chroma_u: OwnedPlane::from_fn(chroma_extent, row_tail, pattern.chroma_u),
            chroma_v: OwnedPlane::from_fn(chroma_extent, row_tail, pattern.chroma_v),
        }
    }

    /// Кадр одного цвета YUV.
    pub fn solid(
        chroma: ChromaSubsampling,
        luma_extent: PlaneExtent,
        row_tail: usize,
        yuv: [u8; 3],
    ) -> Self {
        Self::from_pattern(
            chroma,
            luma_extent,
            row_tail,
            &FramePattern {
                luma: &|_, _| yuv[0],
                chroma_u: &|_, _| yuv[1],
                chroma_v: &|_, _| yuv[2],
            },
        )
    }

    pub fn upload(&self, key: u64) -> PlanarFrameUpload<'_> {
        PlanarFrameUpload {
            key: VideoFrameKey(key),
            layout: PlanarLayout {
                chroma: self.chroma,
                depth: SampleDepth::Bits8,
            },
            luma_extent: self.luma_extent,
            luma: self.luma.source(),
            chroma_u: self.chroma_u.source(),
            chroma_v: self.chroma_v.source(),
        }
    }

    /// Видимые байты всех плоскостей без хвостов строк (столько записывает загрузка с
    /// `GL_UNPACK_ROW_LENGTH`).
    pub fn visible_bytes(&self) -> usize {
        [&self.luma, &self.chroma_u, &self.chroma_v]
            .iter()
            .map(|plane| plane.visible_row_bytes * plane.rows as usize)
            .sum()
    }

    /// Байты блоков плоскостей вместе с хвостами строк, кроме последней (столько записывает
    /// загрузка в текстуры шириной в stride).
    pub fn block_bytes(&self) -> usize {
        [&self.luma, &self.chroma_u, &self.chroma_v]
            .iter()
            .map(|plane| plane.bytes.len())
            .sum()
    }
}

pub fn extent(width: u32, height: u32) -> PlaneExtent {
    PlaneExtent { width, height }
}

/// Запрос «картинка на всю цель без поворота».
pub fn fill_request<'a>(
    target: SurfaceSize,
    visible_rects: &'a [PixelRect],
    color: YuvColorEncoding,
    adjustment: ColorAdjustment,
) -> VideoDrawRequest<'a> {
    VideoDrawRequest {
        viewport: full_rect(target),
        visible_rects,
        picture: PictureMapping::FILL,
        orientation: QuarterTurn::Identity,
        color,
        adjustment,
    }
}

pub fn full_rect(target: SurfaceSize) -> PixelRect {
    PixelRect {
        x: 0,
        y: 0,
        width: target.width(),
        height: target.height(),
    }
}

/// Видеослой с настройками по умолчанию.
pub fn renderer(context: &Gles2Context, options: VideoLayerOptions) -> VideoLayerRenderer {
    VideoLayerRenderer::new(context, options).expect("video layer is created")
}

/// Кадр показа: начать, нарисовать видео, показать, отдать fence. Возвращает пиксели цели.
pub fn draw_frame(
    context: &mut Gles2Context,
    renderer: &mut VideoLayerRenderer,
    request: &VideoDrawRequest<'_>,
) -> (VideoDrawOutcome, OffscreenPixels) {
    let outcome = match context.begin_frame().expect("frame begins") {
        FrameStart::Ready(frame) => {
            let outcome = renderer.draw(&frame, request).expect("video draws");
            frame.present().expect("frame presents");
            outcome
        }
        FrameStart::Skipped(reason) => panic!("offscreen frame skipped: {reason:?}"),
    };
    let fence = context.insert_fence().expect("fence");
    renderer.retire_submitted_work(std::sync::Arc::new(fence));
    let pixels = context.read_offscreen_pixels().expect("offscreen readback");
    (outcome, pixels)
}

pub fn pixel(pixels: &OffscreenPixels, x: u32, y: u32) -> [u8; 4] {
    pixels.pixel(x, y).expect("pixel inside target")
}

pub fn assert_close(actual: [u8; 4], expected: [u8; 4], tolerance: u8, what: &str) {
    for channel in 0..4 {
        let difference = actual[channel].abs_diff(expected[channel]);
        assert!(
            difference <= tolerance,
            "{what}: {actual:?} vs expected {expected:?} (±{tolerance})"
        );
    }
}

/// Все пиксели прямоугольника равны `expected` с допуском.
pub fn assert_rect(
    pixels: &OffscreenPixels,
    rect: PixelRect,
    expected: [u8; 4],
    tolerance: u8,
    what: &str,
) {
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            assert_close(
                pixel(pixels, x, y),
                expected,
                tolerance,
                &format!("{what} at ({x},{y})"),
            );
        }
    }
}

// --- Эталон цвета (транскрипция wgpu) -------------------------------------------------------

/// Эталонный RGBA8 для отсчёта YUV без фильтрации (однотонный кадр).
pub fn reference_rgba(
    encoding: YuvColorEncoding,
    adjustment: ColorAdjustment,
    yuv: [u8; 3],
) -> [u8; 4] {
    let [y, u, v] = yuv.map(|sample| f32::from(sample) / 255.0);
    let (luma_offset, luma_scale) = match encoding.range {
        YuvRange::Limited => (0.0625_f32, 1.164_383_6_f32),
        YuvRange::Full => (0.0, 1.0),
    };
    let rows: [[f32; 3]; 3] = match (encoding.matrix, encoding.range) {
        (YuvMatrix::Bt709, YuvRange::Limited) => [
            [1.0, 0.0, 1.792_741_1],
            [1.0, -0.213_248_61, -0.532_909_33],
            [1.0, 2.112_401_7, 0.0],
        ],
        (YuvMatrix::Bt709, YuvRange::Full) => [
            [1.0, 0.0, 1.574_8],
            [1.0, -0.187_324_27, -0.468_124_27],
            [1.0, 1.855_6, 0.0],
        ],
        (YuvMatrix::Bt601, YuvRange::Limited) => [
            [1.0, 0.0, 1.596_027_1],
            [1.0, -0.391_762_3, -0.812_968],
            [1.0, 2.017_232_2, 0.0],
        ],
        (YuvMatrix::Bt601, YuvRange::Full) => [
            [1.0, 0.0, 1.402],
            [1.0, -0.344_136_3, -0.714_136_3],
            [1.0, 1.772, 0.0],
        ],
    };
    let weights: [f32; 3] = match encoding.matrix {
        YuvMatrix::Bt709 => [0.212_6, 0.715_2, 0.072_2],
        YuvMatrix::Bt601 => [0.299, 0.587, 0.114],
    };
    let normalized = [(y - luma_offset) * luma_scale, u - 0.5, v - 0.5];
    let rgb =
        rows.map(|row| row[0] * normalized[0] + row[1] * normalized[1] + row[2] * normalized[2]);
    let adjusted = apply_sdr_adjustments(rgb, weights, adjustment);
    let to_byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    [
        to_byte(adjusted[0]),
        to_byte(adjusted[1]),
        to_byte(adjusted[2]),
        255,
    ]
}

/// `apply_sdr_adjustments` из WGSL: насыщенность → контраст → экспозиция → яркость →
/// gain/offset → clamp.
fn apply_sdr_adjustments(
    rgb: [f32; 3],
    weights: [f32; 3],
    adjustment: ColorAdjustment,
) -> [f32; 3] {
    let luma = rgb[0] * weights[0] + rgb[1] * weights[1] + rgb[2] * weights[2];
    let exposure = adjustment.exposure.exp2();
    let mut result = [0.0; 3];
    for channel in 0..3 {
        let saturated = luma + (rgb[channel] - luma) * adjustment.saturation;
        let contrasted = (saturated - 0.5) * adjustment.contrast + 0.5;
        let exposed = contrasted * exposure;
        let adjusted = (exposed + adjustment.brightness) * adjustment.rgb_gain[channel]
            + adjustment.rgb_offset[channel];
        result[channel] = adjusted.clamp(0.0, 1.0);
    }
    result
}
