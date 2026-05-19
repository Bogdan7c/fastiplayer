use winit::event::WindowEvent;

/// Решение render pass-а о том, нужен ли следующий redraw без внешнего window event-а.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RedrawPacing {
    /// Playback/seek/opening state требует непрерывного render loop-а.
    continuous: bool,

    /// Worker command или egui попросили ещё один redraw для доставки нового состояния.
    follow_up: bool,
}

impl RedrawPacing {
    /// Создаёт итоговое pacing-решение после render pass-а.
    pub(crate) const fn new(continuous: bool, follow_up: bool) -> Self {
        Self {
            continuous,
            follow_up,
        }
    }

    /// Возвращает `true`, если shell должен держать `ControlFlow::Poll`.
    pub(crate) const fn wants_continuous_redraw(self) -> bool {
        self.continuous
    }

    /// Возвращает `true`, если следующий redraw надо запросить сразу.
    pub(crate) const fn wants_immediate_redraw(self) -> bool {
        self.continuous || self.follow_up
    }
}

/// Возвращает `true`, если window/input событие должно дать один redraw в Wait mode.
pub(crate) fn should_request_redraw_after_window_event(event: &WindowEvent) -> bool {
    !matches!(
        event,
        WindowEvent::RedrawRequested | WindowEvent::CloseRequested
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что continuous playback всегда требует следующий redraw.
    #[test]
    fn continuous_pacing_requests_immediate_redraw() {
        let pacing = RedrawPacing::new(true, false);

        assert!(pacing.wants_continuous_redraw());
        assert!(pacing.wants_immediate_redraw());
    }

    /// Проверяет, что follow-up от worker/egui тоже будит render loop.
    #[test]
    fn follow_up_pacing_requests_immediate_redraw() {
        let pacing = RedrawPacing::new(false, true);

        assert!(!pacing.wants_continuous_redraw());
        assert!(pacing.wants_immediate_redraw());
    }

    /// Проверяет, что idle frame без причин остаётся в Wait mode.
    #[test]
    fn idle_pacing_does_not_request_immediate_redraw() {
        let pacing = RedrawPacing::new(false, false);

        assert!(!pacing.wants_continuous_redraw());
        assert!(!pacing.wants_immediate_redraw());
    }

    /// Проверяет, что сам redraw event не создаёт бесконечный redraw в Wait mode.
    #[test]
    fn redraw_requested_event_does_not_request_extra_redraw() {
        assert!(!should_request_redraw_after_window_event(
            &WindowEvent::RedrawRequested
        ));
    }

    /// Проверяет, что закрытие окна не планирует лишний frame перед exit.
    #[test]
    fn close_requested_event_does_not_request_extra_redraw() {
        assert!(!should_request_redraw_after_window_event(
            &WindowEvent::CloseRequested
        ));
    }
}
