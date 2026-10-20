//! Планировщик пробуждений UI-потока (cpu-parity, сессия 04): события вместо vsync.
//!
//! До сессии 04 при активном воспроизведении shell после каждого кадра просил `request_redraw`
//! и просыпался на каждом frame callback (vsync), делая полный проход даже без изменений.
//! Теперь источники пробуждения — события: ввод, wake worker-а (новый кадр, событие плеера),
//! запросы egui, таймеры UI-модели (тост, автоскрытие, секунда/пиксель позиции) и фоновые
//! задачи. Во время воспроизведения дополнительно работает страховочный таймер
//! [`PLAYBACK_REDRAW_FALLBACK_INTERVAL`]: если какое-то событие потеряно, UI всё равно
//! проверит себя не позже чем через 250 мс (счётчик `redraw_fallback_found_work` покажет,
//! сработала ли страховка по делу).
//!
//! Инвариант: в паузе и простое пробуждений без причины нет (как и раньше — `Wait`).

use std::time::{Duration, Instant};

use winit::{event::WindowEvent, event_loop::ControlFlow};

use crate::startup_media::STARTUP_MEDIA_POLL_INTERVAL;

/// Единый интервал polling-а shell background jobs, когда UI не рисует по событиям.
const BACKGROUND_JOB_POLL_INTERVAL: Duration = STARTUP_MEDIA_POLL_INTERVAL;

/// Страховочный таймер воспроизведения: верхняя граница «UI не проверял себя».
///
/// Это не ритм отрисовки, а сеть под канатоходцем: в норме кадр будят события раньше.
pub(crate) const PLAYBACK_REDRAW_FALLBACK_INTERVAL: Duration = Duration::from_millis(250);

/// Решение render pass-а о следующем пробуждении без внешнего window event-а.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RedrawPacing {
    /// Нужен ещё один проход сразу: egui просил, worker-команда ушла, действия панелей.
    immediate: bool,

    /// Воспроизведение активно: держим страховочный таймер, но НЕ непрерывный redraw.
    playback_active: bool,
}

/// Почему планировщик запросил redraw без внешнего события.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedrawCause {
    /// Кадр сам попросил продолжение (egui, worker-команда, действия панелей).
    Immediate,
    /// Настал дедлайн UI-модели (таймер, egui `request_repaint_after`, позиция).
    UiDeadline,
    /// Настал срок опроса фоновой задачи.
    BackgroundPoll,
    /// Страховочный таймер воспроизведения.
    PlaybackFallback,
}

/// Действие, которое shell должен применить к winit event loop после решения pacing-а.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RedrawControlAction {
    /// Следующий режим ожидания winit event loop.
    pub(crate) control_flow: ControlFlow,

    /// Почему нужно запросить redraw у окна вместе с установкой `control_flow`; `None` — не нужно.
    pub(crate) redraw_cause: Option<RedrawCause>,
}

impl RedrawControlAction {
    /// Собирает action без раскрытия деталей хранения scheduler-а наружу.
    const fn new(control_flow: ControlFlow, redraw_cause: Option<RedrawCause>) -> Self {
        Self {
            control_flow,
            redraw_cause,
        }
    }

    /// Нужно ли запросить redraw у окна.
    #[must_use]
    pub(crate) const fn request_redraw(self) -> bool {
        self.redraw_cause.is_some()
    }
}

impl RedrawPacing {
    /// Создаёт итоговое pacing-решение после кадра.
    pub(crate) const fn new(immediate: bool, playback_active: bool) -> Self {
        Self {
            immediate,
            playback_active,
        }
    }

    /// Возвращает `true`, если следующий redraw надо запросить сразу.
    pub(crate) const fn wants_immediate_redraw(self) -> bool {
        self.immediate
    }

    /// Возвращает `true`, если воспроизведение активно и нужен страховочный таймер.
    pub(crate) const fn playback_active(self) -> bool {
        self.playback_active
    }
}

/// Scheduler timed wakeup-ов: фоновые задачи, дедлайны UI-модели и страховка воспроизведения.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BackgroundPollScheduler {
    /// Ближайший deadline, когда idle loop должен снова проверить background jobs.
    next_background_job_poll_at: Option<Instant>,
    /// Ближайшая видимая UI смена (countdown/expiry/позиция/egui), независимая от background jobs.
    next_ui_wake_at: Option<Instant>,
    /// Страховочный таймер воспроизведения; `None` вне воспроизведения.
    next_playback_fallback_at: Option<Instant>,
}

