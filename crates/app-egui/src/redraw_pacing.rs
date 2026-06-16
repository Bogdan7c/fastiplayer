use std::time::{Duration, Instant};

use winit::{event::WindowEvent, event_loop::ControlFlow};

use crate::startup_media::STARTUP_MEDIA_POLL_INTERVAL;

/// Единый интервал polling-а shell background jobs, когда playback не даёт continuous redraw.
const BACKGROUND_JOB_POLL_INTERVAL: Duration = STARTUP_MEDIA_POLL_INTERVAL;

/// Решение render pass-а о том, нужен ли следующий redraw без внешнего window event-а.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RedrawPacing {
    /// Playback/seek/opening state требует непрерывного render loop-а.
    continuous: bool,

    /// Worker command или egui попросили ещё один redraw для доставки нового состояния.
    follow_up: bool,
}

/// Действие, которое shell должен применить к winit event loop после решения pacing-а.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RedrawControlAction {
    /// Следующий режим ожидания winit event loop.
    pub(crate) control_flow: ControlFlow,

    /// Нужно ли запросить redraw у окна вместе с установкой `control_flow`.
    pub(crate) request_redraw: bool,
}

impl RedrawControlAction {
    /// Собирает action без раскрытия деталей хранения scheduler-а наружу.
    const fn new(control_flow: ControlFlow, request_redraw: bool) -> Self {
        Self {
            control_flow,
            request_redraw,
        }
    }
}

impl RedrawPacing {
    /// Создаёт итоговое pacing-решение после render pass-а.
    pub(crate) const fn new(continuous: bool, follow_up: bool) -> Self {
        Self {
            continuous,
            follow_up,
        }
    }

    /// Возвращает `true`, если shell должен перерисовываться непрерывно (playback).
    pub(crate) const fn wants_continuous_redraw(self) -> bool {
        self.continuous
    }

    /// Возвращает `true`, если следующий redraw надо запросить сразу.
    pub(crate) const fn wants_immediate_redraw(self) -> bool {
        self.continuous || self.follow_up
    }
}

/// Scheduler timed wakeup-ов для shell background jobs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BackgroundPollScheduler {
    /// Ближайший deadline, когда idle loop должен снова проверить background jobs.
    next_background_job_poll_at: Option<Instant>,
}

impl BackgroundPollScheduler {
    /// Создаёт scheduler без запланированного background wakeup-а.
    pub(crate) const fn new() -> Self {
        Self {
            next_background_job_poll_at: None,
        }
    }

    /// Возвращает winit action после render pass-а и обновляет deadline background polling-а.
    pub(crate) fn after_render(
        &mut self,
        pacing: RedrawPacing,
        has_pending_background_job: bool,
        now: Instant,
    ) -> RedrawControlAction {
        self.clear_deadline_if_background_jobs_finished(has_pending_background_job);

        if pacing.wants_continuous_redraw() {
            // Continuous playback перерисовывается каждый frame callback (vsync),
            // но event loop СПИТ между кадрами. ControlFlow::Poll крутил loop
            // вхолостую между 60Hz redraw-ами (redraw гейтится frame callback-ом,
            // present не блокирует), сжигая целое ядро. Wait + request_redraw даёт
            // тот же 60fps без busy-spin.
            return RedrawControlAction::new(ControlFlow::Wait, true);
        }

        if pacing.wants_immediate_redraw() {
            return RedrawControlAction::new(ControlFlow::Wait, true);
        }

        self.schedule_idle_background_poll_after_render(has_pending_background_job, now)
    }

    /// Возвращает winit action прямо перед idle wait.
    pub(crate) fn before_idle_wait(
        &mut self,
        has_continuous_redraw: bool,
        has_pending_background_job: bool,
        now: Instant,
    ) -> RedrawControlAction {
        self.clear_deadline_if_background_jobs_finished(has_pending_background_job);

        if has_continuous_redraw {
            // См. after_render: continuous playback держим на Wait + request_redraw,
            // а не на Poll, чтобы не крутить event loop между frame callback-ами.
            return RedrawControlAction::new(ControlFlow::Wait, true);
        }

        self.schedule_background_poll_before_idle_wait(has_pending_background_job, now)
    }

    /// Сбрасывает stale deadline, когда shell больше не ждёт фоновых задач.
    fn clear_deadline_if_background_jobs_finished(&mut self, has_pending_background_job: bool) {
        if !has_pending_background_job {
            self.next_background_job_poll_at = None;
        }
    }

    /// Планирует первый timed wakeup после idle render-а без немедленного redraw.
    fn schedule_idle_background_poll_after_render(
        &mut self,
        has_pending_background_job: bool,
        now: Instant,
    ) -> RedrawControlAction {
        if !has_pending_background_job {
            return RedrawControlAction::new(ControlFlow::Wait, false);
        }

        let deadline = self
            .next_background_job_poll_at
            .unwrap_or(now + BACKGROUND_JOB_POLL_INTERVAL);
        self.next_background_job_poll_at = Some(deadline);
        RedrawControlAction::new(ControlFlow::WaitUntil(deadline), false)
    }

