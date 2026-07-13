use std::num::NonZeroU64;

use super::*;

fn request(raw: u64) -> MediaInstallRequestId {
    MediaInstallRequestId::from_non_zero(NonZeroU64::new(raw).unwrap())
}

fn instance(raw: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(NonZeroU64::new(raw).unwrap())
}

fn revision(raw: u64) -> PlaybackIntentRevision {
    PlaybackIntentRevision::from_non_zero(NonZeroU64::new(raw).unwrap())
}

fn accepted(raw: u64, intent: PlaybackIntent) -> AcceptedPlaybackIntent {
    AcceptedPlaybackIntent {
        revision: revision(raw),
        intent,
    }
}

#[test]
fn staged_updates_are_latest_only_stale_and_idempotent() {
    let control = PlaybackIntentControl::default();
    let request_id = request(1);
    control.register_staged_request(request_id, accepted(1, PlaybackIntent::StartPlaying));

    let latest = control.submit_update(PlaybackIntentUpdate {
        request_id,
        revision: revision(3),
        intent: PlaybackIntent::StartPaused,
    });
    assert_eq!(
        latest.receipt.try_outcome(),
        Some(PlaybackIntentUpdateOutcome::AppliedToStaged)
    );

    let stale = control.submit_update(PlaybackIntentUpdate {
        request_id,
        revision: revision(2),
        intent: PlaybackIntent::StartPlaying,
    });
    assert_eq!(
        stale.receipt.try_outcome(),
        Some(PlaybackIntentUpdateOutcome::StaleRevision {
            latest_revision: revision(3),
        })
    );

    let idempotent = control.submit_update(PlaybackIntentUpdate {
        request_id,
        revision: revision(3),
        intent: PlaybackIntent::StartPaused,
    });
    assert_eq!(
        idempotent.receipt.try_outcome(),
        Some(PlaybackIntentUpdateOutcome::AppliedToStaged)
    );
}

#[test]
fn commit_moves_highest_staged_intent_to_exact_installed_instance() {
    let control = PlaybackIntentControl::default();
    let request_id = request(1);
    let media_instance_id = instance(10);
    control.register_staged_request(request_id, accepted(1, PlaybackIntent::StartPlaying));
    control.submit_update(PlaybackIntentUpdate {
        request_id,
        revision: revision(4),
        intent: PlaybackIntent::StartPaused,
    });

    let mut committed_inside_owner_turn = None;
    let committed = control.commit_staged_request(request_id, media_instance_id, |accepted| {
        committed_inside_owner_turn = Some(accepted);
    });

    assert_eq!(committed, accepted(4, PlaybackIntent::StartPaused));
    assert_eq!(committed_inside_owner_turn, Some(committed));
}

#[test]
fn installed_update_is_exact_and_becomes_stale_after_new_commit() {
    let control = PlaybackIntentControl::default();
    let first_request = request(1);
    control.register_staged_request(first_request, accepted(1, PlaybackIntent::StartPaused));
    control.commit_staged_request(first_request, instance(10), |_| {});

    let installed = control.submit_update(PlaybackIntentUpdate {
        request_id: first_request,
        revision: revision(2),
        intent: PlaybackIntent::StartPlaying,
    });
    assert!(installed.wake_player_owner);
    let pending = control.take_pending_installed_update().unwrap();
    control.finish_installed_update(pending, true);
    assert_eq!(
        installed.receipt.try_outcome(),
        Some(PlaybackIntentUpdateOutcome::AppliedToInstalled {
            media_instance_id: instance(10),
        })
    );

    let second_request = request(2);
    control.register_staged_request(second_request, accepted(1, PlaybackIntent::StartPaused));
    control.commit_staged_request(second_request, instance(11), |_| {});

    let stale = control.submit_update(PlaybackIntentUpdate {
        request_id: first_request,
        revision: revision(3),
        intent: PlaybackIntent::StartPaused,
    });
    assert_eq!(
        stale.receipt.try_outcome(),
        Some(PlaybackIntentUpdateOutcome::StaleInstance)
    );
}

#[test]
fn cancel_forgets_staged_request_without_touching_current_instance() {
    let control = PlaybackIntentControl::default();
    let current_request = request(1);
    control.register_staged_request(current_request, accepted(1, PlaybackIntent::StartPlaying));
    control.commit_staged_request(current_request, instance(10), |_| {});

    let cancelled_request = request(2);
    control.register_staged_request(cancelled_request, accepted(1, PlaybackIntent::StartPaused));
    control.forget_staged_request(cancelled_request);

    let cancelled = control.submit_update(PlaybackIntentUpdate {
        request_id: cancelled_request,
        revision: revision(2),
        intent: PlaybackIntent::StartPlaying,
    });
    assert_eq!(
        cancelled.receipt.try_outcome(),
        Some(PlaybackIntentUpdateOutcome::UnknownOrSupersededRequest)
    );

    let current = control.submit_update(PlaybackIntentUpdate {
        request_id: current_request,
        revision: revision(2),
        intent: PlaybackIntent::StartPaused,
    });
    assert!(current.wake_player_owner);
}

#[test]
fn staged_update_targets_exact_old_current_instance_until_commit() {
    let control = PlaybackIntentControl::default();
    let current_request = request(1);
    control.register_staged_request(current_request, accepted(1, PlaybackIntent::StartPlaying));
    control.commit_staged_request(current_request, instance(10), |_| {});

    let candidate_request = request(2);
    control.register_staged_request(candidate_request, accepted(1, PlaybackIntent::StartPlaying));
    let submitted = control.submit_update(PlaybackIntentUpdate {
        request_id: candidate_request,
        revision: revision(2),
        intent: PlaybackIntent::StartPaused,
    });

    assert!(submitted.wake_player_owner);
    assert_eq!(
        submitted.receipt.try_outcome(),
        Some(PlaybackIntentUpdateOutcome::AppliedToStaged)
    );
    let pending = control
        .take_pending_current_for_staged()
        .expect("staged update обязан адресовать old current exact instance");
    assert_eq!(pending.request_id, candidate_request);
    assert_eq!(pending.media_instance_id, instance(10));
    assert_eq!(pending.intent, PlaybackIntent::StartPaused);
}

#[test]
fn newer_sender_registration_supersedes_queued_old_request_and_rollback_restores_previous() {
    let control = PlaybackIntentControl::default();
    let first_request = request(1);
    control.register_staged_request(first_request, accepted(1, PlaybackIntent::StartPlaying));

    let rejected_registration =
        control.begin_staged_registration(request(2), accepted(1, PlaybackIntent::StartPaused));
    assert!(!control.staged_request_is_latest(first_request));
    control.rollback_staged_registration(rejected_registration);
    assert!(control.staged_request_is_latest(first_request));

    let latest_request = request(3);
    let _accepted_registration = control
        .begin_staged_registration(latest_request, accepted(1, PlaybackIntent::StartPlaying));
    assert!(!control.staged_request_is_latest(first_request));
    assert!(control.staged_request_is_latest(latest_request));
}
