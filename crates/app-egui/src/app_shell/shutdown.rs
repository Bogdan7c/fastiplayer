//! Terminal policy process-lifetime владельцев приложения.

use std::time::Duration;

use desktop_integration::DesktopIntegrationShutdownOutcome;
use player_core::PlayerWorkerShutdownOutcome;

use super::AppShell;
use crate::app_instance::ShutdownQuality;
use crate::playlist_runtime::PlaylistTerminalShutdownOutcome;
use crate::process_shutdown::{ProcessOwnerShutdownOutcome, ShutdownDeadline};
use crate::state::{AppState, LifecycleTimelineSeekSettlement};
use tracing::debug;

/// Единый process-terminal бюджет всех фоновых владельцев приложения.
///
/// Пять секунд дают cooperative owners время завершить I/O, но не позволяют
/// закрытию окна превратиться в неограниченный join. Все owners получают один
/// абсолютный deadline и делят этот бюджет последовательно.
pub(super) const PROCESS_TERMINAL_SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);

/// Suspend ждёт уже принятый exact seek достаточно долго для обычного local commit-а.
///
/// Один общий абсолютный deadline ограничивает все pending receipts. После секунды
/// lifecycle явно выбирает документированную pre-seek позицию и освобождает runtime.
pub(super) const TIMELINE_SEEK_LIFECYCLE_SETTLEMENT_BUDGET: Duration = Duration::from_secs(1);

/// Ненулевой код означает, что process обязан завершиться, не освобождая lease через `Drop`.
pub(super) const TERMINAL_SHUTDOWN_TIMEOUT_EXIT_CODE: i32 = 70;

/// Публикует typed lifecycle settlement без выдачи timeout-а за успешный seek commit.
pub(super) fn report_timeline_seek_lifecycle_settlement(
    settlement: LifecycleTimelineSeekSettlement,
) {
    match settlement {
        LifecycleTimelineSeekSettlement::NoPendingSeek => {}
        LifecycleTimelineSeekSettlement::Settled {
            checkpoint_position,
        } => debug!(
            ?checkpoint_position,
            "Lifecycle wait terminal outcomes everyone pending timeline seek-owls"
        ),
        LifecycleTimelineSeekSettlement::DeadlineElapsed {
            checkpoint_position,
            abandoned_receipt_count,
        } => tracing::warn!(
            ?checkpoint_position,
            abandoned_receipt_count,
            "Lifecycle deadline cancels pending timeline seek; keep pre-seek position"
        ),
        LifecycleTimelineSeekSettlement::MissingOwnerOutcome {
            checkpoint_position,
            abandoned_receipt_count,
        } => tracing::error!(
            ?checkpoint_position,
            abandoned_receipt_count,
            "Player owner loses pending timeline seek outcome; save pre-see position"
        ),
    }
}

/// Process lifecycle shell-а отделён от renderer suspend/resume lifecycle-а.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AppShellProcessLifecycle {
    Running,
    ShuttingDown,
    ShutdownCompleted,
    /// Закрытие дошло до конца, но с типизированной ошибкой владельца (не таймаут:
    /// таймаут завершает процесс кодом 70). Перезапуск после такого выхода не делается.
    ShutdownCompletedWithErrors,
}

/// Pure решение для повторного входа в terminal lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TerminalEntryDisposition {
    Begin,
    AlreadyCompleted,
    ExitRequired,
}

pub(super) const fn terminal_entry_disposition(
    lifecycle: AppShellProcessLifecycle,
) -> TerminalEntryDisposition {
    match lifecycle {
        AppShellProcessLifecycle::Running => TerminalEntryDisposition::Begin,
        AppShellProcessLifecycle::ShutdownCompleted
        | AppShellProcessLifecycle::ShutdownCompletedWithErrors => {
            TerminalEntryDisposition::AlreadyCompleted
        }
        AppShellProcessLifecycle::ShuttingDown => TerminalEntryDisposition::ExitRequired,
    }
}

