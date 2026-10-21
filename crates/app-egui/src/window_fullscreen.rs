//! Вход и выход окна из полноэкранного режима — один путь для всех источников.
//!
//! Кнопка, клавиша F, двойной клик по видео (`ControlAction::ToggleFullscreen`), CLI-флаг
//! `--fullscreen` первого запуска и тот же флаг, пересланный вторым запуском (cpu-parity
//! 06b), входят в фуллскрин через [`enter_fullscreen`]. Поэтому автоскрытие панелей (UX14)
//! и выход по Esc работают одинаково, откуда бы ни пришла команда: контроллер автоскрытия
//! сам видит режим окна на следующем кадре.
//!
//! Окно спрятано за [`FullscreenWindowPort`]: в приложении это winit `Window`, в тестах —
//! фальшивка, поэтому правила (переключение, «уже в фуллскрине — ничего не делать»)
//! проверяются без живого окна.

use tracing::warn;
use winit::window::{Fullscreen, Window};

/// Что окно умеет для полноэкранного режима.
pub(crate) trait FullscreenWindowPort {
    /// Окно сейчас в полноэкранном режиме, выставленном приложением.
    fn is_fullscreen(&self) -> bool;
    /// Просит у системы полноэкранный режим без рамки на мониторе окна.
    fn request_borderless_fullscreen(&self);
    /// Возвращает окно в обычный режим.
    fn leave_fullscreen(&self);
}

impl FullscreenWindowPort for Window {
    fn is_fullscreen(&self) -> bool {
        self.fullscreen().is_some()
    }

    fn request_borderless_fullscreen(&self) {
        // Монитор неизвестен (бывает на Wayland до первого `surface.enter`) — `Borderless(None)`
        // по контракту winit означает «текущий монитор», а не молчаливый отказ.
        let monitor = self.current_monitor();
        if monitor.is_none() {
            warn!(
                "Current window monitor unknown; full screen requested on monitor at system choice"
            );
        }
        self.set_fullscreen(Some(Fullscreen::Borderless(monitor)));
    }

    fn leave_fullscreen(&self) {
        self.set_fullscreen(None);
    }
}

/// Итог [`enter_fullscreen`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FullscreenEntry {
    /// Запрошен полноэкранный режим.
    Requested,
    /// Окно уже было в фуллскрине; ничего не менялось (не выключаем то, что просили включить).
    AlreadyFullscreen,
}

/// Итог [`toggle_fullscreen`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FullscreenToggle {
    /// Окно было обычным: запрошен фуллскрин.
    Entered,
    /// Окно было в фуллскрине: возвращено в обычный режим.
    Left,
}

/// Переводит окно в фуллскрин; уже полноэкранное остаётся как есть (идемпотентно).
pub(crate) fn enter_fullscreen(window: &impl FullscreenWindowPort) -> FullscreenEntry {
    if window.is_fullscreen() {
        return FullscreenEntry::AlreadyFullscreen;
    }
    window.request_borderless_fullscreen();
    FullscreenEntry::Requested
}

/// Переключает режим окна (кнопка, F, двойной клик).
pub(crate) fn toggle_fullscreen(window: &impl FullscreenWindowPort) -> FullscreenToggle {
    match enter_fullscreen(window) {
        FullscreenEntry::Requested => FullscreenToggle::Entered,
        FullscreenEntry::AlreadyFullscreen => {
            window.leave_fullscreen();
            FullscreenToggle::Left
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests;
