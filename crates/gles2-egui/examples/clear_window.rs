//! Ручная проверка фундамента GLES2 на живом железе (780M, T410): окно winit, заливка цветом,
//! вывод возможностей драйвера.
//!
//! Запуск:
//! ```text
//! cargo run -p gles2-egui --example clear_window -- [--transparent] [--immediate]
//! ```
//! - `--transparent` — прозрачное окно (конфиг с альфой): заливка полупрозрачная, сквозь окно
//!   виден рабочий стол. Если альфы нет, в выводе будет `Opaque { reason: NoAlphaConfig }`.
//! - `--immediate` — `eglSwapInterval(0)` вместо ожидания развёртки.
//! - `--scripted-check` — без клавиатуры (для запуска по ssh): 120 кадров, отпуск и возврат окна,
//!   смена vsync на месте, resize, fence; печатает исходы и `scripted check: PASS`, затем выходит.
//!
//! Режим окна выбирает winit: нативный Wayland, если есть `WAYLAND_DISPLAY`; XWayland — если
//! запустить с пустым `WAYLAND_DISPLAY` (`env -u WAYLAND_DISPLAY …`).
//!
//! Клавиши (по физическому положению, раскладка не важна):
//! - `C` — следующий цвет заливки;
//! - `D` — отпустить окно / вернуть его (контракт «одно окно — одна surface»);
//! - `V` — переключить vsync на месте;
//! - `H` — опросить сброс GPU;
//! - `Esc` — выход.
//!
//! Потеря контекста (зависание GPU) не роняет пример: контекст пересоздаётся.

use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gles2_egui::{
    DetachOutcome, DmaBufImportSupport, DrmFourcc, FenceStatus, FormatSampling, FrameStart,
    Gles2Capabilities, Gles2Context, Gles2Error, PixelExtent, PresentationReconfigured,
    PresentationSettings, ReattachOutcome, VsyncMode, WindowAlphaMode, WindowAlphaRequest,
    WindowSource,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

/// Как работает пример.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// Окно ждёт клавиш.
    Interactive,
    /// Сценарий проверки без клавиатуры, затем выход.
    ScriptedCheck,
}

/// Параметры командной строки.
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
                "--scripted-check" => mode = RunMode::ScriptedCheck,
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        Ok(Self { settings, mode })
    }
}

/// Кадров в сценарии: ~2 с при 60 Гц — видно глазами и хватает для среднего интервала.
const SCRIPTED_FRAME_COUNT: u32 = 120;
/// Сколько ждать fence в сценарии.
const SCRIPTED_FENCE_TIMEOUT: Duration = Duration::from_secs(2);

/// Цвета заливки (straight RGBA). У прозрачного окна альфа 0,6 — окно полупрозрачное.
const CLEAR_COLORS: [[f32; 3]; 4] = [
    [0.10, 0.45, 0.85],
    [0.85, 0.25, 0.20],
    [0.15, 0.70, 0.35],
    [0.95, 0.80, 0.10],
];
const TRANSPARENT_ALPHA: f32 = 0.6;

struct ClearWindowApp {
    options: ExampleOptions,
    window: Option<Arc<Window>>,
    context: Option<Gles2Context>,
    color_index: usize,
    failure: Option<String>,
}

impl ClearWindowApp {
    fn new(options: ExampleOptions) -> Self {
        Self {
            options,
            window: None,
            context: None,
            color_index: 0,
            failure: None,
        }
    }

    fn create_context(&mut self, window: &Arc<Window>) -> Result<(), String> {
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
        print_capabilities(context.capabilities());
        println!("presentation: {:?}", context.presentation());
        self.context = Some(context);
        Ok(())
    }

