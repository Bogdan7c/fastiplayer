use std::num::NonZeroU64;

use super::*;

#[test]
fn synchronous_receipt_reports_missing_owner_instead_of_waiting_forever() {
    let (receipt, port) = MediaInstallReceipt::new(test_request_id(907));
    drop(port);
    assert!(matches!(
        receipt.wait_for_signal(),
        Err(MediaInstallReceiptWaitError::MissingOwnerOutcome)
    ));
    assert!(receipt.try_take_ready_to_commit().is_none());
    assert!(receipt.try_take_completion().is_none());
}

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

fn installed_commit(
    media_instance_id: MediaInstanceId,
) -> (MediaInstanceId, AcceptedPlaybackIntent) {
    (
        media_instance_id,
        AcceptedPlaybackIntent {
            revision: PlaybackIntentRevision::INITIAL,
            intent: PlaybackIntent::StartPaused,
        },
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
            MediaInstallFailureStage::VideoPreflightTimeout,
            MediaInstallFailureStage::PlaybackWindowPreparation,
            MediaInstallFailureStage::PositionPreparation,
            MediaInstallFailureStage::CandidateVideoResourceAcquisition,
            MediaInstallFailureStage::CandidateVideoBackendMatching,
            MediaInstallFailureStage::CandidateVideoBackendConfiguration,
            MediaInstallFailureStage::CandidateVideoStatusPublication,
            MediaInstallFailureStage::LegacyMediaOpenedTransition,
        ]
    );
    let _future_commit_point = MediaInstallCommitPoint::ReplaceActiveOwnershipAndPublishInstalled;
}

#[test]
fn accepted_authorization_requires_exact_installed_terminal_as_fatal_invariant() {
    let request_id = test_request_id(80);
    let (missing_receipt, _missing_port) = MediaInstallReceipt::new(request_id);
    assert_eq!(
        missing_receipt.take_required_installed_after_authorization(),
        Err(AcceptedMediaInstallTerminalError::MissingInstalled { request_id })
    );

    let (unexpected_receipt, unexpected_port) = MediaInstallReceipt::new(request_id);
    unexpected_port.publish_terminal(MediaInstallCompletion::Cancelled {
        request_id,
        cause: MediaInstallCancellationCause::LifecycleShutdown,
    });
    assert!(matches!(
        unexpected_receipt.take_required_installed_after_authorization(),
        Err(AcceptedMediaInstallTerminalError::UnexpectedCompletion(
            MediaInstallCompletion::Cancelled { .. }
        ))
    ));

    let installed_instance_id = instance_id(81);
    let (installed_receipt, install_port) = MediaInstallReceipt::new(request_id);
    let mut protocol = MediaInstallProtocol::accept(request_id, install_port);
    protocol.mark_ready_to_commit();
    assert_eq!(
        protocol.apply_control(
            MediaInstallControl::Authorize(AuthorizeInstallCommit { request_id }),
            || installed_commit(installed_instance_id),
        ),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    assert_eq!(
        installed_receipt.take_required_installed_after_authorization(),
        Ok(installed_instance_id)
    );
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
        || installed_commit(installed_instance_id),
    );

    assert_eq!(outcome, MediaInstallControlOutcome::AuthorizationAccepted);
    assert_eq!(
        receipt.try_take_completion(),
        Some(MediaInstallCompletion::Installed {
            request_id,
            media_instance_id: installed_instance_id,
            applied_intent_revision: PlaybackIntentRevision::INITIAL,
            applied_intent: PlaybackIntent::StartPaused,
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
            || installed_commit(installed_instance_id),
        ),
        MediaInstallControlOutcome::NotReady
    );
    assert_eq!(
        protocol.apply_control(
            MediaInstallControl::Authorize(AuthorizeInstallCommit {
                request_id: stale_request_id,
            }),
            || installed_commit(installed_instance_id),
        ),
        MediaInstallControlOutcome::StaleRequest
    );

    protocol.mark_ready_to_commit();
    assert_eq!(
        protocol.apply_control(
            MediaInstallControl::Authorize(AuthorizeInstallCommit { request_id }),
            || installed_commit(installed_instance_id),
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
                || installed_commit(installed_instance_id),
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

    first_protocol.complete_failed(MediaInstallFailure::new(
        MediaInstallFailureStage::OpenTransition,
        PlayerError::new(PlayerErrorKind::DemuxError, "first"),
    ));
    second_protocol.complete_failed(MediaInstallFailure::new(
        MediaInstallFailureStage::OpenTransition,
        PlayerError::new(PlayerErrorKind::DemuxError, "second"),
    ));

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

#[test]
fn synchronous_receipt_wait_reports_missing_owner_without_timeout_success() {
    let request_id = test_request_id(905);
    let (receipt, owner_port) = MediaInstallReceipt::new(request_id);
    drop(owner_port);

    assert_eq!(
        receipt.wait_until_signal_available(),
        Err(MediaInstallReceiptWaitError::MissingOwnerOutcome)
    );
}

#[test]
fn synchronous_receipt_preserves_ready_before_already_published_terminal() {
    let request_id = test_request_id(906);
    let installed_instance_id = instance_id(906);
    let (mut protocol, receipt) = accepted_protocol(request_id);

    protocol.mark_ready_to_commit();
    assert_eq!(
        protocol.apply_control(
            MediaInstallControl::Authorize(AuthorizeInstallCommit { request_id }),
            || installed_commit(installed_instance_id),
        ),
        MediaInstallControlOutcome::AuthorizationAccepted
    );

    assert!(matches!(
        receipt
            .wait_for_signal()
            .expect("ReadyToCommit signal должен сохраниться"),
        MediaInstallReceiptSignal::ReadyToCommit(MediaInstallPhase::ReadyToCommit {
            request_id: ready_request_id,
        }) if ready_request_id == request_id
    ));
    assert!(matches!(
        receipt
            .wait_for_signal()
            .expect("Installed signal должен следовать после ReadyToCommit"),
        MediaInstallReceiptSignal::Terminal(MediaInstallCompletion::Installed {
            request_id: installed_request_id,
            media_instance_id,
            ..
        }) if installed_request_id == request_id && media_instance_id == installed_instance_id
    ));
}
