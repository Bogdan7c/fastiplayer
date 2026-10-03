//! Runtime-исполнитель transport-команд, которые controller guard не выполнил сразу.
//!
//! Граница ответственности:
//! - controller решает, что делать с командой во время install (`TransportGuardOutcome`)
//!   и хранит одну отложенную команду после terminal drain;
//! - runtime (этот модуль) пересчитывает команду с актуальным контекстом
//!   (D17 threshold, D50 discovery availability) и говорит app, какой pending request
//!   нужно освободить;
//! - app (`transport_runtime`) применяет результат тем же adapter-ом, что и обычное нажатие.

use std::time::Duration;

use player_core::MediaInstallCancellationCause;

use super::PlaylistRuntime;
use super::controller::{
    DeferredTransportExecutionContext, DeferredTransportExecutionOutcome,
    PlaylistControllerInvariantViolation, TransportGuardOutcome,
};
use crate::media_open::MediaOpenRequestId;

/// Pending media open, который controller уже снял со своей стороны.
///
/// App обязан отменить его у coordinator-а до запуска нового install-а: иначе старый
/// request дойдёт до Installed уже без playlist binding-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReleasedPendingRequest {
    pub request_id: MediaOpenRequestId,
    pub cause: MediaInstallCancellationCause,
}

/// Что app должен сделать с transport-командой, которую остановил install guard.
pub(crate) enum GuardedTransportFollowUp {
    /// Pending install не мешает: результат применяется сразу.
    Execute(DeferredTransportExecutionOutcome),
    /// Controller снял pending install; app отменяет его request и применяет результат.
    ExecuteAfterRelease {
        released: ReleasedPendingRequest,
        executed: DeferredTransportExecutionOutcome,
    },
    /// Install уже за barrier-ом: команда сохранена и исполнится после terminal drain.
    AwaitTerminal { request_id: MediaOpenRequestId },
    /// Controller отсутствует (load gate ещё закрыт) — исполнять нечем.
    ControllerUnavailable,
    /// Controller в fatal invariant state; команда не исполняется.
    Fatal(PlaylistControllerInvariantViolation),
}

impl PlaylistRuntime {
    /// Переводит guard outcome в конкретную инструкцию для app adapter-а.
    ///
    /// `current_position` — позиция текущего active media (нужна D17 для Previous).
    pub(crate) fn resolve_transport_guard(
        &mut self,
        guard: TransportGuardOutcome,
        current_position: Duration,
    ) -> GuardedTransportFollowUp {
        match guard {
            TransportGuardOutcome::ExecuteNow {
                intent,
                aborted_request_id,
                cancellation_cause,
                // Mode dirty уже записан в controller dirty revision и публикуется ниже.
                mode_dirty: _,
            } => {
                let Some(executed) = self.execute_deferred_transport(intent, current_position)
                else {
                    return GuardedTransportFollowUp::ControllerUnavailable;
                };
                match aborted_request_id {
                    // Reserved-before-dispatch: domain token уже abort-нут, request ещё жив.
                    Some(request_id) => GuardedTransportFollowUp::ExecuteAfterRelease {
                        released: ReleasedPendingRequest {
                            request_id,
                            // Guard всегда передаёт cause вместе с request ID; fallback —
                            // та же cause, которую guard вычисляет из intent-а.
                            cause: cancellation_cause.unwrap_or(intent.cancellation_cause()),
                        },
                        executed,
                    },
                    // Pending install не было — освобождать нечего.
                    None => GuardedTransportFollowUp::Execute(executed),
                }
            }
            TransportGuardOutcome::CancelPendingThenExecute {
                request_id,
                cause,
                intent,
            } => {
                let Some(executed) = self.execute_deferred_transport(intent, current_position)
                else {
                    return GuardedTransportFollowUp::ControllerUnavailable;
                };
                GuardedTransportFollowUp::ExecuteAfterRelease {
                    released: ReleasedPendingRequest { request_id, cause },
                    executed,
                }
            }
            TransportGuardOutcome::AwaitAuthorizationResolution { request_id }
            | TransportGuardOutcome::AwaitInstalled { request_id } => {
                GuardedTransportFollowUp::AwaitTerminal { request_id }
            }
            TransportGuardOutcome::Fatal(violation) => GuardedTransportFollowUp::Fatal(violation),
        }
    }

    /// Исполняет команду, сохранённую controller-ом при terminal drain, ровно один раз.
    ///
    /// App вызывает это только когда strong media open slot свободен: так новая команда
    /// не конкурирует с ещё не завершённым request-ом.
    pub(crate) fn take_terminal_transport_execution(
        &mut self,
        current_position: Duration,
    ) -> Option<DeferredTransportExecutionOutcome> {
        let intent = self.controller.as_mut()?.take_terminal_transport_intent()?;
        self.execute_deferred_transport(intent, current_position)
    }

    /// Общий путь исполнения: контекст из runtime-owned settings/discovery и publish dirty.
    fn execute_deferred_transport(
        &mut self,
        intent: super::controller::DeferredTransportIntent,
        current_position: Duration,
    ) -> Option<DeferredTransportExecutionOutcome> {
        let context = DeferredTransportExecutionContext {
            current_position,
            previous_restart_threshold: self.previous_restart_threshold(),
            wait_availability: self.discovery.manual_wait_availability(),
        };
        let controller = self.controller.as_mut()?;
        let dirty_before = controller.dirty_revision();
        let executed = controller.execute_deferred_transport_intent(intent, context);
        // Как и `request_playlist_navigation`: D50 wait должен видеть актуальный interest.
        self.discovery.synchronize_navigation_interest(controller);
        // Reserved abort применяет desired modes; persistence получает их одним publish.
        self.publish_controller_mutation_if_dirty(dirty_before);
        Some(executed)
    }
}