    /// Будит shell для due background poll-а или оставляет текущий timed wakeup.
    fn schedule_background_poll_before_idle_wait(
        &mut self,
        has_pending_background_job: bool,
        now: Instant,
    ) -> RedrawControlAction {
        if !has_pending_background_job {
            return RedrawControlAction::new(ControlFlow::Wait, false);
        }

        let current_deadline = self.next_background_job_poll_at.unwrap_or(now);
        if now >= current_deadline {
            let next_deadline = now + BACKGROUND_JOB_POLL_INTERVAL;
            self.next_background_job_poll_at = Some(next_deadline);
            return RedrawControlAction::new(ControlFlow::WaitUntil(next_deadline), true);
        }

        self.next_background_job_poll_at = Some(current_deadline);
        RedrawControlAction::new(ControlFlow::WaitUntil(current_deadline), false)
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

    /// Достаёт deadline из `ControlFlow::WaitUntil` для точных scheduler assertions.
    fn wait_until_deadline(action: RedrawControlAction) -> Instant {
        match action.control_flow {
            ControlFlow::WaitUntil(deadline) => deadline,
            other_control_flow => {
                panic!("ожидали WaitUntil(deadline), получили {other_control_flow:?}");
            }
        }
    }

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

    /// Continuous playback держит Wait + request_redraw (без busy-spin), а не Poll.
    #[test]
    fn after_render_continuous_requests_wait_and_redraw() {
        let mut scheduler = BackgroundPollScheduler::new();
        let now = Instant::now();

        let action = scheduler.after_render(RedrawPacing::new(true, false), false, now);

        assert!(matches!(action.control_flow, ControlFlow::Wait));
        assert!(action.request_redraw);
    }

    /// Проверяет, что follow-up redraw остаётся в Wait mode и не включает busy polling.
    #[test]
    fn after_render_follow_up_requests_wait_and_redraw() {
        let mut scheduler = BackgroundPollScheduler::new();
        let now = Instant::now();

        let action = scheduler.after_render(RedrawPacing::new(false, true), false, now);

        assert!(matches!(action.control_flow, ControlFlow::Wait));
        assert!(action.request_redraw);
    }

    /// Проверяет, что отсутствие background jobs очищает deadline и не будит окно.
    #[test]
    fn no_pending_background_clears_to_wait_without_redraw() {
        let mut scheduler = BackgroundPollScheduler::new();
        let now = Instant::now();

        scheduler.after_render(RedrawPacing::new(false, false), true, now);
        let action = scheduler.after_render(RedrawPacing::new(false, false), false, now);

        assert!(matches!(action.control_flow, ControlFlow::Wait));
        assert!(!action.request_redraw);
        assert!(scheduler.next_background_job_poll_at.is_none());
    }

    /// Проверяет, что idle render с pending job планирует timed wakeup.
    #[test]
    fn pending_background_schedules_wait_until_deadline() {
        let mut scheduler = BackgroundPollScheduler::new();
        let now = Instant::now();
        let expected_deadline = now + BACKGROUND_JOB_POLL_INTERVAL;

        let action = scheduler.after_render(RedrawPacing::new(false, false), true, now);

        assert_eq!(wait_until_deadline(action), expected_deadline);
        assert!(!action.request_redraw);
        assert_eq!(
            scheduler.next_background_job_poll_at,
            Some(expected_deadline)
        );
    }

    /// Проверяет, что due deadline перед idle wait запрашивает redraw и переносит deadline.
    #[test]
    fn due_background_poll_requests_redraw() {
        let mut scheduler = BackgroundPollScheduler::new();
        let now = Instant::now();
        let due_at = now + BACKGROUND_JOB_POLL_INTERVAL;

        scheduler.after_render(RedrawPacing::new(false, false), true, now);
        let action = scheduler.before_idle_wait(false, true, due_at);

        assert_eq!(
            wait_until_deadline(action),
            due_at + BACKGROUND_JOB_POLL_INTERVAL
        );
        assert!(action.request_redraw);
    }

    /// Проверяет, что ранний wakeup до deadline не создаёт лишний redraw.
    #[test]
    fn not_due_background_poll_does_not_request_redraw() {
        let mut scheduler = BackgroundPollScheduler::new();
        let now = Instant::now();
        let expected_deadline = now + BACKGROUND_JOB_POLL_INTERVAL;
        let before_deadline = now + (BACKGROUND_JOB_POLL_INTERVAL / 2);

        scheduler.after_render(RedrawPacing::new(false, false), true, now);
        let action = scheduler.before_idle_wait(false, true, before_deadline);

        assert_eq!(wait_until_deadline(action), expected_deadline);
        assert!(!action.request_redraw);
    }
}
