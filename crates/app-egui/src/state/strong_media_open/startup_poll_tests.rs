use super::*;

fn positioned_at(target: std::time::Duration) -> player_core::PreparedInitialPosition {
    let target = media_core::MediaTime::from_duration(target);
    player_core::PreparedInitialPosition::PositionedAt {
        target_position: target,
        landing_policy: player_core::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        result: media_core::DemuxSeekResult {
            requested_position: target,
            actual_position: target.saturating_sub(media_core::MediaDuration::from_secs(5)),
            actual_track_timestamp: None,
        },
    }
}

#[test]
fn prepared_position_strategy_is_derived_from_exact_restore_contract() {
    let target = std::time::Duration::from_secs(355);
    assert_eq!(
        prepared_position_restore_strategy(
            positioned_at(target),
            crate::playlist_runtime::StartupPosition::Restore(target),
        ),
        Ok(PreparedPositionRestoreStrategy::AdoptPreparedInitialPosition)
    );
    assert_eq!(
        prepared_position_restore_strategy(
            player_core::PreparedInitialPosition::Beginning,
            crate::playlist_runtime::StartupPosition::Restore(target),
        ),
        Ok(PreparedPositionRestoreStrategy::SeekAfterInstall)
    );
}

#[test]
fn prepared_position_strategy_rejects_missing_or_different_restore_target() {
    let prepared_target = std::time::Duration::from_secs(355);
    assert_eq!(
        prepared_position_restore_strategy(
            positioned_at(prepared_target),
            crate::playlist_runtime::StartupPosition::KeepStart,
        ),
        Err(PreparedPositionRestoreContractError::MissingRestoreTarget)
    );
    assert_eq!(
        prepared_position_restore_strategy(
            positioned_at(prepared_target),
            crate::playlist_runtime::StartupPosition::Restore(std::time::Duration::from_secs(180),),
        ),
        Err(PreparedPositionRestoreContractError::TargetMismatch {
            prepared_target,
            restore_target: std::time::Duration::from_secs(180),
        })
    );
}

/// Startup orchestration не должна снова вызвать blocking compatibility wrapper.
#[test]
fn startup_orchestration_uses_only_stepwise_strong_install_boundary() {
    let orchestration_source = include_str!("../../startup_media/orchestration.rs");
    let pending_install_source = include_str!("../../startup_media/pending_install.rs");

    assert!(orchestration_source.contains("begin_prepared_media_strong("));
    assert!(pending_install_source.contains("poll_prepared_media_strong("));
    assert!(!orchestration_source.contains("install_prepared_media_strong("));
    assert!(!pending_install_source.contains("install_prepared_media_strong("));
    assert!(!orchestration_source.contains("wait_for_media_open_progress("));
    assert!(!pending_install_source.contains("wait_for_media_open_progress("));
    assert!(!orchestration_source.contains("wait_for_outcome("));
    assert!(!pending_install_source.contains("wait_for_outcome("));
}

/// Visual checkpoint принадлежит pending-транзакции и не переснимается после Installed.
#[test]
fn same_lineage_visual_checkpoint_crosses_install_barrier_in_order() {
    let pending_source = include_str!("pending.rs");
    let resume_source = include_str!("pending/resume.rs");

    let last_capture = pending_source
        .rfind("capture_same_lineage_restore_before_barrier(")
        .expect("same-lineage path must capture the pre-barrier checkpoint");
    let authorization = pending_source
        .find("authorize_ready_same_lineage_media_open(")
        .expect("same-lineage path must cross the explicit authorization barrier");
    assert!(last_capture < authorization);

    assert!(!resume_source.contains("capture_backend_swap_video_checkpoint("));
    let terminal_finish = resume_source
        .find("installed_media_from_terminal(source, terminal)")
        .expect("Installed terminal facts must be captured before visual activation");
    let video_commit = resume_source
        .find("commit_installed_video_candidate(")
        .expect("Installed video candidate must commit before visual activation");
    let freeze_activation = resume_source
        .find("begin_backend_swap_video_freeze(")
        .expect("successful same-lineage install must activate its checkpoint");
    assert!(terminal_finish < video_commit && video_commit < freeze_activation);
    let intent_outcome = resume_source
        .find("receipt.try_outcome()")
        .expect("playback intent receipt remains the final fallible pre-commit barrier");
    let same_lineage_rebind = resume_source
        .find("complete_same_item_media_switch(")
        .expect("same-lineage identity must rebind after successful intent");
    assert!(intent_outcome < same_lineage_rebind);
}

/// Cancel-win разрешает fallback, а missing/fatal terminal остаётся sticky fatal.
#[test]
fn fallback_classification_accepts_only_proven_pre_barrier_terminal() {
    let request_id = MediaOpenRequestId::from_non_zero(
        NonZeroU64::new(17).expect("fixture request id is non-zero"),
    );
    let cancelled = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::Cancelled {
        request_id,
        cause: MediaInstallCancellationCause::Superseded,
    });
    let fatal = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::FatalInvariant {
        request_id,
        violation: crate::media_open::MediaOpenInvariantViolation::MissingPlayerControlResolution,
    });

    assert!(cancelled.is_proven_pre_barrier_failure());
    assert_eq!(cancelled.terminal_request_id(), Some(request_id));
    assert!(!fatal.is_proven_pre_barrier_failure());
    assert_eq!(fatal.terminal_request_id(), Some(request_id));
    assert!(!StrongMediaOpenError::MissingTerminal.is_proven_pre_barrier_failure());
    let compensated = StrongMediaOpenError::PostInstalledCompensated {
        request_id,
        failure: Box::new(StrongMediaOpenError::PositionRestoreReceipt),
    };
    let cleanup_failed = StrongMediaOpenError::PostInstalledCompensationFailed {
        request_id,
        failure: Box::new(StrongMediaOpenError::PositionRestoreReceipt),
        cleanup: PostInstalledCompensationFailure::ReleaseReceipt,
    };
    assert!(compensated.allows_navigation_failure_recovery());
    assert_eq!(compensated.terminal_request_id(), Some(request_id));
    assert!(!cleanup_failed.allows_navigation_failure_recovery());
    assert!(cleanup_failed.may_have_crossed_install_barrier());
}
