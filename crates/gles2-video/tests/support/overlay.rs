//! Картинки оверлея для EGL-тестов слоя и эталон смешивания на CPU.

use std::sync::Arc;

use gles2_egui::{FrameStart, Gles2Context, OffscreenPixels, SurfaceSize};
use gles2_video::{
    BitmapOverlayRenderer, OverlayBitmap, OverlayBitmapRect, OverlayCanvasOrigins,
    OverlayDrawOutcome, OverlayDrawRequest, OverlayLayerOptions, OverlayPixelFormat, OverlayPixels,
    OverlayPlacementGroup, OverlayPrepareReport, OverlayTint,
};

/// Заливка цели до оверлея — «видео» стенда (тот же цвет, что у wgpu-тестов субтитров).
pub const BACKGROUND: [f32; 4] = [0.25, 0.5, 0.75, 1.0];

/// Байт хвоста строки: если слой его загрузит, пиксели уедут далеко.
pub const ROW_TAIL_GARBAGE: u8 = 0xFF;

/// Картинка теста: окно в общем буфере, как у растеризатора субтитров.
#[derive(Debug, Clone)]
pub struct TestBitmap {
    pub rect: OverlayBitmapRect,
    pub format: OverlayPixelFormat,
    pub buffer: Arc<Vec<u8>>,
    pub stride: usize,
    pub tint: OverlayTint,
    pub group: OverlayPlacementGroup,
}

pub const WHITE: OverlayTint = OverlayTint {
    red: 255,
    green: 255,
    blue: 255,
    alpha: 255,
};

pub const fn tint(red: u8, green: u8, blue: u8, alpha: u8) -> OverlayTint {
    OverlayTint {
        red,
        green,
        blue,
        alpha,
    }
}

impl TestBitmap {
    /// Пиксели из функции `(x, y) → байты пикселя`; `row_tail` байт мусора после каждой строки
    /// (у последней хвоста нет).
    pub fn from_fn(
        format: OverlayPixelFormat,
        rect: OverlayBitmapRect,
        row_tail: usize,
        pixel: impl Fn(u32, u32) -> [u8; 4],
    ) -> Self {
        let bytes_per_pixel = format.bytes_per_pixel();
        let row_bytes = rect.width as usize * bytes_per_pixel;
        let stride = row_bytes + row_tail;
        let length = (rect.height as usize - 1) * stride + row_bytes;
        let mut buffer = vec![ROW_TAIL_GARBAGE; length];
        for y in 0..rect.height {
            for x in 0..rect.width {
                let start = y as usize * stride + x as usize * bytes_per_pixel;
                let value = pixel(x, y);
                buffer[start..start + bytes_per_pixel].copy_from_slice(&value[..bytes_per_pixel]);
            }
        }
        Self {
            rect,
            format,
            buffer: Arc::new(buffer),
            stride,
            tint: WHITE,
            group: OverlayPlacementGroup::Main,
        }
    }

    /// Маска со сплошным покрытием `coverage`.
    pub fn solid_coverage(rect: OverlayBitmapRect, coverage: u8, color: OverlayTint) -> Self {
        Self::from_fn(OverlayPixelFormat::Coverage, rect, 0, |_, _| [coverage; 4]).tinted(color)
    }

    /// Сплошная RGBA-картинка, `premultiplied` — уже умноженный на альфу цвет.
    pub fn solid_picture(rect: OverlayBitmapRect, premultiplied: [u8; 4]) -> Self {
        Self::from_fn(OverlayPixelFormat::PremultipliedRgba, rect, 0, |_, _| {
            premultiplied
        })
    }

    pub fn tinted(mut self, color: OverlayTint) -> Self {
        self.tint = color;
        self
    }

    pub fn in_group(mut self, group: OverlayPlacementGroup) -> Self {
        self.group = group;
        self
    }

    /// Та же картинка (тот же буфер) в другом месте холста.
    pub fn moved_to(&self, x: i32, y: i32) -> Self {
        let mut moved = self.clone();
        moved.rect.x = x;
        moved.rect.y = y;
        moved
    }
}

