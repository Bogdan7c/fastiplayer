//! Минимальный пример для любого egui-приложения: окно winit, контекст EGL/GLES2
//! (`gles2-egui`), видеослой из памяти процесса (`gles2-video`) и интерфейс egui поверх видео.
//!
//! Кадры YUV 4:2:0 8 бит рисует сам пример на CPU (бегущая полоса на градиенте) — так видно
//! путь «буфер в памяти → одна копия в текстуру → шейдер YUV→RGB → экран» без декодера.
//! Ввод мыши и клавиатуры идёт в egui через `egui-winit`: ползунок яркости и пауза работают.
//!
//! Запуск:
//! ```text
//! cargo run --release -p gles2-video --example egui_video_window -- [--scripted-check]
//! ```
//! - без флагов — интерактивное окно, `Esc` — выход;
//! - `--scripted-check` — 120 кадров без участия человека: проверяет, что каждый показанный
//!   кадр нарисовал видео и что кадры действительно загружались; печатает `scripted check: PASS`.
//!
//! Режим окна выбирает winit: нативный Wayland при `WAYLAND_DISPLAY`, X11/XWayland — через
//! `env -u WAYLAND_DISPLAY …`.

use std::sync::Arc;

use egui::ViewportId;
use gles2_egui::{
    EguiPainterOptions, FrameCompositor, Gles2Context, PixelExtent, PresentationSettings,
    UiContent, UiFrameOutcome, UiFrameRequest, UiScreen, WindowSource,
};
use gles2_video::{
    ChromaSubsampling, ColorAdjustment, HostRows, PictureMapping, PixelRect, PlanarFrameUpload,
    PlanarLayout, PlaneExtent, PlaneSource, PlaneSourceError, QuarterTurn, SampleDepth,
    UploadOutcome, VideoColorConversion, VideoDrawOutcome, VideoDrawRequest, VideoFrameKey,
    VideoLayerOptions, VideoLayerRenderer, VideoUnderlay, YuvColorEncoding,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

/// Размер синтетического «видео». 16:9 в окне 16:10 — видны полосы letterbox.
const VIDEO_WIDTH: u32 = 640;
const VIDEO_HEIGHT: u32 = 360;
/// Байт в строке яркости (8 бит на отсчёт, строки без хвостов).
const LUMA_ROW_BYTES: usize = VIDEO_WIDTH as usize;
/// Сколько кадров показывает `--scripted-check`: 2 с при 60 Гц.
const SCRIPTED_CHECK_FRAMES: u32 = 120;
/// Фон под всем кадром (премультиплицированный RGBA). Видеослой всё равно заливает цель
/// чёрным, фон нужен для кадров без видео.
const BACKGROUND: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    Interactive,
    ScriptedCheck,
}

impl RunMode {
    fn from_args(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        match (args.next().as_deref(), args.next()) {
            (None, _) => Ok(Self::Interactive),
            (Some("--scripted-check"), None) => Ok(Self::ScriptedCheck),
            _ => Err("usage: egui_video_window [--scripted-check]".to_owned()),
        }
    }
}

/// Кадр YUV 4:2:0 8 бит в памяти процесса: три плоскости без хвостов строк.
struct SyntheticYuvFrame {
    luma: Vec<u8>,
    chroma_u: Vec<u8>,
    chroma_v: Vec<u8>,
}

impl SyntheticYuvFrame {
    fn new() -> Self {
        let luma_bytes = (VIDEO_WIDTH * VIDEO_HEIGHT) as usize;
        let chroma_bytes = (chroma_width() * chroma_height()) as usize;
        Self {
            luma: vec![0; luma_bytes],
            chroma_u: vec![128; chroma_bytes],
            chroma_v: vec![128; chroma_bytes],
        }
    }