    /// Premultiplied-цвет заливки: у прозрачного окна RGB умножены на альфу (контракт egui и
    /// композитора).
    fn clear_color(&self, alpha_mode: WindowAlphaMode) -> [f32; 4] {
        let [red, green, blue] = CLEAR_COLORS[self.color_index % CLEAR_COLORS.len()];
        match alpha_mode {
            WindowAlphaMode::PremultipliedTransparent => [
                red * TRANSPARENT_ALPHA,
                green * TRANSPARENT_ALPHA,
                blue * TRANSPARENT_ALPHA,
                TRANSPARENT_ALPHA,
            ],
            WindowAlphaMode::Opaque { .. } => [red, green, blue, 1.0],
        }
    }

    fn redraw(&mut self) {
        let Some(alpha_mode) = self
            .context
            .as_ref()
            .map(|context| context.presentation().alpha)
        else {
            return;
        };
        let color = self.clear_color(alpha_mode);
        let Some(context) = self.context.as_mut() else {
            return;
        };
        let outcome = match context.begin_frame() {
            Ok(FrameStart::Ready(frame)) => {
                frame.clear(color);
                frame.present().map(Some)
            }
            Ok(FrameStart::Skipped(reason)) => {
                println!("frame skipped: {reason:?}");
                Ok(None)
            }
            Err(error) => Err(error),
        };
        match outcome {
            Ok(Some(presented)) => println!("frame presented: {:?}", presented.timings),
            Ok(None) => {}
            Err(Gles2Error::ContextLost(loss)) => self.recover_from_context_loss(&loss.to_string()),
            Err(error) => eprintln!("frame failed: {error}"),
        }
    }

    /// Потеря контекста: старый контекст разрушается, создаётся новый на том же окне.
    fn recover_from_context_loss(&mut self, reason: &str) {
        eprintln!("GLES2 context lost ({reason}); recreating");
        self.context = None;
        let Some(window) = self.window.clone() else {
            return;
        };
        if let Err(error) = self.create_context(&window) {
            eprintln!("{error}");
        }
        window.request_redraw();
    }

    fn handle_key(&mut self, key: KeyCode, event_loop: &ActiveEventLoop) {
        let Some(context) = self.context.as_mut() else {
            return;
        };
        match key {
            KeyCode::Escape => event_loop.exit(),
            KeyCode::KeyC => self.color_index += 1,
            KeyCode::KeyD => {
                if context.presentation().detached {
                    println!("reattach: {:?}", context.reattach_presentation());
                } else {
                    let outcome = context.detach_presentation();
                    if matches!(outcome, Ok(DetachOutcome::Detached)) {
                        println!("detached: the window keeps its last frame, D brings it back");
                    } else {
                        println!("detach: {outcome:?}");
                    }
                }
            }
            KeyCode::KeyV => {
                let mut settings = context.presentation().settings;
                settings.vsync = match settings.vsync {
                    VsyncMode::WaitForVblank => VsyncMode::Immediate,
                    VsyncMode::Immediate => VsyncMode::WaitForVblank,
                };
                println!(
                    "reconfigure {:?}: {:?}",
                    settings.vsync,
                    context.reconfigure_presentation(settings)
                );
            }
            KeyCode::KeyH => match context.check_context_health() {
                Ok(()) => println!("context healthy"),
                Err(loss) => self.recover_from_context_loss(&loss.to_string()),
            },
            _ => return,
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

impl ApplicationHandler for ClearWindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let transparent = self.options.settings.alpha == WindowAlphaRequest::TransparentPreferred;
        let attributes = Window::default_attributes()
            .with_title("gles2-egui clear_window")
            .with_inner_size(LogicalSize::new(800.0, 450.0))
            .with_transparent(transparent);
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.failure = Some(format!("window creation failed: {error}"));
                event_loop.exit();
                return;
            }
        };
        if let Err(error) = self.create_context(&window) {
            self.failure = Some(error);
            event_loop.exit();
            return;
        }
        self.window = Some(window);
        if self.options.mode == RunMode::ScriptedCheck {
            let result = match self.context.as_mut() {
                Some(context) => run_scripted_check(context),
                None => Err("context is missing".to_owned()),
            };
            match result {
                Ok(()) => println!("scripted check: PASS"),
                Err(failure) => self.failure = Some(format!("scripted check: FAIL: {failure}")),
            }
            event_loop.exit();
            return;
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(context) = self.context.as_mut() {
                    let resized = context.resize(PixelExtent {
                        width: size.width,
                        height: size.height,
                    });
                    if let Err(error) = resized {
                        eprintln!("resize failed: {error}");
                    }
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let PhysicalKey::Code(key) = event.physical_key {
                    self.handle_key(key, event_loop);
                }
            }
            _ => {}
        }
    }
}

