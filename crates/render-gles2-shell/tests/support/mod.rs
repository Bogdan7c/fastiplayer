//! Стенд функциональных тестов аппаратного кадра: настоящий EGL без окна, настоящие буферы
//! ядра (`udmabuf`), провайдер под обёрткой освобождения по fence и показ в цель.
//!
//! Без EGL или `/dev/udmabuf` тест пропускается с причиной в stderr; `GLES2_EGUI_REQUIRE_EGL=1`
//! превращает отсутствие EGL в падение.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "тестовый стенд: громкие падения, каждый тестовый бинарь берёт часть стенда"
)]

pub mod nv12_buffers;
pub mod provider;
pub mod udmabuf;

use std::sync::Arc;
use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use gles2_egui::{
    CreateError, FrameStart, Gles2Context, GpuFence, OffscreenAlpha, OffscreenPixels, SurfaceSize,
};
use gles2_video::{VideoDrawOutcome, VideoLayerOptions, VideoLayerRenderer};
use render_core::{PictureLayout, RenderLiveSettings, RenderViewport};
use render_gles2_shell::{
    Gles2FrameReleaseBinding, VideoDrawPlan, VideoPlacement, renderable_frame,
    wrap_video_backend_for_gles2_release,
};
use video_backend_api::{PresentFrameResourceProviderHandle, StartedVideoBackend};
use video_core::{DecodedFrame, FrameResourceHandle, VideoFrameDiagnostics};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use nv12_buffers::{LUMA_HEIGHT, LUMA_WIDTH, Nv12Layout, PALETTE, TestNv12};
use provider::{ProviderOnlyDecoder, ProviderState, ScriptedProvider};

const REQUIRE_EGL_VARIABLE: &str = "GLES2_EGUI_REQUIRE_EGL";
pub const TARGET_WIDTH: u32 = 64;
pub const TARGET_HEIGHT: u32 = 48;
/// Допуск на округление `f32` в шейдере против `f32` эталона.
pub const TOLERANCE: u8 = 1;
pub const WAIT: Duration = Duration::from_secs(5);

/// Контекст без окна или пропуск теста с причиной.
pub fn offscreen_or_skip(test_name: &str) -> Option<Gles2Context> {
    let size = SurfaceSize::new(TARGET_WIDTH, TARGET_HEIGHT).unwrap();
    match Gles2Context::offscreen_with_alpha(size, OffscreenAlpha::Opaque) {
        Ok(context) => Some(context),
        Err(
            error @ (CreateError::EglUnavailable { .. }
            | CreateError::PlatformExtensionMissing { .. }
            | CreateError::DisplayOpen(_)),
        ) => {
            assert!(
                std::env::var_os(REQUIRE_EGL_VARIABLE).is_none(),
                "{test_name}: EGL is required but unavailable: {error}"
            );
            eprintln!("skip {test_name}: no EGL surfaceless GLES2 context: {error}");
            None
        }
        Err(error) => panic!("{test_name}: offscreen context failed: {error}"),
    }
}

/// Контракт кадра, соответствующий раскладке буферов.
pub fn contract_for(layout: Nv12Layout) -> VideoFrameContract {
    VideoFrameContract::dma_buf_nv12(match layout {
        Nv12Layout::Composed => DmaBufImageLayout::ComposedLayers,
        Nv12Layout::Separate => DmaBufImageLayout::SeparateLayers,
    })
}

/// Кадр декодера 32×16 с контрактом `contract` и номером ресурса `handle`.
pub fn decoded_frame(contract: VideoFrameContract, handle: u64) -> DecodedFrame {
    DecodedFrame {
        generation: 1,
        pts: Duration::ZERO,
        frame_contract: contract,
        width: LUMA_WIDTH,
        height: LUMA_HEIGHT,
        render_width: LUMA_WIDTH,
        render_height: LUMA_HEIGHT,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle: FrameResourceHandle(handle),
        diagnostics: VideoFrameDiagnostics::default(),
    }
}

/// Результат показа кадра в цель.
pub struct Presented {
    pub outcome: VideoDrawOutcome,
    pub pixels: OffscreenPixels,
    pub fence: Arc<GpuFence>,
}

/// Всё, что собирает плеер вокруг аппаратного кадра, кроме приложения.
pub struct Rig {
    pub context: Gles2Context,
    pub video: VideoLayerRenderer,
    pub binding: Gles2FrameReleaseBinding,
    /// Обёрнутый провайдер: тот, что получает рендер и аренда кадра в плеере.
    pub provider: PresentFrameResourceProviderHandle,
    /// Журнал внутреннего провайдера.
    pub state: Arc<ProviderState>,
    pub layout: Nv12Layout,
    /// Держит декодер (а с ним внутренний провайдер) живым.
    _backend: StartedVideoBackend,
}

impl Rig {
    /// Стенд с буферами `layout`; `None` — пропуск теста (нет EGL или `/dev/udmabuf`).
    pub fn new(test_name: &str, layout: Nv12Layout) -> Option<Self> {
        let context = offscreen_or_skip(test_name)?;
        let nv12 = TestNv12::create(test_name, layout, &PALETTE)?;
        let state = ProviderState::new(nv12, 500);
        let inner = PresentFrameResourceProviderHandle::new(ScriptedProvider(Arc::clone(&state)));
        let binding = Gles2FrameReleaseBinding::new();
        let (backend, provider) = wrap_video_backend_for_gles2_release(
            StartedVideoBackend::from_decoder_thread(
                "dma-buf-test",
                ProviderOnlyDecoder { provider: inner },
            ),
            &binding,
        );
        let video = VideoLayerRenderer::new(&context, VideoLayerOptions::default()).unwrap();
        Some(Self {
            context,
            video,
            binding,
            provider,
            state,
            layout,
            _backend: backend,
        })
    }

