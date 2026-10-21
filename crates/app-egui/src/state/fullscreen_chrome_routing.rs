//! Подключение владельца автоскрытия chrome ([`crate::fullscreen_chrome`]) к кадру `AppState`.
//!
//! Здесь только связка: режим окна, ввод кадра и подтверждённые настройки превращаются во
//! вход контроллера. Логика скрытия живёт у владельца, отрисовка сдвига — в `ui::edge_slide`.

use std::time::Instant;

use tracing::debug;
use winit::window::Window;

use super::AppState;
use crate::fullscreen_chrome::{ChromeFrameInput, UserActivity, WindowPresentationMode};

impl AppState {
    /// Продвигает таймер бездействия и анимацию панелей. Вызывается раз за кадр до `render_ui`,
    /// чтобы кадр рисовал панели уже в актуальной позиции.
    pub(crate) fn advance_fullscreen_chrome(
        &mut self,
        window: &Window,
        egui_input: &egui::RawInput,
        now: Instant,
    ) {
        let mode = if window.fullscreen().is_some() {
            WindowPresentationMode::Fullscreen
        } else {
            WindowPresentationMode::Windowed
        };
        self.fullscreen_chrome.advance(ChromeFrameInput {
            mode,
            activity: UserActivity::from_raw_input(egui_input),
            delay: self.committed_config_snapshot.fullscreen_autohide_delay(),
            // Та же длительность, что у сайдбара: при reduced motion она равна нулю.
            slide_duration_seconds: self
                .committed_config_snapshot
                .sidebar_slide_duration_seconds(),
            now,
        });
    }

    /// Когда event loop должен проснуться, чтобы спрятать панели без ввода.
    #[must_use]
    pub(crate) fn fullscreen_chrome_wake_deadline(&self) -> Option<Instant> {
        self.fullscreen_chrome.next_wake_deadline()
    }

    /// Переключает fullscreen состояние окна (кнопка, F, двойной клик). Вход — тот же
    /// [`crate::window_fullscreen::enter_fullscreen`], что у `--fullscreen` (сессия 06b).
    pub(super) fn toggle_fullscreen(window: &Window) {
        let toggle = crate::window_fullscreen::toggle_fullscreen(window);
        debug!(?toggle, "Fullscreen toggled by the user");
    }
}
