//! Фальшивое окно: помнит режим и записывает, какие запросы получило.

use std::cell::{Cell, RefCell};

use super::FullscreenWindowPort;

/// Запрос, который фальшивое окно получило от приложения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FakeWindowRequest {
    BorderlessFullscreen,
    LeaveFullscreen,
}

/// Окно без winit: режим меняется сразу, как у композитора, который сразу согласился.
#[derive(Debug, Default)]
pub(crate) struct FakeFullscreenWindow {
    fullscreen: Cell<bool>,
    requests: RefCell<Vec<FakeWindowRequest>>,
}

impl FakeFullscreenWindow {
    /// Окно, уже находящееся в фуллскрине.
    pub(crate) fn already_fullscreen() -> Self {
        let window = Self::default();
        window.fullscreen.set(true);
        window
    }

    pub(crate) fn requests(&self) -> Vec<FakeWindowRequest> {
        self.requests.borrow().clone()
    }
}

impl FullscreenWindowPort for FakeFullscreenWindow {
    fn is_fullscreen(&self) -> bool {
        self.fullscreen.get()
    }

    fn request_borderless_fullscreen(&self) {
        self.requests
            .borrow_mut()
            .push(FakeWindowRequest::BorderlessFullscreen);
        self.fullscreen.set(true);
    }

    fn leave_fullscreen(&self) {
        self.requests
            .borrow_mut()
            .push(FakeWindowRequest::LeaveFullscreen);
        self.fullscreen.set(false);
    }
}