    /// Рисует кадр номер `frame_index`: горизонтальный градиент яркости, белая полоса бежит
    /// слева направо, оттенок цветности медленно меняется. Значения — limited range (16…235).
    fn paint(&mut self, frame_index: u64) {
        let bar_x = (frame_index * 4) % u64::from(VIDEO_WIDTH);
        // Плоскость без хвостов строк: ровно `VIDEO_HEIGHT` строк по `LUMA_ROW_BYTES` байт.
        let (rows, _no_tail) = self.luma.as_chunks_mut::<LUMA_ROW_BYTES>();
        for (row_index, row) in rows.iter_mut().enumerate() {
            for (column, sample) in row.iter_mut().enumerate() {
                let on_bar = (column as u64).abs_diff(bar_x) < 8;
                let gradient = 16 + (column * 219 / VIDEO_WIDTH as usize) as u8;
                // Тонкие тёмные линии каждые 45 строк — чтобы было видно масштаб и letterbox.
                let grid_line = row_index % 45 == 0;
                *sample = match (on_bar, grid_line) {
                    (true, _) => 235,
                    (false, true) => 16,
                    (false, false) => gradient,
                };
            }
        }
        let hue = (frame_index % 256) as u8;
        self.chroma_u.fill(hue);
        self.chroma_v.fill(255 - hue);
    }

    /// Описание кадра для `VideoLayerRenderer::upload`. Байты не копируются здесь: одна копия
    /// в текстуру происходит внутри `upload`.
    fn upload_request(
        &self,
        key: VideoFrameKey,
    ) -> Result<PlanarFrameUpload<'_>, PlaneSourceError> {
        let luma_row = LUMA_ROW_BYTES;
        let chroma_row = chroma_width() as usize;
        Ok(PlanarFrameUpload {
            key,
            layout: PlanarLayout {
                chroma: ChromaSubsampling::Yuv420,
                depth: SampleDepth::Bits8,
            },
            luma_extent: PlaneExtent {
                width: VIDEO_WIDTH,
                height: VIDEO_HEIGHT,
            },
            luma: PlaneSource::HostRows(HostRows::new(
                &self.luma,
                luma_row,
                luma_row,
                VIDEO_HEIGHT,
            )?),
            chroma_u: PlaneSource::HostRows(HostRows::new(
                &self.chroma_u,
                chroma_row,
                chroma_row,
                chroma_height(),
            )?),
            chroma_v: PlaneSource::HostRows(HostRows::new(
                &self.chroma_v,
                chroma_row,
                chroma_row,
                chroma_height(),
            )?),
        })
    }
}

/// Ширина плоскости цветности 4:2:0: половина яркости с округлением вверх.
const fn chroma_width() -> u32 {
    VIDEO_WIDTH.div_ceil(2)
}

/// Высота плоскости цветности 4:2:0: половина яркости с округлением вверх.
const fn chroma_height() -> u32 {
    VIDEO_HEIGHT.div_ceil(2)
}

/// Letterbox: картинка `picture_width × picture_height` целиком внутри `viewport` с сохранением
/// пропорций. `display_uv = viewport_uv × uv_scale + uv_offset`; вне `[0, 1]` — полосы.
fn letterbox_mapping(
    viewport: PixelRect,
    picture_width: u32,
    picture_height: u32,
) -> PictureMapping {
    if viewport.width == 0 || viewport.height == 0 {
        return PictureMapping::FILL;
    }
    let viewport_aspect = viewport.width as f32 / viewport.height as f32;
    let picture_aspect = picture_width as f32 / picture_height as f32;
    // Доля области видео, которую картинка занимает по горизонтали и по вертикали.
    let (fraction_x, fraction_y) = if picture_aspect > viewport_aspect {
        (1.0, viewport_aspect / picture_aspect)
    } else {
        (picture_aspect / viewport_aspect, 1.0)
    };
    PictureMapping {
        uv_scale: [1.0 / fraction_x, 1.0 / fraction_y],
        uv_offset: [
            -(1.0 - fraction_x) / (2.0 * fraction_x),
            -(1.0 - fraction_y) / (2.0 * fraction_y),
        ],
    }
}

/// Что меняет интерфейс egui.
struct VideoControls {
    brightness: f32,
    paused: bool,
}

