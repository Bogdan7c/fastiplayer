//! Приёмник внешнего drag & drop: печатает каждое событие нового канала
//! `winit::platform::external_drag` и legacy-события `HoveredFile` / `DroppedFile`.
//!
//! Предназначен для ручной проверки патча:
//!
//! ```text
//! cargo run --manifest-path crates/winit-patch/Cargo.toml --example external_drag
//! ```
//!
//! Wayland (по умолчанию в сессии Plasma) или X11 (`WAYLAND_DISPLAY= cargo run ...`).
//! Затем перетащите в окно файл / три файла / папку из Dolphin или ссылку из браузера.
//! События печатаются в stdout строками вида `external: Dropped ...`.

use std::error::Error;
use std::num::NonZeroU32;
use std::sync::Arc;

use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, OwnedDisplayHandle};
use winit::platform::external_drag::ExternalDragEvent;
use winit::window::{Window, WindowId};

#[path = "util/tracing.rs"]
mod tracing;

/// Цвет фона окна, когда жест не идёт (0xRRGGBB).
const IDLE_COLOR: u32 = 0x0020_2830;
/// Цвет фона, пока над окном висит принятый жест (подсветка).
const HOVER_COLOR: u32 = 0x0030_6050;

/// Окно примера вместе с поверхностью отрисовки.
struct DragWindow {
    /// Поверхность должна быть уничтожена раньше окна (порядок полей значим).
    surface: Surface<OwnedDisplayHandle, Arc<Window>>,
    window: Arc<Window>,
    /// Идёт ли сейчас принятый жест (для подсветки).
    drag_hovering: bool,
}

#[derive(Default)]
struct Application {
    context: Option<Context<OwnedDisplayHandle>>,
    window: Option<DragWindow>,
}

impl Application {
    /// Закрашивает окно цветом по состоянию жеста.
    fn redraw(drag_window: &mut DragWindow) -> Result<(), Box<dyn Error>> {
        let size = drag_window.window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return Ok(());
        };
        drag_window.surface.resize(width, height)?;
        let color = if drag_window.drag_hovering { HOVER_COLOR } else { IDLE_COLOR };
        let mut buffer = drag_window.surface.buffer_mut()?;
        buffer.fill(color);
        buffer.present()?;
        Ok(())
    }

    /// Создаёт окно и поверхность отрисовки.
    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Box<dyn Error>> {
        let attributes = Window::default_attributes()
            .with_title("external_drag: бросьте сюда файл или ссылку")
            .with_inner_size(LogicalSize::new(640.0, 360.0));
        let window = Arc::new(event_loop.create_window(attributes)?);

        // Владеющий handle дисплея не привязан ко времени жизни цикла событий.
        let context = Context::new(event_loop.owned_display_handle())?;
        let surface = Surface::new(&context, Arc::clone(&window))?;
        self.context = Some(context);
        self.window = Some(DragWindow { surface, window, drag_hovering: false });
        Ok(())
    }
}

impl ApplicationHandler for Application {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(error) = self.create_window(event_loop) {
            ::tracing::error!("не удалось создать окно: {error}");
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::HoveredFile(path) => println!("legacy: HoveredFile {path:?}"),
            WindowEvent::HoveredFileCancelled => println!("legacy: HoveredFileCancelled"),
            WindowEvent::DroppedFile(path) => println!("legacy: DroppedFile {path:?}"),
            WindowEvent::RedrawRequested => {
                if let Some(drag_window) = self.window.as_mut() {
                    if let Err(error) = Self::redraw(drag_window) {
                        ::tracing::error!("ошибка отрисовки: {error}");
                    }
                }
            },
            _ => {},
        }
    }

    fn external_drag_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: ExternalDragEvent) {
        println!("external: {event:?}");
        let Some(drag_window) = self.window.as_mut() else {
            return;
        };
        // `ExternalDragEvent` помечен `non_exhaustive`: неизвестные будущие варианты не подсвечивают.
        drag_window.drag_hovering =
            matches!(event, ExternalDragEvent::Entered { .. } | ExternalDragEvent::Moved { .. });
        drag_window.window.request_redraw();
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    tracing::init();
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut Application::default())?;
    Ok(())
}
