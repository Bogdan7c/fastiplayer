//! Terminal policy process-lifetime владельцев приложения.

use std::time::Duration;

use desktop_integration::DesktopIntegrationShutdownOutcome;
use player_core::PlayerWorkerShutdownOutcome;

use crate::playlist_runtime::PlaylistTerminalShutdownOutcome;
use crate::process_shutdown::ProcessOwnerShutdownOutcome;

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

/// Process lifecycle shell-а отделён от renderer suspend/resume lifecycle-а.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AppShellProcessLifecycle {
    Running,
    ShuttingDown,
    ShutdownCompleted,
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
        AppShellProcessLifecycle::ShutdownCompleted => TerminalEntryDisposition::AlreadyCompleted,
        AppShellProcessLifecycle::ShuttingDown => TerminalEntryDisposition::ExitRequired,
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
        aggregate_terminal_dispositions, desktop_integration_disposition, player_disposition,
        process_owner_disposition, terminal_entry_disposition,
    };
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
