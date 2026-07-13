use std::num::NonZeroU64;

use super::*;

/// Создаёт deterministic request ID без влияния process allocator-а.
fn test_request_id(raw_identity: u64) -> MediaInstallRequestId {
    MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(raw_identity).expect("test request identity must be non-zero"),
    )
}

/// Создаёт deterministic media instance ID.
fn instance_id(raw_identity: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(
        NonZeroU64::new(raw_identity).expect("test instance identity must be non-zero"),
    )
}

/// Создаёт accepted protocol и caller receipt для focused state tests.
fn accepted_protocol(
    request_id: MediaInstallRequestId,
) -> (MediaInstallProtocol, MediaInstallReceipt) {
    let (receipt, port) = MediaInstallReceipt::new(request_id);
    (MediaInstallProtocol::accept(request_id, port), receipt)
}

#[test]
fn fallible_stage_inventory_and_future_atomic_commit_point_are_explicit() {
    assert_eq!(
        MediaInstallFailureStage::ALL,
        [
            MediaInstallFailureStage::LegacyResetSeekFloor,
            MediaInstallFailureStage::LegacyResetDecoderFlush,
            MediaInstallFailureStage::LegacyResetDecoderStream,
            MediaInstallFailureStage::OpenTransition,
            MediaInstallFailureStage::AudioTrackPlanning,
            MediaInstallFailureStage::VideoStreamConfiguration,
            MediaInstallFailureStage::LegacyMediaOpenedTransition,
        ]
    );
    let _future_commit_point = MediaInstallCommitPoint::ReplaceActiveOwnershipAndPublishInstalled;
}

#[test]
fn command_acceptance_ready_and_installed_are_distinct_phases() {
    let request_id = test_request_id(1);
    let installed_instance_id = instance_id(11);
    let (mut protocol, receipt) = accepted_protocol(request_id);

    assert_eq!(receipt.try_take_ready_to_commit(), None);
    assert_eq!(receipt.try_take_completion(), None);

    protocol.mark_ready_to_commit();

    assert_eq!(
        receipt.try_take_ready_to_commit(),
        Some(MediaInstallPhase::ReadyToCommit { request_id })
    );
    assert_eq!(receipt.try_take_completion(), None);

    let outcome = protocol.apply_control(
        MediaInstallControl::Authorize(AuthorizeInstallCommit { request_id }),
        || installed_instance_id,
    );

    assert_eq!(outcome, MediaInstallControlOutcome::AuthorizationAccepted);
    assert_eq!(
        receipt.try_take_completion(),
        Some(MediaInstallCompletion::Installed {
            request_id,
            media_instance_id: installed_instance_id,
        })
    );
}