/// Качество закрытия для решения о перезапуске: чистым считается только полностью
/// завершённое закрытие без ошибок владельцев. Любое другое состояние (закрытие не
/// запускалось, ещё идёт, закончилось ошибкой) перезапуск запрещает.
pub(super) const fn shutdown_quality_of(lifecycle: AppShellProcessLifecycle) -> ShutdownQuality {
    match lifecycle {
        AppShellProcessLifecycle::ShutdownCompleted => ShutdownQuality::Clean,
        AppShellProcessLifecycle::Running
        | AppShellProcessLifecycle::ShuttingDown
        | AppShellProcessLifecycle::ShutdownCompletedWithErrors => ShutdownQuality::Degraded,
    }
}

/// Итоговое состояние shell-а после завершённого закрытия владельцев.
///
/// `ExitRequired` сюда не доходит (процесс уже завершён кодом 70); защитно он приравнен к
/// закрытию с ошибкой.
pub(super) const fn lifecycle_after_shutdown(
    disposition: OwnerTerminalDisposition,
) -> AppShellProcessLifecycle {
    match disposition {
        OwnerTerminalDisposition::Completed => AppShellProcessLifecycle::ShutdownCompleted,
        OwnerTerminalDisposition::Failed | OwnerTerminalDisposition::ExitRequired => {
            AppShellProcessLifecycle::ShutdownCompletedWithErrors
        }
    }
}

impl AppShell {
    /// Bounded-завершает AppState, который ещё не был установлен в shell.
    pub(super) fn shutdown_uninstalled_app_state_or_exit(app_state: &mut AppState) {
        let player = app_state
            .shutdown_player_until(ShutdownDeadline::after(PROCESS_TERMINAL_SHUTDOWN_BUDGET));
        let disposition = player_disposition(player);
        match disposition {
            OwnerTerminalDisposition::ExitRequired => {
                tracing::error!(
                    ?player,
                    "AppState process owner fails after renderer construction error"
                );
                std::process::exit(TERMINAL_SHUTDOWN_TIMEOUT_EXIT_CODE);
            }
            OwnerTerminalDisposition::Failed => {
                tracing::warn!(
                    ?player,
                    "AppState process owner ended terminal failure after construction error"
                );
            }
            OwnerTerminalDisposition::Completed => {}
        }
    }
}

/// Нормализованная политика одного typed owner outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OwnerTerminalDisposition {
    Completed,
    Failed,
    ExitRequired,
}

/// Полный typed отчёт process shutdown; секретные media/config значения сюда не входят.
#[derive(Debug)]
pub(super) struct AppShellShutdownReport {
    pub(super) desktop_integration: Option<DesktopIntegrationShutdownOutcome>,
    pub(super) player: Option<PlayerWorkerShutdownOutcome>,
    pub(super) local_file_open: ProcessOwnerShutdownOutcome,
    pub(super) startup_media: ProcessOwnerShutdownOutcome,
    pub(super) settings: ProcessOwnerShutdownOutcome,
    pub(super) playlist: PlaylistTerminalShutdownOutcome,
}

impl AppShellShutdownReport {
    /// Выбирает наиболее строгую политику: live thread требует process exit,
    /// terminal failure без live thread разрешает обычный teardown с warning.
    pub(super) fn terminal_disposition(&self) -> OwnerTerminalDisposition {
        let outcomes = [
            self.desktop_integration.map_or(
                OwnerTerminalDisposition::Completed,
                desktop_integration_disposition,
            ),
            self.player
                .map_or(OwnerTerminalDisposition::Completed, player_disposition),
            process_owner_disposition(self.local_file_open),
            process_owner_disposition(self.startup_media),
            process_owner_disposition(self.settings),
            playlist_disposition(self.playlist),
        ];
        aggregate_terminal_dispositions(outcomes)
    }
}

/// Pure mapping MPRIS backend outcome в process policy.
pub(super) const fn desktop_integration_disposition(
    outcome: DesktopIntegrationShutdownOutcome,
) -> OwnerTerminalDisposition {
    match outcome {
        DesktopIntegrationShutdownOutcome::Completed
        | DesktopIntegrationShutdownOutcome::AlreadyCompleted => {
            OwnerTerminalDisposition::Completed
        }
        DesktopIntegrationShutdownOutcome::TimedOut => OwnerTerminalDisposition::ExitRequired,
        DesktopIntegrationShutdownOutcome::ThreadPanicked
        | DesktopIntegrationShutdownOutcome::TransportFailed(_) => OwnerTerminalDisposition::Failed,
    }
}

