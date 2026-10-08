//! Склейка shell ↔ приём запросов второго запуска.
//!
//! Решения принимают владельцы: что принять — `instance_forwarding`, как открыть —
//! диспетчер внешнего открытия в `AppState`, как поднять окно —
//! `instance_forwarding::window_activation`. Shell только выбирает момент: запрос
//! исполняется, когда есть окно, renderer и `AppState`.

use super::AppShell;
use super::shutdown::AppShellProcessLifecycle;
use crate::instance_forwarding::external_open_request_for;
use crate::instance_forwarding::window_activation::raise_window;

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
    pub(super) fn execute_pending_forwarded_requests(&mut self) -> bool {
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
        let mut executed_any = false;
        while let Some(request) = self.instance_forwarding.take_next_pending_request() {
            let (action, activation_token) = request.into_parts();
            let raise = raise_window(window, activation_token);
            tracing::info!(?raise, "Окно поднято по запросу второго запуска");
            if let Some(open_request) = external_open_request_for(action) {
                app_state.handle_forwarded_open_request(
                    open_request,
                    &mut self.playlist_runtime,
                    renderer,
                );
            }
            executed_any = true;
        }
        executed_any
    }
}
