//! Ручная проверка и замер художника egui на живом железе (780M, T410): окно winit,
//! интерфейс, похожий на плеер (верхняя и нижняя панели поверх «видео», открытая боковая панель
//! настроек), скруглённые углы окна.
//!
//! Запуск:
//! ```text
//! cargo run --release -p gles2-egui --example egui_window -- [--transparent] [--immediate] [--bench]
//! ```
//! - `--transparent` — прозрачное окно: углы скругляются маской (радиус 12 точек).
//! - `--immediate` — `eglSwapInterval(0)`: кадры не ждут развёртки, интервал = цена кадра.
//! - `--bench` — без клавиатуры (для ssh): 300 кадров полным проходом egui, затем 300 кадров
//!   повтора UI (`ReusePrevious`); печатает медиану и p95 стадий и выходит.
//!
//! Режим окна выбирает winit: нативный Wayland, если есть `WAYLAND_DISPLAY`; XWayland — с пустым
//! `WAYLAND_DISPLAY` (`env -u WAYLAND_DISPLAY …`). Клавиши: `R` — полный проход / повтор,
//! `Esc` — выход.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, Context, RawInput, Rect, pos2, vec2};
use gles2_egui::{
    CornerRadius, EguiPainterOptions, FrameCompositor, Gles2Context, NoFrameLayers, PixelExtent,
    PresentationSettings, UiContent, UiFrameOutcome, UiFrameRequest, UiScreen, VsyncMode,
    WindowAlphaRequest, WindowSource,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

/// Кадров на каждую фазу замера: 5 с при 60 Гц — хватает для устойчивой медианы и p95.
const BENCH_FRAMES_PER_PHASE: usize = 300;
/// Радиус углов окна, точки (умолчание плеера `ui.window.corner_radius_px`).
const CORNER_RADIUS_POINTS: f32 = 12.0;
/// «Видео» под интерфейсом — непрозрачная заливка.
const VIDEO_STAND_IN: [f32; 4] = [0.12, 0.28, 0.42, 1.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    Interactive,
    Bench,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiMode {
    FullPass,
    Reuse,
}

#[derive(Debug, Clone, Copy)]
struct ExampleOptions {
    settings: PresentationSettings,
    mode: RunMode,
}

impl ExampleOptions {
    fn from_args(arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut settings = PresentationSettings::default();
        let mut mode = RunMode::Interactive;
        for argument in arguments {
            match argument.as_str() {
                "--transparent" => settings.alpha = WindowAlphaRequest::TransparentPreferred,
                "--immediate" => settings.vsync = VsyncMode::Immediate,
                "--bench" => mode = RunMode::Bench,
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        Ok(Self { settings, mode })
    }
}

/// Замеры одного кадра.
#[derive(Debug, Clone, Copy)]
struct FrameSample {
    egui_pass: Duration,
    render_call: Duration,
    egui_buffer_update: Duration,
    egui_render: Duration,
    window_corner_mask: Duration,
    surface_present: Duration,
    interval: Option<Duration>,
    draw_calls: u32,
    streamed_bytes: usize,
}

/// Состояние «плеера» в примере: значения виджетов.
struct PlayerUiState {
    position: f32,
    volume: f32,
    subtitles: bool,
    frame_index: u64,
}

struct EguiWindowApp {
    options: ExampleOptions,
    window: Option<Arc<Window>>,
    context: Option<Gles2Context>,
    compositor: Option<FrameCompositor>,
    egui_context: Context,
    player: PlayerUiState,
    ui_mode: UiMode,
    last_present: Option<Instant>,
    samples: Vec<(UiMode, FrameSample)>,
    failure: Option<String>,
}

impl EguiWindowApp {
    fn new(options: ExampleOptions) -> Self {
        Self {
            options,
            window: None,
            context: None,
            compositor: None,
            egui_context: Context::default(),
            player: PlayerUiState {
                position: 0.35,
                volume: 0.7,
                subtitles: true,
                frame_index: 0,
            },
            ui_mode: UiMode::FullPass,
            last_present: None,
            samples: Vec::new(),
            failure: None,
        }
    }

    fn create_renderer(&mut self, window: &Arc<Window>) -> Result<(), String> {
        let size = window.inner_size();
        let source: Arc<dyn WindowSource> = window.clone();
        let context = Gles2Context::for_window(
            source,
            PixelExtent {
                width: size.width,
                height: size.height,
            },
            self.options.settings,
        )
        .map_err(|error| format!("GLES2 context creation failed: {error}"))?;
        let compositor = FrameCompositor::new(&context, EguiPainterOptions::default())
            .map_err(|error| format!("frame compositor creation failed: {error}"))?;
        println!(
            "renderer: {} | {} | presentation {:?}",
            context.capabilities().gl_renderer(),
            context.capabilities().gl_version_string(),
            context.presentation()
        );
        println!(
            "painter: u32 indices {}, VAO {}, dithering {:?}",
            compositor.painter().uses_u32_indices(),
            compositor.painter().uses_vertex_array(),
            compositor.painter().dithering()
        );
        self.context = Some(context);
        self.compositor = Some(compositor);
        Ok(())
    }

    /// Один кадр: полный проход egui или повтор удержанного.
    fn redraw(&mut self) -> Result<(), String> {
        let (Some(window), Some(context), Some(compositor)) = (
            &self.window,
            self.context.as_mut(),
            self.compositor.as_mut(),
        ) else {
            return Ok(());
        };
        let extent = context.extent();
        let pixels_per_point = window.scale_factor() as f32;
        let screen = UiScreen {
            size_in_pixels: [extent.width, extent.height],
            pixels_per_point,
        };
        let wants_reuse = self.ui_mode == UiMode::Reuse && compositor.can_reuse_retained_ui(screen);
        let egui_started = Instant::now();
        let content = if wants_reuse {
            UiContent::ReusePrevious
        } else {
            self.player.frame_index += 1;
            let (primitives, textures_delta) = run_player_ui(
                &self.egui_context,
                screen,
                compositor.painter().max_texture_side(),
                &mut self.player,
            );
            UiContent::Fresh {
                primitives,
                textures_delta,
            }
        };
        let egui_pass = egui_started.elapsed();
        let render_started = Instant::now();
        let outcome = compositor
            .render(
                context,
                UiFrameRequest {
                    screen,
                    content,
                    background: VIDEO_STAND_IN,
                    layers: &mut NoFrameLayers,
                    corner_radius: CornerRadius::from_points(CORNER_RADIUS_POINTS),
                },
            )
            .map_err(|error| format!("frame failed: {error}"))?;
        let render_call = render_started.elapsed();
        if let UiFrameOutcome::Presented(presented) = outcome {
            let now = Instant::now();
            let interval = self.last_present.map(|previous| now - previous);
            self.last_present = Some(now);
            let mode = if wants_reuse {
                UiMode::Reuse
            } else {
                UiMode::FullPass
            };
            self.samples.push((
                mode,
                FrameSample {
                    egui_pass,
                    render_call,
                    egui_buffer_update: presented.timings.egui_buffer_update,
                    egui_render: presented.timings.egui_render,
                    window_corner_mask: presented.timings.window_corner_mask,
                    surface_present: presented.timings.surface_present,
                    interval,
                    draw_calls: presented.work.paint.draw_calls,
                    streamed_bytes: presented
                        .work
                        .prepare
                        .map_or(0, |prepare| prepare.vertex_bytes + prepare.index_bytes),
                },
            ));
        }
        Ok(())
    }

    /// Фаза замера по числу собранных кадров.
    fn advance_bench(&mut self, event_loop: &ActiveEventLoop) {
        let collected = self.samples.len();
        if collected == BENCH_FRAMES_PER_PHASE {
            self.ui_mode = UiMode::Reuse;
            // Интервал между фазами не относится ни к одной.
            self.last_present = None;
        }
        if collected >= 2 * BENCH_FRAMES_PER_PHASE {
            print_bench_report(&self.samples);
            event_loop.exit();
        }
    }

    fn destroy_renderer(&mut self) {
        if let (Some(compositor), Some(context)) = (self.compositor.take(), self.context.as_ref())
            && let Err(error) = compositor.destroy(context)
        {
            eprintln!("compositor destroy failed: {error}");
        }
        self.context = None;
    }
}

impl ApplicationHandler for EguiWindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let transparent = self.options.settings.alpha == WindowAlphaRequest::TransparentPreferred;
        let attributes = Window::default_attributes()
            .with_title("gles2-egui: egui painter")
            .with_inner_size(LogicalSize::new(960.0, 540.0))
            .with_transparent(transparent)
            .with_decorations(!transparent);
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.failure = Some(format!("window creation failed: {error}"));
                event_loop.exit();
                return;
            }
        };
        if let Err(error) = self.create_renderer(&window) {
            self.failure = Some(error);
            event_loop.exit();
            return;
        }
        window.request_redraw();
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(context) = self.context.as_mut()
                    && let Err(error) = context.resize(PixelExtent {
                        width: size.width,
                        height: size.height,
                    })
                {
                    self.failure = Some(format!("resize failed: {error}"));
                    event_loop.exit();
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::Escape) => event_loop.exit(),
                    PhysicalKey::Code(KeyCode::KeyR) => {
                        self.ui_mode = match self.ui_mode {
                            UiMode::FullPass => UiMode::Reuse,
                            UiMode::Reuse => UiMode::FullPass,
                        };
                        println!("UI mode: {:?}", self.ui_mode);
                    }
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.redraw() {
                    self.failure = Some(error);
                    event_loop.exit();
                    return;
                }
                if self.options.mode == RunMode::Bench {
                    self.advance_bench(event_loop);
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _: &ActiveEventLoop) {
        self.destroy_renderer();
    }
}

/// Интерфейс, похожий на плеер: верхняя и нижняя панели поверх «видео», открытая панель
/// настроек слева, текст, ползунки, кнопки.
fn run_player_ui(
    egui_context: &Context,
    screen: UiScreen,
    max_texture_side: usize,
    player: &mut PlayerUiState,
) -> (Vec<egui::epaint::ClippedPrimitive>, egui::TexturesDelta) {
    let size_in_points = vec2(
        screen.size_in_pixels[0] as f32 / screen.pixels_per_point,
        screen.size_in_pixels[1] as f32 / screen.pixels_per_point,
    );
    egui_context.set_pixels_per_point(screen.pixels_per_point);
    let raw_input = RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), size_in_points)),
        max_texture_side: Some(max_texture_side),
        ..Default::default()
    };
    // Позиция «воспроизведения» сдвигается каждый кадр: полный проход действительно новый.
    player.position = (player.position + 0.0005) % 1.0;
    let translucent = egui::Frame::new()
        .fill(Color32::from_black_alpha(170))
        .inner_margin(8.0);
    let output = egui_context.run_ui(raw_input, |ui| {
        egui::Panel::top("top bar")
            .frame(translucent)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Fastiplayer");
                    ui.label("Big Buck Bunny 1080p60.mkv");
                });
            });
        egui::Panel::bottom("controls")
            .frame(translucent)
            .show(ui, |ui| {
                ui.add(egui::Slider::new(&mut player.position, 0.0..=1.0).show_value(false));
                ui.horizontal(|ui| {
                    for label in ["⏮", "▶", "⏭"] {
                        let _clicked = ui.button(label).clicked();
                    }
                    ui.add(egui::Slider::new(&mut player.volume, 0.0..=1.0).text("Volume"));
                    ui.label(format!("frame {}", player.frame_index));
                });
            });
        egui::Panel::left("settings")
            .frame(translucent)
            .show(ui, |ui| {
                ui.heading("Settings");
                ui.checkbox(&mut player.subtitles, "Subtitles");
                for row in 0..8 {
                    ui.label(format!("Option {row}: OpenGL ES 2.0 renderer"));
                }
            });
    });
    let primitives = egui_context.tessellate(output.shapes, output.pixels_per_point);
    (primitives, output.textures_delta)
}