/// Pure mapping player shutdown outcome в process policy.
pub(super) const fn player_disposition(
    outcome: PlayerWorkerShutdownOutcome,
) -> OwnerTerminalDisposition {
    match outcome {
        PlayerWorkerShutdownOutcome::Completed { .. }
        | PlayerWorkerShutdownOutcome::AlreadyCompleted => OwnerTerminalDisposition::Completed,
        PlayerWorkerShutdownOutcome::ThreadPanicked { .. } => OwnerTerminalDisposition::Failed,
        PlayerWorkerShutdownOutcome::TimedOut { .. } => OwnerTerminalDisposition::ExitRequired,
    }
}

/// Pure mapping общего owner outcome в process policy.
pub(super) const fn process_owner_disposition(
    outcome: ProcessOwnerShutdownOutcome,
) -> OwnerTerminalDisposition {
    match outcome {
        ProcessOwnerShutdownOutcome::Completed | ProcessOwnerShutdownOutcome::AlreadyCompleted => {
            OwnerTerminalDisposition::Completed
        }
        ProcessOwnerShutdownOutcome::TimedOut { .. }
        | ProcessOwnerShutdownOutcome::ThreadPanicked {
            pending_threads: 1..,
            ..
        } => OwnerTerminalDisposition::ExitRequired,
        ProcessOwnerShutdownOutcome::ThreadPanicked {
            pending_threads: 0, ..
        } => OwnerTerminalDisposition::Failed,
    }
}

/// Pure mapping playlist aggregate outcome в process policy.
const fn playlist_disposition(
    outcome: PlaylistTerminalShutdownOutcome,
) -> OwnerTerminalDisposition {
    match outcome {
        PlaylistTerminalShutdownOutcome::Completed(_)
        | PlaylistTerminalShutdownOutcome::AlreadyCompleted => OwnerTerminalDisposition::Completed,
        PlaylistTerminalShutdownOutcome::Failed(_) => OwnerTerminalDisposition::Failed,
        PlaylistTerminalShutdownOutcome::ExitRequired(_) => OwnerTerminalDisposition::ExitRequired,
    }
}

/// Агрегирует owner policies без потери различия между failure и live timeout.
pub(super) fn aggregate_terminal_dispositions(
    outcomes: impl IntoIterator<Item = OwnerTerminalDisposition>,
) -> OwnerTerminalDisposition {
    let mut aggregate = OwnerTerminalDisposition::Completed;
    for outcome in outcomes {
        match outcome {
            OwnerTerminalDisposition::ExitRequired => {
                return OwnerTerminalDisposition::ExitRequired;
            }
            OwnerTerminalDisposition::Failed => aggregate = OwnerTerminalDisposition::Failed,
            OwnerTerminalDisposition::Completed => {}
        }
    }
    aggregate
}

#[cfg(test)]
mod tests {
    use desktop_integration::{
        DesktopIntegrationShutdownOutcome, DesktopIntegrationShutdownTransportFailure,
    };
    use player_core::{PlayerWorkerShutdownOutcome, PlayerWorkerShutdownRequestOutcome};

    use super::{
        AppShellProcessLifecycle, OwnerTerminalDisposition, TerminalEntryDisposition,
        aggregate_terminal_dispositions, desktop_integration_disposition, lifecycle_after_shutdown,
        player_disposition, process_owner_disposition, shutdown_quality_of,
        terminal_entry_disposition,
    };
    use crate::app_instance::ShutdownQuality;
    use crate::process_shutdown::ProcessOwnerShutdownOutcome;

