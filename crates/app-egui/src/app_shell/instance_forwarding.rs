//! Склейка shell ↔ приём запросов второго запуска.
//!
//! Решения принимают владельцы: что принять — `instance_forwarding`, как открыть —
//! диспетчер внешнего открытия в `AppState`, как поднять окно —
//! `instance_forwarding::window_activation`. Shell только выбирает момент: запрос
//! исполняется, когда есть окно, renderer и `AppState`. Там же один раз применяется
//! `--fullscreen` первого запуска (`startup_window`): это тоже запрос к окну, ждущий окна.

mod startup_window;

use desktop_integration::WindowActivationToken;
use render_wgpu_shell::Renderer;
use winit::window::Window;

use super::AppShell;
use super::shutdown::AppShellProcessLifecycle;
use crate::external_open::request::ExternalOpenRequest;
use crate::instance_forwarding::execution::{ForwardedRequestTarget, execute_forwarded_request};
use crate::instance_forwarding::window_activation::raise_window;
use crate::playlist_runtime::PlaylistRuntime;
use crate::state::AppState;
use crate::window_fullscreen::enter_fullscreen;

impl AppShell {
    /// Пробуждение от службы приёма: подтвердить доставки и исполнить, что можно.
    ///
    /// Возвращает `true`, если окну нужна перерисовка.
    pub(super) fn drain_instance_forwarding(&mut self) -> bool {
        let accepted = self.instance_forwarding.drain_deliveries();
        accepted > 0 && self.execute_pending_forwarded_requests()
    }

    /// Исполняет подтверждённые запросы по порядку, если окно и состояние готовы.
    /// Иначе запросы ждут (их повторно исполнит `restore_runtime`).
    fn execute_pending_forwarded_requests(&mut self) -> bool {
        if self.process_lifecycle != AppShellProcessLifecycle::Running {
            return false;
        }
        let (Some(window), Some(renderer), Some(app_state)) = (
            self.window.as_deref(),
            self.renderer.as_ref(),
            self.app_state.as_mut(),
        ) else {
            return false;
        };
        let mut target = ShellForwardedRequestTarget {
            window,
            app_state,
            playlist_runtime: &mut self.playlist_runtime,
            renderer,
        };
        let mut executed_any = false;
        while let Some(request) = self.instance_forwarding.take_next_pending_request() {
            execute_forwarded_request(request, &mut target);
            executed_any = true;
        }
        executed_any
    }
}

/// Настоящий исполнитель: окно winit, диспетчер внешнего открытия `AppState`, общий
/// вход в фуллскрин. Порядок шагов решает `instance_forwarding::execution`.
struct ShellForwardedRequestTarget<'shell> {
    window: &'shell Window,
    app_state: &'shell mut AppState,
    playlist_runtime: &'shell mut PlaylistRuntime,
    renderer: &'shell Renderer,
}

impl ForwardedRequestTarget for ShellForwardedRequestTarget<'_> {
    fn raise_window(&mut self, activation_token: Option<WindowActivationToken>) {
        let raise = raise_window(self.window, activation_token);
        tracing::info!(?raise, "Window raised at second instance request");
    }

    fn open_forwarded_media(&mut self, request: ExternalOpenRequest) {
        self.app_state
            .handle_forwarded_open_request(request, self.playlist_runtime, self.renderer);
    }

    fn enter_fullscreen(&mut self) {
        let entry = enter_fullscreen(self.window);
        tracing::info!(?entry, "Fullscreen requested by the second instance");
    }
}