#[test]
fn authorization_requires_matching_ready_request_and_rejects_duplicates() {
    let request_id = test_request_id(2);
    let stale_request_id = test_request_id(3);
    let installed_instance_id = instance_id(12);
    let (mut protocol, receipt) = accepted_protocol(request_id);

    assert_eq!(
        protocol.apply_control(
            MediaInstallControl::Authorize(AuthorizeInstallCommit { request_id }),
            || installed_instance_id,
        ),
        MediaInstallControlOutcome::NotReady
    );
    assert_eq!(
        protocol.apply_control(
            MediaInstallControl::Authorize(AuthorizeInstallCommit {
                request_id: stale_request_id,
            }),
            || installed_instance_id,
        ),
        MediaInstallControlOutcome::StaleRequest
    );

    protocol.mark_ready_to_commit();
    assert_eq!(
        protocol.apply_control(
            MediaInstallControl::Authorize(AuthorizeInstallCommit { request_id }),
            || installed_instance_id,
        ),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    assert_eq!(
        protocol.apply_control(
            MediaInstallControl::Authorize(AuthorizeInstallCommit { request_id }),
            || panic!("duplicate authorization must not run commit"),
        ),
        MediaInstallControlOutcome::AlreadyTerminal
    );

    assert_eq!(
        receipt
            .try_take_completion()
            .map(|completion| completion.request_id()),
        Some(request_id)
    );
}

#[test]
fn every_pre_barrier_cancellation_cause_remains_distinct() {
    let causes = [
        MediaInstallCancellationCause::UserCancelled,
        MediaInstallCancellationCause::Superseded,
        MediaInstallCancellationCause::StopAfterCurrent,
        MediaInstallCancellationCause::TransportStop,
        MediaInstallCancellationCause::StructuralInvalidation,
        MediaInstallCancellationCause::LifecycleSuspended,
        MediaInstallCancellationCause::LifecycleShutdown,
    ];

    for (index, cause) in causes.into_iter().enumerate() {
        let request_id = test_request_id(index as u64 + 10);
        let (mut protocol, receipt) = accepted_protocol(request_id);
        protocol.mark_ready_to_commit();

        let outcome = protocol.apply_control(
            MediaInstallControl::Cancel(CancelMediaInstall { request_id, cause }),
            || panic!("cancellation must not commit an instance"),
        );

        assert_eq!(outcome, MediaInstallControlOutcome::CancellationAccepted);
        assert_eq!(
            receipt.try_take_completion(),
            Some(MediaInstallCompletion::Cancelled { request_id, cause })
        );
    }
}

#[test]
fn authorize_and_each_cancel_cause_follow_ordered_winner() {
    let causes = [
        MediaInstallCancellationCause::UserCancelled,
        MediaInstallCancellationCause::Superseded,
        MediaInstallCancellationCause::StopAfterCurrent,
        MediaInstallCancellationCause::TransportStop,
        MediaInstallCancellationCause::StructuralInvalidation,
        MediaInstallCancellationCause::LifecycleSuspended,
        MediaInstallCancellationCause::LifecycleShutdown,
    ];

    for (index, cause) in causes.into_iter().enumerate() {
        let cancel_first_request_id = test_request_id(index as u64 + 30);
        let (mut cancel_first, cancel_first_receipt) = accepted_protocol(cancel_first_request_id);
        cancel_first.mark_ready_to_commit();
        assert_eq!(
            cancel_first.apply_control(
                MediaInstallControl::Cancel(CancelMediaInstall {
                    request_id: cancel_first_request_id,
                    cause,
                }),
                || panic!("cancel-first ordering must not commit"),
            ),
            MediaInstallControlOutcome::CancellationAccepted
        );
        assert_eq!(
            cancel_first.apply_control(
                MediaInstallControl::Authorize(AuthorizeInstallCommit {
                    request_id: cancel_first_request_id,
                }),
                || panic!("late authorization must not commit"),
            ),
            MediaInstallControlOutcome::AlreadyTerminal
        );
        assert!(matches!(
            cancel_first_receipt.try_take_completion(),
            Some(MediaInstallCompletion::Cancelled {
                cause: terminal_cause,
                ..
            }) if terminal_cause == cause
        ));

        let authorize_first_request_id = test_request_id(index as u64 + 50);
        let installed_instance_id = instance_id(index as u64 + 70);
        let (mut authorize_first, authorize_first_receipt) =
            accepted_protocol(authorize_first_request_id);
        authorize_first.mark_ready_to_commit();
        assert_eq!(
            authorize_first.apply_control(
                MediaInstallControl::Authorize(AuthorizeInstallCommit {
                    request_id: authorize_first_request_id,
                }),
                || installed_instance_id,
            ),
            MediaInstallControlOutcome::AuthorizationAccepted
        );
        assert_eq!(
            authorize_first.apply_control(
                MediaInstallControl::Cancel(CancelMediaInstall {
                    request_id: authorize_first_request_id,
                    cause,
                }),
                || panic!("late cancellation must not re-run commit"),
            ),
            MediaInstallControlOutcome::AlreadyTerminal
        );
        assert!(matches!(
            authorize_first_receipt.try_take_completion(),
            Some(MediaInstallCompletion::Installed {
                media_instance_id,
                ..
            }) if media_instance_id == installed_instance_id
        ));
    }
}

#[test]
fn terminal_slot_is_lossless_and_drains_exactly_once() {
    let request_id = test_request_id(90);
    let failure = MediaInstallFailure::new(
        MediaInstallFailureStage::VideoStreamConfiguration,
        PlayerError::new(PlayerErrorKind::UnsupportedVideoCodec, "test rejection"),
    );
    let (mut protocol, receipt) = accepted_protocol(request_id);

    protocol.complete_failed(failure.clone());

    assert_eq!(
        receipt.try_take_completion(),
        Some(MediaInstallCompletion::Failed {
            request_id,
            failure,
        })
    );
    assert_eq!(receipt.try_take_completion(), None);
}

#[test]
fn receipts_keep_request_and_completion_correlation_separate() {
    let first_request_id = test_request_id(91);
    let second_request_id = test_request_id(92);
    let (mut first_protocol, first_receipt) = accepted_protocol(first_request_id);
    let (mut second_protocol, second_receipt) = accepted_protocol(second_request_id);

    first_protocol.complete_failed(MediaInstallFailure::legacy_open_rejected("first"));
    second_protocol.complete_failed(MediaInstallFailure::legacy_open_rejected("second"));

    assert_eq!(first_receipt.request_id(), first_request_id);
    assert_eq!(second_receipt.request_id(), second_request_id);
    assert_eq!(
        first_receipt
            .try_take_completion()
            .expect("first completion")
            .request_id(),
        first_request_id
    );
    assert_eq!(
        second_receipt
            .try_take_completion()
            .expect("second completion")
            .request_id(),
        second_request_id
    );
}
