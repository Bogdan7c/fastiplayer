//! App adapter для transport-команд, которые остановил playlist install guard.
//!
//! Две точки входа:
//! - `apply_guarded_transport` — сразу после нажатия (Play row / Next / Previous / MPRIS Stop),
//!   когда controller вернул guard outcome вместо обычного результата;
//! - `poll_playlist_transport` — раз за кадр: обычный poll install-а, затем исполнение
//!   команды, которую controller сохранил при terminal drain.
//!
//! Результат исполнения применяется теми же функциями, что и обычное нажатие, поэтому
//! отложенная команда не получает отдельной «упрощённой» семантики.

use std::time::Duration;

use render_wgpu_shell::Renderer;
use tracing::{debug, warn};

use super::{apply_manual_navigation_outcome, apply_playlist_row_play};
use crate::playlist_runtime::{
    DeferredTransportExecutionOutcome, GuardedTransportFollowUp, ManualNavigationCancelOutcome,
    PlaylistRuntime, ReleasedPendingRequest, RuntimeRowPlayOutcome, TransportGuardOutcome,
};
use crate::state::AppState;

/// Исполняет решение guard-а для только что нажатой команды.
///
/// `current_position` — позиция текущего active media (D17 restart для Previous).
pub(crate) fn apply_guarded_transport(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    guard: TransportGuardOutcome,
    current_position: Duration,
) {
    match playlist_runtime.resolve_transport_guard(guard, current_position) {
        GuardedTransportFollowUp::Execute(executed) => {
            apply_deferred_transport_execution(app_state, playlist_runtime, renderer, executed);
        }
        GuardedTransportFollowUp::ExecuteAfterRelease { released, executed } => {
            // Сначала освобождаем старый request: новый install встанет в очередь за ним.
            app_state.release_superseded_playlist_request(playlist_runtime, released);
            apply_deferred_transport_execution(app_state, playlist_runtime, renderer, executed);
        }
        GuardedTransportFollowUp::AwaitTerminal { request_id } => {
            debug!(
                ?request_id,
                "Transport-команда сохранена до terminal текущего install-а"
            );
        }
        GuardedTransportFollowUp::ControllerUnavailable => {
            debug!("Transport-команда пропущена: playlist controller ещё не загружен");
        }
        GuardedTransportFollowUp::Fatal(violation) => {
            warn!(
                ?violation,
                "Transport-команда не исполнена: playlist controller в fatal state"
            );
        }
    }
}

/// Poll playlist install-а и исполнение команды, отложенной до его terminal.
///
/// Отложенная команда стартует только при свободном strong-open слоте: если старый request
/// ещё не отдал terminal (cancel-win ждёт poll), команда подождёт следующий кадр.
pub(crate) fn poll_playlist_transport(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
) {
    app_state.poll_playlist_transport(playlist_runtime, renderer);
    if !app_state.strong_media_open_slot_is_idle() {
        return;
    }
    let current_position = app_state.last_known_player_position();
    if let Some(executed) = playlist_runtime.take_terminal_transport_execution(current_position) {
        apply_deferred_transport_execution(app_state, playlist_runtime, renderer, executed);
    }
}

/// Neutral Stop: exact request на active instance либо guard во время install-а.
pub(crate) fn apply_neutral_stop_request(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    stop: Option<Result<player_core::ExactMediaTransportRequest, TransportGuardOutcome>>,
    current_position: Duration,
) {
    match stop {
        Some(Ok(request)) => app_state.dispatch_exact_playlist_transport(request),
        Some(Err(guard)) => apply_guarded_transport(
            app_state,
            playlist_runtime,
            renderer,
            guard,
            current_position,
        ),
        // Active media нет — останавливать нечего.
        None => {}
    }
}

/// Применяет пересчитанную команду тем же adapter-ом, что и обычное нажатие.
fn apply_deferred_transport_execution(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    executed: DeferredTransportExecutionOutcome,
) {
    let current_position = app_state.last_known_player_position();
    match executed {
        DeferredTransportExecutionOutcome::PlayItem(outcome) => {
            let applied = apply_playlist_row_play(
                app_state,
                playlist_runtime,
                renderer,
                RuntimeRowPlayOutcome::Controller(outcome),
            );
            if !applied {
                debug!("Отложенный Play row не применён: строка больше не в очереди");
            }
        }
        DeferredTransportExecutionOutcome::Navigation(outcome) => {
            apply_manual_navigation_outcome(app_state, playlist_runtime, renderer, outcome);
        }
        DeferredTransportExecutionOutcome::NeutralStop(stop) => {
            apply_neutral_stop_request(
                app_state,
                playlist_runtime,
                renderer,
                stop,
                current_position,
            );
        }
        DeferredTransportExecutionOutcome::CancelManualNavigation(outcome) => {
            apply_deferred_manual_navigation_cancel(app_state, playlist_runtime, outcome);
        }
    }
}

/// D56 Cancel: снятый с controller-а pending request отменяется так же, как для Play/Next.
fn apply_deferred_manual_navigation_cancel(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    outcome: ManualNavigationCancelOutcome,
) {
    match outcome {
        ManualNavigationCancelOutcome::CancelPending {
            request_id, cause, ..
        } => app_state.release_superseded_playlist_request(
            playlist_runtime,
            ReleasedPendingRequest { request_id, cause },
        ),
        other => debug!(outcome = ?other, "Отложенная отмена навигации применена"),
    }
}