/// Счётчики `--scripted-check`: проверяется результат, а не вызовы.
#[derive(Debug, Default)]
struct FrameTally {
    presented: u32,
    presented_with_video: u32,
    uploaded: u32,
    busy: u32,
}

/// Всё, что живёт вместе с контекстом GLES2. Порядок уничтожения важен: GL-объекты слоёв —
/// до контекста (см. `destroy`).
struct GpuState {
    context: Gles2Context,
    compositor: FrameCompositor,
    video: VideoLayerRenderer,
    egui_input: egui_winit::State,
}

impl GpuState {
    fn create(window: &Arc<Window>, egui_context: &egui::Context) -> Result<Self, String> {
        let size = window.inner_size();
        let source: Arc<dyn WindowSource> = window.clone();
        let context = Gles2Context::for_window(
            source,
            PixelExtent {
                width: size.width,
                height: size.height,
            },
            PresentationSettings::default(),
        )
        .map_err(|error| format!("GLES2 context creation failed: {error}"))?;
        let compositor = FrameCompositor::new(&context, EguiPainterOptions::default())
            .map_err(|error| format!("frame compositor creation failed: {error}"))?;
        let video = VideoLayerRenderer::new(&context, VideoLayerOptions::default())
            .map_err(|error| format!("video layer creation failed: {error}"))?;
        let egui_input = egui_winit::State::new(
            egui_context.clone(),
            ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            Some(compositor.painter().max_texture_side()),
        );
        println!(
            "renderer: {} | {} | capability tiers: {:?}",
            context.capabilities().gl_renderer(),
            context.capabilities().gl_version_string(),
            context.capabilities().tiers()
        );
        Ok(Self {
            context,
            compositor,
            video,
            egui_input,
        })
    }

    /// Загружает кадр в кольцо текстур. Делается до начала кадра показа: тот же ключ на паузе —
    /// `AlreadyCurrent`, без копии; `Busy` — все слоты ещё читает GPU, на экране прежний кадр.
    fn upload_frame(
        &mut self,
        frame: &SyntheticYuvFrame,
        key: VideoFrameKey,
    ) -> Result<UploadOutcome, String> {
        let request = frame
            .upload_request(key)
            .map_err(|error| format!("frame description rejected: {error}"))?;
        self.video
            .upload(&self.context, &request)
            .map_err(|error| format!("video upload failed: {error}"))
    }

    /// Проход egui: ввод из `egui-winit`, интерфейс, тесселяция.
    fn run_egui_pass(
        &mut self,
        window: &Window,
        egui_context: &egui::Context,
        controls: &mut VideoControls,
    ) -> UiContent {
        let raw_input = self.egui_input.take_egui_input(window);
        let gl_renderer = self.context.capabilities().gl_renderer().to_owned();
        let output = egui_context.run_ui(raw_input, |ui| show_controls(ui, controls, &gl_renderer));
        self.egui_input
            .handle_platform_output(window, output.platform_output);
        UiContent::Fresh {
            primitives: egui_context.tessellate(output.shapes, output.pixels_per_point),
            textures_delta: output.textures_delta,
        }
    }

    /// Собирает и показывает кадр: видео на всё окно (letterbox), egui поверх — порядок проходов
    /// задаёт `FrameCompositor`. Возвращает итог кадра и что сделал видеослой.
    fn compose_frame(
        &mut self,
        screen: UiScreen,
        content: UiContent,
        brightness: f32,
    ) -> Result<(UiFrameOutcome, Option<VideoDrawOutcome>), String> {
        let viewport = PixelRect {
            x: 0,
            y: 0,
            width: screen.size_in_pixels[0],
            height: screen.size_in_pixels[1],
        };
        let visible_rects = [viewport];
        let mut underlay = VideoUnderlay::new(
            &mut self.video,
            VideoDrawRequest {
                viewport,
                visible_rects: &visible_rects,
                picture: letterbox_mapping(viewport, VIDEO_WIDTH, VIDEO_HEIGHT),
                orientation: QuarterTurn::Identity,
                color: VideoColorConversion::Sdr {
                    encoding: YuvColorEncoding::SDR_BT709_LIMITED,
                    adjustment: ColorAdjustment {
                        brightness,
                        ..ColorAdjustment::IDENTITY
                    },
                },
            },
        );
        let frame = self
            .compositor
            .render(
                &mut self.context,
                UiFrameRequest {
                    screen,
                    content,
                    background: BACKGROUND,
                    layers: &mut underlay,
                },
            )
            .map_err(|error| format!("frame failed: {error}"))?;
        Ok((frame, underlay.outcome()))
    }