impl OverlayBitmap for TestBitmap {
    fn rect(&self) -> OverlayBitmapRect {
        self.rect
    }

    fn pixels(&self) -> OverlayPixels<'_> {
        OverlayPixels {
            format: self.format,
            bytes: &self.buffer,
            stride: self.stride,
        }
    }

    fn tint(&self) -> OverlayTint {
        self.tint
    }

    fn placement_group(&self) -> OverlayPlacementGroup {
        self.group
    }

    fn shows_same_pixels_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.buffer, &other.buffer)
            && self.format == other.format
            && self.stride == other.stride
            && self.rect.width == other.rect.width
            && self.rect.height == other.rect.height
    }
}

pub const fn rect(x: i32, y: i32, width: u32, height: u32) -> OverlayBitmapRect {
    OverlayBitmapRect {
        x,
        y,
        width,
        height,
    }
}

pub fn overlay_renderer(
    context: &Gles2Context,
    options: OverlayLayerOptions,
) -> BitmapOverlayRenderer<TestBitmap> {
    BitmapOverlayRenderer::new(context, options).expect("overlay layer is created")
}

/// Холст обеих групп в одной точке.
pub const fn origins_at(x: f32, y: f32) -> OverlayCanvasOrigins {
    OverlayCanvasOrigins {
        main: [x, y],
        fixed: [x, y],
    }
}

/// Запрос «холст в `origins`, рисовать везде».
pub fn request_everywhere(
    target: SurfaceSize,
    origins: OverlayCanvasOrigins,
) -> OverlayDrawRequest {
    OverlayDrawRequest {
        origins,
        clip: super::full_rect(target),
    }
}

/// Кадр: заливка [`BACKGROUND`], `prepare` (если дан список), `draw`, показ, пиксели цели.
pub fn overlay_frame(
    context: &mut Gles2Context,
    renderer: &mut BitmapOverlayRenderer<TestBitmap>,
    bitmaps: Option<&[TestBitmap]>,
    request: &OverlayDrawRequest,
) -> (
    Option<OverlayPrepareReport>,
    OverlayDrawOutcome,
    OffscreenPixels,
) {
    let (report, outcome) = match context.begin_frame().expect("frame begins") {
        FrameStart::Ready(frame) => {
            frame.clear(BACKGROUND);
            let report = bitmaps.map(|list| renderer.prepare(&frame, list).expect("prepares"));
            let outcome = renderer.draw(&frame, request).expect("overlay draws");
            frame.present().expect("frame presents");
            (report, outcome)
        }
        FrameStart::Skipped(reason) => panic!("offscreen frame skipped: {reason:?}"),
    };
    let pixels = context.read_offscreen_pixels().expect("offscreen readback");
    (report, outcome, pixels)
}

/// Эталон «over» с премультиплицированной альфой в байтах: `src + dst·(255 − src.a)/255`.
pub fn blend_over(source: [f32; 4], destination: [u8; 4]) -> [u8; 4] {
    let mut out = [0_u8; 4];
    for channel in 0..4 {
        let value = source[channel] * 255.0 + f32::from(destination[channel]) * (1.0 - source[3]);
        out[channel] = value.round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// Выход шейдера маски (премультиплицированный, 0..1): `(rgb·cov·a, cov·a)`.
pub fn coverage_source(coverage: u8, color: OverlayTint) -> [f32; 4] {
    let alpha = f32::from(coverage) / 255.0 * f32::from(color.alpha) / 255.0;
    [
        f32::from(color.red) / 255.0 * alpha,
        f32::from(color.green) / 255.0 * alpha,
        f32::from(color.blue) / 255.0 * alpha,
        alpha,
    ]
}

/// Выход шейдера картинки: `texel·a`.
pub fn picture_source(texel: [u8; 4], tint_alpha: u8) -> [f32; 4] {
    let multiplier = f32::from(tint_alpha) / 255.0;
    texel.map(|byte| f32::from(byte) / 255.0 * multiplier)
}