impl BackgroundPollScheduler {
    /// Создаёт scheduler без запланированных wakeup-ов.
    pub(crate) const fn new() -> Self {
        Self {
            next_background_job_poll_at: None,
            next_ui_wake_at: None,
            next_playback_fallback_at: None,
        }
    }

    /// Возвращает winit action после кадра и обновляет deadline-ы.
    #[cfg(test)]
    pub(crate) fn after_render(
        &mut self,
        pacing: RedrawPacing,
        has_pending_background_job: bool,
        now: Instant,
    ) -> RedrawControlAction {
        self.after_render_with_deadline(pacing, has_pending_background_job, None, now)
    }

    /// Учитывает owner-produced UI deadline без timer thread или idle polling.
    pub(crate) fn after_render_with_deadline(
        &mut self,
        pacing: RedrawPacing,
        has_pending_background_job: bool,
        ui_deadline: Option<Instant>,
        now: Instant,
    ) -> RedrawControlAction {
        self.next_ui_wake_at = ui_deadline;
        self.clear_deadline_if_background_jobs_finished(has_pending_background_job);
        self.rearm_playback_fallback_after_render(pacing.playback_active(), now);

        if pacing.wants_immediate_redraw() {
            return RedrawControlAction::new(ControlFlow::Wait, Some(RedrawCause::Immediate));
        }

        let background =
            self.schedule_idle_background_poll_after_render(has_pending_background_job, now);
        let with_ui = merge_ui_deadline(background, ui_deadline, now);
        merge_playback_fallback(with_ui, self.next_playback_fallback_at)
    }

    /// Возвращает winit action прямо перед idle wait.
    pub(crate) fn before_idle_wait(
        &mut self,
        playback_active: bool,
        has_pending_background_job: bool,
        now: Instant,
    ) -> RedrawControlAction {
        self.clear_deadline_if_background_jobs_finished(has_pending_background_job);
        if !playback_active {
            self.next_playback_fallback_at = None;
        }

        let background =
            self.schedule_background_poll_before_idle_wait(has_pending_background_job, now);
        let with_ui = merge_ui_deadline(background, self.next_ui_wake_at, now);
        if !playback_active {
            return with_ui;
        }
        if with_ui.request_redraw() {
            return with_ui;
        }

        // Страховка воспроизведения: созрела — один redraw и перевзвод, нет — ждём до неё.
        let fallback_at = self
            .next_playback_fallback_at
            .unwrap_or(now + PLAYBACK_REDRAW_FALLBACK_INTERVAL);
        if now >= fallback_at {
            let next_fallback_at = now + PLAYBACK_REDRAW_FALLBACK_INTERVAL;
            self.next_playback_fallback_at = Some(next_fallback_at);
            return RedrawControlAction::new(
                ControlFlow::WaitUntil(next_fallback_at),
                Some(RedrawCause::PlaybackFallback),
            );
        }
        self.next_playback_fallback_at = Some(fallback_at);
        merge_playback_fallback(with_ui, Some(fallback_at))
    }

    /// Сбрасывает stale deadline, когда shell больше не ждёт фоновых задач.
    fn clear_deadline_if_background_jobs_finished(&mut self, has_pending_background_job: bool) {
        if !has_pending_background_job {
            self.next_background_job_poll_at = None;
        }
    }

    /// Кадр только что нарисован (или осознанно пропущен): страховка отсчитывается заново.
    fn rearm_playback_fallback_after_render(&mut self, playback_active: bool, now: Instant) {
        self.next_playback_fallback_at =
            playback_active.then(|| now + PLAYBACK_REDRAW_FALLBACK_INTERVAL);
    }

    /// Планирует первый timed wakeup после idle render-а без немедленного redraw.
    fn schedule_idle_background_poll_after_render(
        &mut self,
        has_pending_background_job: bool,
        now: Instant,
    ) -> RedrawControlAction {
        if !has_pending_background_job {
            return RedrawControlAction::new(ControlFlow::Wait, None);
        }

        let deadline = self
            .next_background_job_poll_at
            .unwrap_or(now + BACKGROUND_JOB_POLL_INTERVAL);
        self.next_background_job_poll_at = Some(deadline);
        RedrawControlAction::new(ControlFlow::WaitUntil(deadline), None)
    }

