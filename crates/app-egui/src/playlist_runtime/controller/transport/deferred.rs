//! Исполнение transport-команд, которые D08/D39 guard не дал выполнить сразу.
//!
//! Controller здесь только *решает*: он пересчитывает сохранённый intent через обычные
//! entry points (`play_item`, `manual_navigation`, `neutral_stop`,
//! `cancel_manual_navigation`) и возвращает typed outcome. Side effects (отмена media open,
//! запуск install, exact player request) исполняет app через тот же adapter, что и для
//! обычного нажатия.
//!
//! Инвариант latest-only: после terminal drain хранится не больше одной команды, и любая
//! новая transport-команда пользователя вытесняет её. Command FIFO здесь нет.

use std::time::Duration;

use player_core::ExactMediaTransportRequest;

use super::{
    ControllerManualNavigationOutcome, ControllerPlayItemOutcome, DiscoveryManualWaitAvailability,
    PreviousRestartThreshold, TransportGuardOutcome,
};
use crate::playlist_runtime::controller::PlaylistController;
use crate::playlist_runtime::controller::install::{
    ControllerTerminalDrain, DeferredControllerIntent, DeferredTransportIntent,
};
use crate::playlist_runtime::controller::manual_navigation::ManualNavigationCancelOutcome;

/// Контекст повторной оценки ровно одного transport-intent.
///
/// Значения передаёт runtime: controller не читает player snapshot и settings сам.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeferredTransportExecutionContext {
    /// Позиция текущего active media для D17 restart-current у Previous.
    pub current_position: Duration,
    /// D17 порог «Previous перезапускает текущий трек».
    pub previous_restart_threshold: PreviousRestartThreshold,
    /// D50: может ли discovery ещё добавить item для ожидания Next/Previous.
    pub wait_availability: DiscoveryManualWaitAvailability,
}

/// Результат исполнения не смешивает разные player/domain boundaries.
pub(crate) enum DeferredTransportExecutionOutcome {
    /// Play row: restart/coalesce/новый install.
    PlayItem(ControllerPlayItemOutcome),
    /// Next/Previous: тот же набор outcome-ов, что у обычной навигации.
    Navigation(ControllerManualNavigationOutcome),
    /// Neutral Stop: exact request на active instance, `None` если active media нет.
    NeutralStop(Option<Result<ExactMediaTransportRequest, TransportGuardOutcome>>),
    /// Отмена ручной навигации (D56).
    CancelManualNavigation(ManualNavigationCancelOutcome),
}

impl PlaylistController {
    /// Повторно прогоняет сохранённый intent через обычный controller entry point.
    ///
    /// Вызывается только когда guard уже снят: либо guard сам снял pending install
    /// (`ExecuteNow`/`CancelPendingThenExecute`), либо install дошёл до terminal drain.
    pub(crate) fn execute_deferred_transport_intent(
        &mut self,
        intent: DeferredTransportIntent,
        context: DeferredTransportExecutionContext,
    ) -> DeferredTransportExecutionOutcome {
        match intent {
            DeferredTransportIntent::PlayItem { item_id, origin } => {
                DeferredTransportExecutionOutcome::PlayItem(self.play_item(item_id, origin))
            }
            DeferredTransportIntent::Navigate { direction, origin } => {
                DeferredTransportExecutionOutcome::Navigation(self.manual_navigation(
                    direction,
                    origin,
                    context.current_position,
                    context.previous_restart_threshold,
                    context.wait_availability,
                ))
            }
            DeferredTransportIntent::Stop { origin } => {
                DeferredTransportExecutionOutcome::NeutralStop(self.neutral_stop(origin))
            }
            DeferredTransportIntent::CancelManualNavigation => {
                DeferredTransportExecutionOutcome::CancelManualNavigation(
                    self.cancel_manual_navigation(),
                )
            }
        }
    }

    /// Terminal drain передаёт свой deferred transport-intent в слот исполнения.
    ///
    /// Lifecycle intent (`Suspend`) сюда не попадает: им владеет suspend orchestration.
    /// Drain по-прежнему возвращает intent как факт для caller-а; слот лишь гарантирует,
    /// что app исполнит его ровно один раз.
    pub(in crate::playlist_runtime::controller) fn retain_terminal_transport_intent(
        &mut self,
        drain: &ControllerTerminalDrain,
    ) {
        if let Some(DeferredControllerIntent::Transport(intent)) = drain.deferred_intent {
            self.terminal_transport_intent = Some(intent);
        }
    }

    /// Новая команда пользователя вытесняет ещё не исполненную отложенную (latest-only).
    pub(in crate::playlist_runtime::controller) fn supersede_terminal_transport_intent(&mut self) {
        self.terminal_transport_intent = None;
    }

    /// Забирает отложенную после terminal drain команду ровно один раз.
    pub(crate) fn take_terminal_transport_intent(&mut self) -> Option<DeferredTransportIntent> {
        self.terminal_transport_intent.take()
    }
}