/// Рисует один кадр заданным цветом; `Ok(false)` — кадр пропущен.
fn draw_scripted_frame(context: &mut Gles2Context, rgba: [f32; 4]) -> Result<bool, String> {
    match context.begin_frame() {
        Ok(FrameStart::Ready(frame)) => {
            frame.clear(rgba);
            frame
                .present()
                .map(|_| true)
                .map_err(|error| format!("present failed: {error}"))
        }
        Ok(FrameStart::Skipped(_)) => Ok(false),
        Err(error) => Err(format!("begin_frame failed: {error}")),
    }
}

/// Сценарий проверки без клавиатуры: кадры, отпуск/возврат окна, vsync на месте, resize, fence.
fn run_scripted_check(context: &mut Gles2Context) -> Result<(), String> {
    let opaque_color = |index: u32| {
        let [red, green, blue] = CLEAR_COLORS[index as usize % CLEAR_COLORS.len()];
        [red, green, blue, 1.0]
    };

    let started_at = Instant::now();
    for index in 0..SCRIPTED_FRAME_COUNT {
        if !draw_scripted_frame(context, opaque_color(index / 30))? {
            return Err(format!("frame {index} was skipped"));
        }
    }
    let average = started_at.elapsed() / SCRIPTED_FRAME_COUNT;
    println!("{SCRIPTED_FRAME_COUNT} frames, average interval {average:?}");

    let detached = context.detach_presentation();
    println!("detach: {detached:?}");
    if detached != Ok(DetachOutcome::Detached) {
        return Err(format!("detach: {detached:?}"));
    }
    if draw_scripted_frame(context, opaque_color(0))? {
        return Err("a frame was presented while detached".to_owned());
    }
    let detached_again = context.detach_presentation();
    if detached_again != Ok(DetachOutcome::AlreadyDetached) {
        return Err(format!("second detach: {detached_again:?}"));
    }
    let reattached = context.reattach_presentation();
    println!("reattach: {reattached:?}");
    if reattached != Ok(ReattachOutcome::Reattached) {
        return Err(format!("reattach: {reattached:?}"));
    }
    if !draw_scripted_frame(context, opaque_color(1))? {
        return Err("no frame after reattach".to_owned());
    }

    let current = context.presentation().settings;
    let same = context.reconfigure_presentation(current);
    if same != Ok(PresentationReconfigured::AlreadyActive) {
        return Err(format!("reconfigure with the same settings: {same:?}"));
    }
    let toggled = PresentationSettings {
        vsync: match current.vsync {
            VsyncMode::WaitForVblank => VsyncMode::Immediate,
            VsyncMode::Immediate => VsyncMode::WaitForVblank,
        },
        ..current
    };
    let applied = context.reconfigure_presentation(toggled);
    println!("reconfigure {:?}: {applied:?}", toggled.vsync);
    if !matches!(applied, Ok(PresentationReconfigured::Applied(_))) {
        return Err(format!("reconfigure: {applied:?}"));
    }
    if context.reconfigure_presentation(current).is_err() {
        return Err("restoring vsync failed".to_owned());
    }

    let extent = context.extent();
    let half = PixelExtent {
        width: (extent.width / 2).max(1),
        height: (extent.height / 2).max(1),
    };
    context
        .resize(half)
        .map_err(|error| format!("resize: {error}"))?;
    if !draw_scripted_frame(context, opaque_color(2))? {
        return Err("no frame after resize".to_owned());
    }
    context
        .resize(extent)
        .map_err(|error| format!("resize back: {error}"))?;

    if !draw_scripted_frame(context, opaque_color(3))? {
        return Err("no frame before fence".to_owned());
    }
    let fence = context
        .insert_fence()
        .map_err(|error| format!("fence: {error}"))?;
    let status = fence.wait(SCRIPTED_FENCE_TIMEOUT);
    println!(
        "fence ({:?}): {status:?}, stats {:?}",
        context.fence_strategy(),
        context.fence_stats()
    );
    if status != Ok(FenceStatus::Signaled) {
        return Err(format!("fence did not signal: {status:?}"));
    }
    context
        .check_context_health()
        .map_err(|loss| format!("context lost: {loss}"))
}