    #[test]
    fn terminal_entry_is_idempotent_and_never_reenters_in_progress_shutdown() {
        assert_eq!(
            terminal_entry_disposition(AppShellProcessLifecycle::Running),
            TerminalEntryDisposition::Begin
        );
        assert_eq!(
            terminal_entry_disposition(AppShellProcessLifecycle::ShutdownCompleted),
            TerminalEntryDisposition::AlreadyCompleted
        );
        assert_eq!(
            terminal_entry_disposition(AppShellProcessLifecycle::ShuttingDown),
            TerminalEntryDisposition::ExitRequired
        );
    }

    #[test]
    fn shutdown_with_errors_is_remembered_but_stays_terminal() {
        assert_eq!(
            lifecycle_after_shutdown(OwnerTerminalDisposition::Completed),
            AppShellProcessLifecycle::ShutdownCompleted
        );
        for disposition in [
            OwnerTerminalDisposition::Failed,
            OwnerTerminalDisposition::ExitRequired,
        ] {
            let lifecycle = lifecycle_after_shutdown(disposition);
            assert_eq!(
                lifecycle,
                AppShellProcessLifecycle::ShutdownCompletedWithErrors
            );
            assert_eq!(
                terminal_entry_disposition(lifecycle),
                TerminalEntryDisposition::AlreadyCompleted
            );
        }
    }

    #[test]
    fn only_a_fully_clean_shutdown_allows_relaunch() {
        use AppShellProcessLifecycle::*;
        assert_eq!(
            shutdown_quality_of(ShutdownCompleted),
            ShutdownQuality::Clean
        );
        for lifecycle in [Running, ShuttingDown, ShutdownCompletedWithErrors] {
            assert_eq!(shutdown_quality_of(lifecycle), ShutdownQuality::Degraded);
        }
    }

    #[test]
    fn owner_outcomes_keep_failure_separate_from_live_timeout() {
        let request = PlayerWorkerShutdownRequestOutcome::CommandAndCancellationAccepted;
        assert_eq!(
            player_disposition(PlayerWorkerShutdownOutcome::ThreadPanicked { request }),
            OwnerTerminalDisposition::Failed
        );
        assert_eq!(
            player_disposition(PlayerWorkerShutdownOutcome::TimedOut { request }),
            OwnerTerminalDisposition::ExitRequired
        );
        assert_eq!(
            process_owner_disposition(ProcessOwnerShutdownOutcome::ThreadPanicked {
                panicked_threads: 1,
                pending_threads: 0,
            }),
            OwnerTerminalDisposition::Failed
        );
        assert_eq!(
            process_owner_disposition(ProcessOwnerShutdownOutcome::ThreadPanicked {
                panicked_threads: 1,
                pending_threads: 1,
            }),
            OwnerTerminalDisposition::ExitRequired
        );
        assert_eq!(
            process_owner_disposition(ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 1 }),
            OwnerTerminalDisposition::ExitRequired
        );
        assert_eq!(
            desktop_integration_disposition(DesktopIntegrationShutdownOutcome::TimedOut),
            OwnerTerminalDisposition::ExitRequired
        );
        assert_eq!(
            desktop_integration_disposition(DesktopIntegrationShutdownOutcome::ThreadPanicked),
            OwnerTerminalDisposition::Failed
        );
        assert_eq!(
            desktop_integration_disposition(DesktopIntegrationShutdownOutcome::TransportFailed(
                DesktopIntegrationShutdownTransportFailure::ControlChannelDisconnected,
            )),
            OwnerTerminalDisposition::Failed
        );
    }

    #[test]
    fn aggregate_prefers_timeout_then_terminal_failure_then_success() {
        assert_eq!(
            aggregate_terminal_dispositions([
                OwnerTerminalDisposition::Completed,
                OwnerTerminalDisposition::Completed,
            ]),
            OwnerTerminalDisposition::Completed
        );
        assert_eq!(
            aggregate_terminal_dispositions([
                OwnerTerminalDisposition::Completed,
                OwnerTerminalDisposition::Failed,
            ]),
            OwnerTerminalDisposition::Failed
        );
        assert_eq!(
            aggregate_terminal_dispositions([
                OwnerTerminalDisposition::Failed,
                OwnerTerminalDisposition::ExitRequired,
                OwnerTerminalDisposition::Completed,
            ]),
            OwnerTerminalDisposition::ExitRequired
        );
    }
}
