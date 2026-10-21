//! `--fullscreen` первого запуска (cpu-parity 06b): окно входит в фуллскрин, когда готово.
//!
//! Флаг применяется в той же точке, где исполняются ждущие запросы второго запуска (конец
//! `restore_runtime`: есть окно, renderer и `AppState`), и тем же входом, что кнопка
//! ([`crate::window_fullscreen::enter_fullscreen`]). Ровно один раз: `restore_runtime`
//! повторяется после suspend/resume, а пользователь к этому времени мог выйти из
//! фуллскрина сам — флаг запуска не должен возвращать его обратно.

use desktop_integration::FullscreenIntent;

use crate::app_shell::AppShell;
use crate::window_fullscreen::enter_fullscreen;

impl AppShell {
    /// Окно готово: сначала флаг первого запуска (один раз), затем ждущие запросы второго
    /// запуска — в порядке прихода.
    pub(in crate::app_shell) fn execute_pending_window_requests(&mut self) {
        self.apply_startup_fullscreen_once();
        self.execute_pending_forwarded_requests();
    }

    fn apply_startup_fullscreen_once(&mut self) {
        let Some(window) = self.window.as_deref() else {
            // Окна ещё нет: намерение остаётся ждать следующего `restore_runtime`.
            return;
        };
        match take_startup_fullscreen_once(&mut self.startup_fullscreen) {
            FullscreenIntent::Enter => {
                let entry = enter_fullscreen(window);
                tracing::info!(?entry, "Fullscreen requested by the command line");
            }
            FullscreenIntent::LeaveAsIs => {}
        }
    }
}

/// Забирает намерение запуска: на его месте остаётся `LeaveAsIs`, поэтому следующий
/// `restore_runtime` (после resume) ничего не просит.
fn take_startup_fullscreen_once(pending: &mut FullscreenIntent) -> FullscreenIntent {
    std::mem::take(pending)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Флаг запуска срабатывает один раз: повторный `restore_runtime` после resume
    /// не возвращает фуллскрин, из которого пользователь мог выйти.
    #[test]
    fn startup_fullscreen_is_taken_exactly_once() {
        let mut pending = FullscreenIntent::Enter;

        assert_eq!(
            take_startup_fullscreen_once(&mut pending),
            FullscreenIntent::Enter
        );
        assert_eq!(
            take_startup_fullscreen_once(&mut pending),
            FullscreenIntent::LeaveAsIs
        );
    }

    /// Запуск без флага ничего не просит.
    #[test]
    fn launch_without_flag_requests_nothing() {
        let mut pending = FullscreenIntent::LeaveAsIs;

        assert_eq!(
            take_startup_fullscreen_once(&mut pending),
            FullscreenIntent::LeaveAsIs
        );
    }
}