    /// Нарисовать текущий кадр видеослоя (letterbox из `render-core`), показать, поставить
    /// fence и опубликовать его привязке освобождения — как делает поток рендера плеера.
    pub fn draw_and_present(&mut self, frame: &DecodedFrame) -> Presented {
        let renderable = renderable_frame(frame).unwrap();
        let plan = VideoDrawPlan::for_frame(
            &renderable,
            VideoPlacement {
                surface_size: (TARGET_WIDTH, TARGET_HEIGHT),
                video_viewport: RenderViewport::full_surface(TARGET_WIDTH, TARGET_HEIGHT),
                exclusions: &[],
            },
            &RenderLiveSettings::default(),
        )
        .unwrap();
        let outcome = match self.context.begin_frame().unwrap() {
            FrameStart::Ready(active) => {
                let outcome = self.video.draw(&active, &plan.request()).unwrap();
                active.present().unwrap();
                outcome
            }
            FrameStart::Skipped(reason) => panic!("offscreen frame skipped: {reason:?}"),
        };
        let fence = Arc::new(self.context.insert_fence().unwrap());
        self.video.retire_submitted_work(Arc::clone(&fence));
        self.binding
            .publish_presented_frame_fence(Arc::clone(&fence));
        let pixels = self.context.read_offscreen_pixels().unwrap();
        Presented {
            outcome,
            pixels,
            fence,
        }
    }
}

/// Прямоугольник картинки на цели (letterbox из `render-core`).
pub fn picture_rect(frame: &DecodedFrame) -> render_core::PictureRect {
    PictureLayout::fit_frame(
        &renderable_frame(frame).unwrap(),
        RenderViewport::new(0, 0, TARGET_WIDTH, TARGET_HEIGHT),
    )
    .unwrap()
    .rect()
}

pub fn pixel(pixels: &OffscreenPixels, x: u32, y: u32) -> [u8; 4] {
    pixels.pixel(x, y).expect("pixel inside target")
}

pub fn assert_close(actual: [u8; 4], expected: [u8; 4], what: &str) {
    for channel in 0..4 {
        assert!(
            actual[channel].abs_diff(expected[channel]) <= TOLERANCE,
            "{what}: {actual:?} vs expected {expected:?} (±{TOLERANCE})"
        );
    }
}

/// Эталонный RGBA8 для YUV8 по формуле wgpu-рендера плеера для SDR BT.709 limited
/// (`render-wgpu-video/src/color_pipeline.rs` + шейдер NV12): независимая CPU-транскрипция,
/// а не константы видеослоя.
pub fn reference_rgba_bt709_limited(yuv: [u8; 3]) -> [u8; 4] {
    let [y, u, v] = yuv.map(|sample| f32::from(sample) / 255.0);
    let rows: [[f32; 3]; 3] = [
        [1.0, 0.0, 1.792_741_1],
        [1.0, -0.213_248_61, -0.532_909_33],
        [1.0, 2.112_401_7, 0.0],
    ];
    let normalized = [(y - 0.0625) * 1.164_383_6, u - 0.5, v - 0.5];
    let to_byte = |row: [f32; 3]| {
        let value = row[0] * normalized[0] + row[1] * normalized[1] + row[2] * normalized[2];
        (value.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    [to_byte(rows[0]), to_byte(rows[1]), to_byte(rows[2]), 255]
}

/// Центры блоков 8×8 исходного кадра на цели, при картинке `picture`.
pub fn block_centers(picture: render_core::PictureRect) -> Vec<(usize, u32, u32)> {
    let scale_x = picture.width / LUMA_WIDTH as f32;
    let scale_y = picture.height / LUMA_HEIGHT as f32;
    (0..8)
        .map(|index| {
            let (block_x, block_y) = ((index % 4) as f32, (index / 4) as f32);
            let x = picture.x + (block_x * 8.0 + 4.0) * scale_x;
            let y = picture.y + (block_y * 8.0 + 4.0) * scale_y;
            (index, x as u32, y as u32)
        })
        .collect()
}

/// Цвета палитры в центрах блоков и чёрные полосы letterbox.
pub fn assert_palette_on_screen(presented: &Presented, frame: &DecodedFrame, what: &str) {
    let picture = picture_rect(frame);
    for (index, x, y) in block_centers(picture) {
        assert_close(
            pixel(&presented.pixels, x, y),
            reference_rgba_bt709_limited(PALETTE[index]),
            &format!("{what}: block {index} at ({x},{y})"),
        );
    }
    // Полосы letterbox вне картинки чёрные.
    assert!(picture.y >= 2.0, "test target must have letterbox bars");
    for (x, y) in [(2, 1), (TARGET_WIDTH - 3, 1), (2, TARGET_HEIGHT - 2)] {
        assert_close(
            pixel(&presented.pixels, x, y),
            [0, 0, 0, 255],
            &format!("{what}: letterbox bar at ({x},{y})"),
        );
    }
}