fn print_capabilities(capabilities: &Gles2Capabilities) {
    println!("== Gles2Capabilities ==");
    println!("GL_RENDERER: {}", capabilities.gl_renderer());
    println!("GL_VENDOR:   {}", capabilities.gl_vendor());
    println!(
        "GL_VERSION:  {} (treated as ES 2.0; reported {}.{})",
        capabilities.gl_version_string(),
        capabilities.reported_gles_version().major,
        capabilities.reported_gles_version().minor
    );
    println!("GLSL:        {}", capabilities.shading_language_version());
    println!(
        "EGL:         {} ({})",
        capabilities.egl_version(),
        capabilities.egl_vendor()
    );
    println!("MAX_TEXTURE_SIZE: {}", capabilities.max_texture_size());
    println!(
        "highp float precision bits (fragment): {}",
        capabilities.fragment_high_float_precision_bits()
    );
    let tiers = capabilities.tiers();
    println!("tier baseline:        {:?}", tiers.baseline);
    println!("tier precision:       {:?}", tiers.precision);
    println!("tier zero-copy:       {:?}", tiers.zero_copy);
    println!("tier zero-copy 10bit: {:?}", tiers.zero_copy_10bit);
    println!("conveniences:         {:?}", tiers.conveniences);
    print_dma_buf_summary(capabilities.dma_buf());
    println!(
        "GL extensions ({}): {}",
        capabilities.gl_extensions().len(),
        capabilities
            .gl_extensions()
            .iter()
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "EGL display extensions ({}): {}",
        capabilities.egl_display_extensions().len(),
        capabilities
            .egl_display_extensions()
            .iter()
            .collect::<Vec<_>>()
            .join(" ")
    );
}

/// Сводка импорта DMA-BUF: форматы для `sampler2D` и только-external, плюс модификаторы
/// плоскостей, которые нужны zero-copy (R8, GR88, ABGR8888).
fn print_dma_buf_summary(support: &DmaBufImportSupport) {
    let DmaBufImportSupport::Queried { formats } = support else {
        println!("DMA-BUF import:       {support:?}");
        return;
    };
    let names_with = |wanted: FormatSampling| {
        formats
            .iter()
            .filter(|format| format.sampling() == wanted)
            .map(|format| format!("{:?}", format.fourcc))
            .collect::<Vec<_>>()
            .join(" ")
    };
    println!(
        "DMA-BUF sampler2D:     {}",
        names_with(FormatSampling::Sampler2D)
    );
    println!(
        "DMA-BUF external-only: {}",
        names_with(FormatSampling::ExternalOnly)
    );
    for plane_format in [DrmFourcc::R8, DrmFourcc::GR88, DrmFourcc::ABGR8888] {
        let modifiers = formats
            .iter()
            .find(|format| format.fourcc == plane_format)
            .map(|format| {
                format
                    .modifiers
                    .iter()
                    .map(|modifier| format!("{:#x}", modifier.modifier))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_else(|| "not offered".to_owned());
        println!("  {plane_format:?} modifiers: {modifiers}");
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = ExampleOptions::from_args(std::env::args().skip(1))?;
    let event_loop = EventLoop::new()?;
    let mut app = ClearWindowApp::new(options);
    event_loop.run_app(&mut app)?;
    match app.failure {
        Some(failure) => Err(failure.into()),
        None => Ok(()),
    }
}