    /// Будит shell для due background poll-а или оставляет текущий timed wakeup.
    fn schedule_background_poll_before_idle_wait(
        &mut self,
        has_pending_background_job: bool,
        now: Instant,
    ) -> RedrawControlAction {
        if !has_pending_background_job {
            return RedrawControlAction::new(ControlFlow::Wait, None);
        }

        let current_deadline = self.next_background_job_poll_at.unwrap_or(now);
        if now >= current_deadline {
            let next_deadline = now + BACKGROUND_JOB_POLL_INTERVAL;
            self.next_background_job_poll_at = Some(next_deadline);
            return RedrawControlAction::new(
                ControlFlow::WaitUntil(next_deadline),
                Some(RedrawCause::BackgroundPoll),
            );
        }

        self.next_background_job_poll_at = Some(current_deadline);
        RedrawControlAction::new(ControlFlow::WaitUntil(current_deadline), None)
    }
}

/// Вливает дедлайн UI-модели: созревший даёт один redraw, будущий — `WaitUntil`.
fn merge_ui_deadline(
    action: RedrawControlAction,
    ui_deadline: Option<Instant>,
    now: Instant,
) -> RedrawControlAction {
    let Some(ui_deadline) = ui_deadline else {
        return action;
    };
    if ui_deadline <= now {
        return RedrawControlAction::new(ControlFlow::Wait, Some(RedrawCause::UiDeadline));
    }
    RedrawControlAction::new(
        earliest_control_flow(action.control_flow, ui_deadline),
        action.redraw_cause,
    )
}

/// Вливает страховочный таймер воспроизведения как ещё один `WaitUntil` без redraw.
fn merge_playback_fallback(
    action: RedrawControlAction,
    fallback_at: Option<Instant>,
) -> RedrawControlAction {
    let Some(fallback_at) = fallback_at else {
        return action;
    };
    if action.request_redraw() {
        return action;
    }
    RedrawControlAction::new(
        earliest_control_flow(action.control_flow, fallback_at),
        None,
    )
}