fn percentile(values: &mut [Duration], fraction: f64) -> Duration {
    if values.is_empty() {
        return Duration::ZERO;
    }
    values.sort_unstable();
    let index = ((values.len() - 1) as f64 * fraction).round() as usize;
    values[index]
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Медиана и p95 одной стадии.
fn stage_line(name: &str, samples: &[FrameSample], stage: fn(&FrameSample) -> Option<Duration>) {
    let mut values: Vec<Duration> = samples.iter().filter_map(stage).collect();
    let median = percentile(&mut values, 0.5);
    let p95 = percentile(&mut values, 0.95);
    println!(
        "  {name:<20} median {:>7.3} ms  p95 {:>7.3} ms",
        milliseconds(median),
        milliseconds(p95)
    );
}

fn print_bench_report(samples: &[(UiMode, FrameSample)]) {
    for mode in [UiMode::FullPass, UiMode::Reuse] {
        let phase: Vec<FrameSample> = samples
            .iter()
            .filter(|(sample_mode, _)| *sample_mode == mode)
            .map(|(_, sample)| *sample)
            .collect();
        let draw_calls = phase
            .iter()
            .map(|sample| sample.draw_calls)
            .max()
            .unwrap_or(0);
        let streamed = phase
            .iter()
            .map(|sample| sample.streamed_bytes)
            .max()
            .unwrap_or(0);
        println!(
            "bench {mode:?}: {} frames, egui draw calls {draw_calls}, streamed ≤ {streamed} bytes/frame",
            phase.len()
        );
        stage_line("egui pass (CPU)", &phase, |sample| Some(sample.egui_pass));
        stage_line("render() call", &phase, |sample| Some(sample.render_call));
        stage_line("egui_buffer_update", &phase, |sample| {
            Some(sample.egui_buffer_update)
        });
        stage_line("egui_render", &phase, |sample| Some(sample.egui_render));
        stage_line("window_corner_mask", &phase, |sample| {
            Some(sample.window_corner_mask)
        });
        stage_line("surface_present", &phase, |sample| {
            Some(sample.surface_present)
        });
        stage_line("frame interval", &phase, |sample| sample.interval);
    }
}

fn main() {
    let options = match ExampleOptions::from_args(std::env::args().skip(1)) {
        Ok(options) => options,
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
    let mut app = EguiWindowApp::new(options);
    if let Err(error) = event_loop.run_app(&mut app) {
        eprintln!("event loop failed: {error}");
        std::process::exit(1);
    }
    if let Some(failure) = app.failure {
        eprintln!("{failure}");
        std::process::exit(1);
    }
}
