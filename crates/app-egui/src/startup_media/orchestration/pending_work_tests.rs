use super::*;

/// Scheduler продолжает polling во всех loading/applying фазах и останавливается на terminal.
#[test]
fn startup_phases_report_pending_work_without_continuous_playback() {
    let mut orchestration = StartupMediaOrchestration::new(false);

    for pending_phase in [
        StartupMediaPhase::WaitingForRuntime,
        StartupMediaPhase::Preparing,
        StartupMediaPhase::PreparedAwaitingAllocator,
        StartupMediaPhase::Applying,
    ] {
        orchestration.phase = pending_phase;
        assert!(orchestration.has_pending_work());
    }

    for terminal_phase in [
        StartupMediaPhase::Activated,
        StartupMediaPhase::Idle,
        StartupMediaPhase::Failed,
        StartupMediaPhase::Shutdown,
    ] {
        orchestration.phase = terminal_phase;
        assert!(!orchestration.has_pending_work());
    }
}