/// Ближайший из уже выбранного `ControlFlow` и нового дедлайна.
fn earliest_control_flow(existing: ControlFlow, deadline: Instant) -> ControlFlow {
    match existing {
        ControlFlow::WaitUntil(existing_deadline) => {
            ControlFlow::WaitUntil(existing_deadline.min(deadline))
        }
        ControlFlow::Wait | ControlFlow::Poll => ControlFlow::WaitUntil(deadline),
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
                panic!("expected WaitUntil(deadline), received {other_control_flow:?}");
            }
        }
    }

    /// Проверяет, что immediate pacing требует следующий redraw.
    #[test]
    fn immediate_pacing_requests_immediate_redraw() {
        let pacing = RedrawPacing::new(true, false);

        assert!(pacing.wants_immediate_redraw());
        assert!(!pacing.playback_active());
    }

    /// Проверяет, что idle frame без причин остаётся в Wait mode.
    #[test]
    fn idle_pacing_does_not_request_immediate_redraw() {
        let pacing = RedrawPacing::new(false, false);

        assert!(!pacing.wants_immediate_redraw());
        assert!(!pacing.playback_active());
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

    /// Сессия 04: воспроизведение больше НЕ просит redraw после каждого кадра —
    /// только страховочный `WaitUntil` на 250 мс. Падает на прежней политике «Wait + redraw».
    #[test]
    fn after_render_playback_waits_for_events_with_fallback_deadline() {
        let mut scheduler = BackgroundPollScheduler::new();
        let now = Instant::now();

        let action = scheduler.after_render(RedrawPacing::new(false, true), false, now);

        assert!(
            !action.request_redraw(),
            "playback must not spin on vsync any more"
        );
        assert_eq!(
            wait_until_deadline(action),
            now + PLAYBACK_REDRAW_FALLBACK_INTERVAL
        );
    }

    /// Перед idle wait во время воспроизведения тоже нет redraw, пока страховка не созрела.
    #[test]
    fn before_idle_wait_playback_keeps_fallback_without_redraw_until_due() {
        let mut scheduler = BackgroundPollScheduler::new();
        let now = Instant::now();
        scheduler.after_render(RedrawPacing::new(false, true), false, now);

        let early = scheduler.before_idle_wait(true, false, now + Duration::from_millis(10));
        assert!(!early.request_redraw());
        assert_eq!(
            wait_until_deadline(early),
            now + PLAYBACK_REDRAW_FALLBACK_INTERVAL
        );

        let due_at = now + PLAYBACK_REDRAW_FALLBACK_INTERVAL;
        let due = scheduler.before_idle_wait(true, false, due_at);
        assert_eq!(due.redraw_cause, Some(RedrawCause::PlaybackFallback));
        assert_eq!(
            wait_until_deadline(due),
            due_at + PLAYBACK_REDRAW_FALLBACK_INTERVAL,
            "a due fallback re-arms itself for the next interval"
        );
    }

    /// Нарисованный кадр перевзводит страховку: отсчёт идёт от кадра, а не от старого дедлайна.
    #[test]
    fn rendered_frame_rearms_playback_fallback() {
        let mut scheduler = BackgroundPollScheduler::new();
        let now = Instant::now();
        scheduler.after_render(RedrawPacing::new(false, true), false, now);

        let later = now + Duration::from_millis(100);
        let action = scheduler.after_render(RedrawPacing::new(false, true), false, later);
        assert_eq!(
            wait_until_deadline(action),
            later + PLAYBACK_REDRAW_FALLBACK_INTERVAL
        );
    }

    /// Пауза/простой: страховки нет, остаётся чистый `Wait` без redraw (инвариант 5).
    #[test]
    fn pause_has_no_fallback_timer_and_no_redraw() {
        let mut scheduler = BackgroundPollScheduler::new();
        let now = Instant::now();
        scheduler.after_render(RedrawPacing::new(false, true), false, now);

        let paused = scheduler.after_render(RedrawPacing::new(false, false), false, now);
        assert_eq!(paused.control_flow, ControlFlow::Wait);
        assert!(!paused.request_redraw());

        let idle = scheduler.before_idle_wait(false, false, now + Duration::from_secs(1));
        assert_eq!(idle.control_flow, ControlFlow::Wait);
        assert!(!idle.request_redraw());
    }

    /// Проверяет, что immediate redraw остаётся в Wait mode и не включает busy polling.
    #[test]
    fn after_render_immediate_requests_wait_and_redraw() {
        let mut scheduler = BackgroundPollScheduler::new();
        let now = Instant::now();

        let action = scheduler.after_render(RedrawPacing::new(true, true), false, now);

        assert!(matches!(action.control_flow, ControlFlow::Wait));
        assert_eq!(action.redraw_cause, Some(RedrawCause::Immediate));
    }

    /// Проверяет, что отсутствие background jobs очищает deadline и не будит окно.
    #[test]
    fn no_pending_background_clears_to_wait_without_redraw() {
        let mut scheduler = BackgroundPollScheduler::new();
        let now = Instant::now();

        scheduler.after_render(RedrawPacing::new(false, false), true, now);
        let action = scheduler.after_render(RedrawPacing::new(false, false), false, now);

        assert!(matches!(action.control_flow, ControlFlow::Wait));
        assert!(!action.request_redraw());
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
        assert!(!action.request_redraw());
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
        assert_eq!(action.redraw_cause, Some(RedrawCause::BackgroundPoll));
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
        assert!(!action.request_redraw());
    }

    #[test]
    fn ui_countdown_deadline_waits_once_without_idle_redraw_spin() {
        let mut scheduler = BackgroundPollScheduler::new();
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        let after_render = scheduler.after_render_with_deadline(
            RedrawPacing::new(false, false),
            false,
            Some(deadline),
            now,
        );
        assert_eq!(after_render.control_flow, ControlFlow::WaitUntil(deadline));
        assert!(!after_render.request_redraw());

        let before_deadline = scheduler.before_idle_wait(false, false, now);
        assert_eq!(
            before_deadline.control_flow,
            ControlFlow::WaitUntil(deadline)
        );
        assert!(!before_deadline.request_redraw());

        let due = scheduler.before_idle_wait(false, false, deadline);
        assert_eq!(due.redraw_cause, Some(RedrawCause::UiDeadline));
        let cleared = scheduler.after_render(RedrawPacing::new(false, false), false, deadline);
        assert_eq!(cleared.control_flow, ControlFlow::Wait);
        assert!(!cleared.request_redraw());
    }

    /// Дедлайн позиции (секунда/пиксель) короче страховки: ждём до него, а не до 250 мс.
    #[test]
    fn earlier_ui_deadline_wins_over_playback_fallback() {
        let mut scheduler = BackgroundPollScheduler::new();
        let now = Instant::now();
        let position_deadline = now + Duration::from_millis(70);

        let action = scheduler.after_render_with_deadline(
            RedrawPacing::new(false, true),
            false,
            Some(position_deadline),
            now,
        );

        assert_eq!(wait_until_deadline(action), position_deadline);
        assert!(!action.request_redraw());
    }
}