    /// Fence прикрывает работу кадра: слот кольца, из которого GPU читал кадр, освободится только
    /// после него. Без этого вызова следующие загрузки ответят `Busy`. Тот же `Arc<GpuFence>`
    /// приложение с настоящим декодером отдало бы и освобождению буфера декодера.
    fn retire_frame_work(&mut self) -> Result<(), String> {
        let fence = self
            .context
            .insert_fence()
            .map_err(|error| format!("frame fence failed: {error}"))?;
        self.video.retire_submitted_work(Arc::new(fence));
        Ok(())
    }

    /// GL-объекты слоёв удаляются, пока контекст ещё текущий; контекст — последним.
    fn destroy(self) {
        let Self {
            context,
            compositor,
            video,
            ..
        } = self;
        if let Err(error) = video.destroy(&context) {
            eprintln!("video layer destroy failed: {error}");
        }
        if let Err(error) = compositor.destroy(&context) {
            eprintln!("compositor destroy failed: {error}");
        }
    }
}

struct VideoWindowApp {
    mode: RunMode,
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    egui_context: egui::Context,
    frame: SyntheticYuvFrame,
    frame_index: u64,
    controls: VideoControls,
    tally: FrameTally,
    failure: Option<String>,
}

impl VideoWindowApp {
    fn new(mode: RunMode) -> Self {
        Self {
            mode,
            window: None,
            gpu: None,
            egui_context: egui::Context::default(),
            frame: SyntheticYuvFrame::new(),
            frame_index: 0,
            controls: VideoControls {
                brightness: 0.0,
                paused: false,
            },
            tally: FrameTally::default(),
            failure: None,
        }
    }

    /// Один кадр: новый кадр видео → загрузка → проход egui → сборка кадра → fence.
    fn redraw(&mut self) -> Result<(), String> {
        let (Some(window), Some(gpu)) = (&self.window, self.gpu.as_mut()) else {
            return Ok(());
        };
        if !self.controls.paused {
            self.frame_index += 1;
            self.frame.paint(self.frame_index);
        }
        let upload = gpu.upload_frame(&self.frame, VideoFrameKey(self.frame_index))?;
        self.tally.record_upload(upload);
        let content = gpu.run_egui_pass(window, &self.egui_context, &mut self.controls);
        let screen = UiScreen {
            size_in_pixels: [gpu.context.extent().width, gpu.context.extent().height],
            pixels_per_point: window.scale_factor() as f32,
        };
        let (frame, video) = gpu.compose_frame(screen, content, self.controls.brightness)?;
        self.tally.record_frame(frame, video)?;
        gpu.retire_frame_work()
    }

    fn finish_scripted_check(&self) -> Result<(), String> {
        let tally = &self.tally;
        if tally.presented_with_video != tally.presented {
            return Err(format!("video missing on some frames: {tally:?}"));
        }
        // Кольцо из трёх слотов может изредка ответить `Busy`, но кадры обязаны меняться.
        if tally.uploaded < SCRIPTED_CHECK_FRAMES / 2 {
            return Err(format!("too few frames uploaded: {tally:?}"));
        }
        println!("scripted check: PASS ({tally:?})");
        Ok(())
    }
}

impl FrameTally {
    /// Учитывает итог загрузки кадра.
    fn record_upload(&mut self, upload: UploadOutcome) {
        match upload {
            UploadOutcome::Uploaded(_) => self.uploaded += 1,
            UploadOutcome::AlreadyCurrent => {}
            UploadOutcome::Busy(_) => self.busy += 1,
        }
    }

