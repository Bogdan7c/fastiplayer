//! Приём файлов, перетаскиваемых в окно: shell только передаёт события владельцу жеста.
//!
//! Все решения (куда брошено, что открыть, подтверждение замены очереди) принимают
//! `external_open` и `AppState`; здесь нет ни разбора URI, ни логики открытия.

use super::AppShell;
use super::shutdown::AppShellProcessLifecycle;
use crate::external_open::DropGestureEvent;

impl AppShell {
    /// Передаёт событие жеста `AppState` и просит перерисовку (подсветка/результат).
    pub(super) fn handle_external_drop_gesture(&mut self, gesture_event: DropGestureEvent) {
        if self.process_lifecycle != AppShellProcessLifecycle::Running {
            return;
        }
        let (Some(renderer), Some(app_state)) = (self.renderer.as_ref(), self.app_state.as_mut())
        else {
            return;
        };
        let needs_redraw = app_state.handle_external_drop_gesture(
            gesture_event,
            &mut self.playlist_runtime,
            renderer,
        );
        if needs_redraw && let Some(window) = self.window.as_deref() {
            window.request_redraw();
        }
    }

    /// Завершает бросок устаревшего источника: все `DroppedFile` прохода event loop → один жест.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn flush_legacy_file_drop(&mut self) {
        let Some(app_state) = self.app_state.as_mut() else {
            return;
        };
        if let Some(gesture_event) = app_state.take_legacy_file_drop() {
            self.handle_external_drop_gesture(gesture_event);
        }
    }
}
