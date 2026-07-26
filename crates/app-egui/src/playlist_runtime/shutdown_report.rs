pub(super) const fn startup_failed(
    outcome: super::startup::PlaylistStartupShutdownOutcome,
) -> bool {
    matches!(
        outcome,
        super::startup::PlaylistStartupShutdownOutcome::ThreadPanicked
    )
}

pub(super) fn shutdown_persistence_failed(
    persistence: playlist_state::ShutdownPersistenceOutcome,
) -> bool {
    match persistence {
        playlist_state::ShutdownPersistenceOutcome::NoCommittedSnapshot
        | playlist_state::ShutdownPersistenceOutcome::AlreadyDurable { .. } => false,
        playlist_state::ShutdownPersistenceOutcome::Attempted(report) => !matches!(
            report.outcome,
            playlist_state::SaveAttemptOutcome::FullWrite(
                playlist_state::AtomicWriteOutcome::Durable
            ) | playlist_state::SaveAttemptOutcome::DirectoryDurabilityRetry(
                playlist_state::DurabilityRetryOutcome::Durable
            )
        ),
    }
}