    /// Учитывает итог кадра. Отказ кадра (`Dropped`) в примере — ошибка: приложение отдаёт
    /// свежий проход egui каждый кадр, удерживать нечего.
    fn record_frame(
        &mut self,
        frame: UiFrameOutcome,
        video: Option<VideoDrawOutcome>,
    ) -> Result<(), String> {
        match frame {
            UiFrameOutcome::Presented(_) => {
                self.presented += 1;
                if matches!(video, Some(VideoDrawOutcome::Drawn { .. })) {
                    self.presented_with_video += 1;
                }
                Ok(())
            }
            // Окно нулевого размера или свёрнуто: показывать некуда, это не ошибка.
            UiFrameOutcome::Skipped(_) => Ok(()),
            UiFrameOutcome::Dropped(reason) => Err(format!("frame dropped: {reason:?}")),
        }
    }
}

/// Интерфейс поверх видео: имя драйвера, яркость и пауза.
fn show_controls(ui: &mut egui::Ui, controls: &mut VideoControls, gl_renderer: &str) {
    let translucent = egui::Frame::new()
        .fill(egui::Color32::from_black_alpha(170))
        .inner_margin(8.0);
    egui::Panel::bottom("controls")
        .frame(translucent)
        .show(ui, |ui| {
            ui.label(format!("OpenGL ES 2.0 on {gl_renderer}"));
            ui.horizontal(|ui| {
                ui.checkbox(&mut controls.paused, "Pause");
                ui.add(egui::Slider::new(&mut controls.brightness, -0.3..=0.3).text("Brightness"));
            });
        });
}

impl ApplicationHandler for VideoWindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("gles2-video: egui over host-memory video")
            .with_inner_size(LogicalSize::new(960.0, 600.0));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.failure = Some(format!("window creation failed: {error}"));
                event_loop.exit();
                return;
            }
        };
        match GpuState::create(&window, &self.egui_context) {
            Ok(gpu) => self.gpu = Some(gpu),
            Err(error) => {
                self.failure = Some(error);
                event_loop.exit();
                return;
            }
        }
        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        // Сначала событие видит egui (мышь, клавиатура, масштаб).
        if let (Some(window), Some(gpu)) = (&self.window, self.gpu.as_mut()) {
            let _response = gpu.egui_input.on_window_event(window, &event);
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.physical_key == PhysicalKey::Code(KeyCode::Escape) =>
            {
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut()
                    && let Err(error) = gpu.context.resize(PixelExtent {
                        width: size.width,
                        height: size.height,
                    })
                {
                    self.failure = Some(format!("resize failed: {error}"));
                    event_loop.exit();
                }
            }
            WindowEvent::RedrawRequested => self.on_redraw_requested(event_loop),
            _ => {}
        }
    }

    fn exiting(&mut self, _: &ActiveEventLoop) {
        if let Some(gpu) = self.gpu.take() {
            gpu.destroy();
        }
    }
}

impl VideoWindowApp {
    fn on_redraw_requested(&mut self, event_loop: &ActiveEventLoop) {
        if let Err(error) = self.redraw() {
            self.failure = Some(error);
            event_loop.exit();
            return;
        }
        if self.mode == RunMode::ScriptedCheck && self.tally.presented >= SCRIPTED_CHECK_FRAMES {
            if let Err(error) = self.finish_scripted_check() {
                self.failure = Some(error);
            }
            event_loop.exit();
            return;
        }
        // Видео идёт непрерывно: следующий кадр сразу; темп задаёт vsync в `swap_buffers`.
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

fn main() {
    let mode = match RunMode::from_args(std::env::args().skip(1)) {
        Ok(mode) => mode,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("event loop creation failed: {error}");
            std::process::exit(1);
        }
    };
    let mut app = VideoWindowApp::new(mode);
    if let Err(error) = event_loop.run_app(&mut app) {
        eprintln!("event loop failed: {error}");
        std::process::exit(1);
    }
    if let Some(failure) = app.failure {
        eprintln!("{failure}");
        std::process::exit(1);
    }
}
