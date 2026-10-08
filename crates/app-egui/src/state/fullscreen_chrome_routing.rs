//! Подключение владельца автоскрытия chrome ([`crate::fullscreen_chrome`]) к кадру `AppState`.
//!
//! Здесь только связка: режим окна, ввод кадра и подтверждённые настройки превращаются во
//! вход контроллера. Логика скрытия живёт у владельца, отрисовка сдвига — в `ui::edge_slide`.

use std::time::Instant;

use tracing::warn;
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

    /// Переключает fullscreen состояние окна.
    pub(super) fn toggle_fullscreen(window: &Window) {
        let is_fullscreen = window.fullscreen().is_some();
        if is_fullscreen {
            window.set_fullscreen(None);
            return;
        }

        // Монитор неизвестен (бывает на Wayland до первого `surface.enter`) — `Borderless(None)`
        // по контракту winit означает «текущий монитор», а не молчаливый отказ.
        let monitor = window.current_monitor();
        if monitor.is_none() {
            warn!(
                "Текущий монитор окна неизвестен; фуллскрин запрошен на мониторе по выбору системы"
            );
        }
        window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(monitor)));
    }
}
