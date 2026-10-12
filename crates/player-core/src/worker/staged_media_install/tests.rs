use std::num::NonZeroU64;
use std::time::{Duration, Instant};

use crossbeam_channel::bounded;
use media_core::{DemuxSeekResult, DemuxSeekability, Demuxer, TimelineNotSeekableReason, TrackId};
use video_backend_api::{
    DetachedVideoBackendCandidateCancellationCause, DetachedVideoBackendCandidateStatus,
    DetachedVideoBackendPortError, DetachedVideoBackendReply, DetachedVideoBackendRequest,
    DetachedVideoBackendResourceError, DetachedVideoBackendResourcePort,
};

use super::{MediaInstallControlReceipt, MediaInstallControlReceiptError};
use crate::worker::{COMMAND_CHANNEL_CAPACITY, PlayerCommandSender, WorkerCommand};
use crate::{
    AuthorizeInstallCommit, CancelMediaInstall, InstalledMediaRelease,
    InstalledMediaReleaseOutcome, InstalledMediaStateRestore, InstalledMediaStateRestoreOutcome,
    InstalledPositionRestore, InstalledSubtitleRestore, InstalledTrackRestore,
    MediaInstallCancellationCause, MediaInstallCompletion, MediaInstallControlOutcome,
    MediaInstallReceiptSignal, MediaInstallRequestId, MediaInstallVideoResourcePort,
    MediaInstanceId, PlaybackIntent, PlaybackIntentRevision, PlaybackIntentUpdate,
    PlaybackIntentUpdateOutcome, PlaybackState, PlayerCommand, PlayerWorker, PlayerWorkerConfig,
    PlayerWorkerSendError, PreparedMedia,
};

#[path = "tests/snapshot_publication.rs"]
mod snapshot_publication;

/// Empty prepared demuxer для no-audio/no-video worker transaction.
struct EmptyDemuxer;

impl Demuxer for EmptyDemuxer {
    /// Empty media не публикует tracks.
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &[]
    }

    /// Duration известна, чтобы snapshot commit был observable.
    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(5))
    }

    /// Fake source остаётся seekable, хотя seek в тесте не вызывается.
    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    /// Empty media сразу сообщает EOF packets.
    fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
        Ok(media_core::DemuxReadEvent::EndOfStream)
    }

    /// Seek возвращает exact target для focused restore boundary test-а.
    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        Ok(DemuxSeekResult {
            requested_position: timestamp.into(),
            actual_position: timestamp.into(),
            actual_track_timestamp: None,
        })
    }
}

/// Live-like fake доказывает typed position-unavailable outcome без fake seek success.
struct UnseekableDemuxer;

impl Demuxer for UnseekableDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::SourceNotSeekable,
        }
    }

    fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
        Ok(media_core::DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        anyhow::bail!("unseekable fake must never receive seek")
    }
}

/// Port обязан остаться неиспользованным для media без video.
struct NoVideoResourcePort;

impl DetachedVideoBackendResourcePort for NoVideoResourcePort {
    type RequestId = MediaInstallRequestId;

    /// Defensive reply делает ошибочный video request observable без panic.
    fn request_detached_backend(
        &mut self,
        request: DetachedVideoBackendRequest<Self::RequestId>,
    ) -> Result<DetachedVideoBackendReply<Self::RequestId>, DetachedVideoBackendPortError> {
        Ok(DetachedVideoBackendReply::unavailable(
            *request.request_id(),
            DetachedVideoBackendResourceError::Unavailable {
                reason: "no-video port must not be requested".to_owned(),
            },
        ))
    }

    /// No-video transaction не публикует candidate status.
    fn publish_candidate_status(
        &mut self,
        _status: DetachedVideoBackendCandidateStatus<Self::RequestId>,
    ) -> Result<(), DetachedVideoBackendPortError> {
        Ok(())
    }

    /// No-video transaction не имеет player decoder half-а для cancel.
    fn cancel_candidate(
        &mut self,
        _request_id: Self::RequestId,
        _cause: DetachedVideoBackendCandidateCancellationCause,
    ) -> Result<(), DetachedVideoBackendPortError> {
        Ok(())
    }
}

/// Ждёт owner-thread result с коротким deterministic deadline.
fn wait_until<T>(mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(value) = poll() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "player worker published staged install outcome before deadline"
        );
        std::thread::yield_now();
    }
}

/// Создаёт deterministic non-zero D52 revision.
fn intent_revision(raw: u64) -> PlaybackIntentRevision {
    PlaybackIntentRevision::from_non_zero(
        NonZeroU64::new(raw).expect("test playback intent revision is non-zero"),
    )
}

#[test]
fn public_worker_api_separates_enqueue_ready_authorization_and_installed() {
    let mut worker =
        PlayerWorker::spawn(PlayerWorkerConfig::default()).expect("Player worker should start up");
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(700).expect("test request id is non-zero"),
    );
    let prepared_media =
        PreparedMedia::from_external_label("worker candidate", Box::new(EmptyDemuxer));
    let receipt = worker
        .stage_prepared_media_install(
            request_id,
            prepared_media,
            PlaybackIntent::StartPaused,
            PlaybackIntentRevision::INITIAL,
            MediaInstallVideoResourcePort::any_playable(NoVideoResourcePort),
        )
        .expect("Staged command should be included in the empty worker queue.");

    wait_until(|| receipt.try_take_ready_to_commit());
    assert!(receipt.try_take_completion().is_none());
    let control_receipt = worker
        .authorize_install_commit(AuthorizeInstallCommit { request_id })
        .expect("Permission must be included in the worker queue");
    let control_outcome = wait_until(|| match control_receipt.try_take_outcome() {
        Ok(outcome) => outcome,
        Err(error) => panic!("accepted authorization lost owner outcome: {error}"),
    });
    assert_eq!(
        control_outcome,
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    assert!(matches!(
        receipt.try_take_completion(),
        Some(MediaInstallCompletion::Installed {
            request_id: completion_request_id,
            ..
        }) if completion_request_id == request_id
    ));
    worker
        .shutdown()
        .expect("Worker shutdown must be completed");
}

#[test]
fn exact_restore_rejects_precommit_and_stale_instance_then_applies_after_installed() {
    let mut worker =
        PlayerWorker::spawn(PlayerWorkerConfig::default()).expect("Player worker should start up");
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(760).expect("test request id is non-zero"),
    );
    let placeholder_instance =
        MediaInstanceId::from_non_zero(NonZeroU64::new(760).expect("test instance id is non-zero"));
    let receipt = worker
        .stage_prepared_media_install(
            request_id,
            PreparedMedia::from_external_label("restore candidate", Box::new(EmptyDemuxer)),
            PlaybackIntent::StartPaused,
            PlaybackIntentRevision::INITIAL,
            MediaInstallVideoResourcePort::any_playable(NoVideoResourcePort),
        )
        .expect("Stage command must be accepted.");
    wait_until(|| receipt.try_take_ready_to_commit());

    let precommit_restore = worker
        .restore_installed_media_state(InstalledMediaStateRestore {
            request_id,
            media_instance_id: placeholder_instance,
            video_track: InstalledTrackRestore::KeepDefault,
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::KeepDefault,
            volume: crate::InstalledVolumeRestore::KeepCurrent,
            position: InstalledPositionRestore::KeepStart,
        })
        .expect("Recovery transport must take command.");
    assert_eq!(
        precommit_restore
            .wait_for_outcome()
            .expect("precommitt restore must have the owner’s outcome"),
        InstalledMediaStateRestoreOutcome::NotInstalledYet
    );

    let authorization = worker
        .authorize_install_commit(AuthorizeInstallCommit { request_id })
        .expect("Authorization must be enqueued");
    assert_eq!(
        authorization
            .wait_for_outcome()
            .expect("Owner’s Permission Outcome is Mandatory"),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    let installed_instance = match receipt
        .wait_for_signal()
        .expect("Installed terminal is required after authorization")
    {
        MediaInstallReceiptSignal::Terminal(MediaInstallCompletion::Installed {
            media_instance_id,
            ..
        }) => media_instance_id,
        unexpected => panic!("Installed terminal received {unexpected:?}"),
    };

    let exact_restore = worker
        .restore_installed_media_state(InstalledMediaStateRestore {
            request_id,
            media_instance_id: installed_instance,
            video_track: InstalledTrackRestore::KeepDefault,
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::Select(TrackId::new(99)),
            volume: crate::InstalledVolumeRestore::Set(0.37),
            position: InstalledPositionRestore::SeekTo(Duration::from_secs(2)),
        })
        .expect("exact restore transport must take command.");
    assert_eq!(
        exact_restore
            .wait_for_outcome()
            .expect("Exactly restore owner outcome is mandatory"),
        InstalledMediaStateRestoreOutcome::Applied {
            media_instance_id: installed_instance,
        }
    );
    wait_until(|| {
        (worker
            .latest_snapshot(crate::FrameCounters::default())
            .volume
            - 0.37)
            .abs()
            .lt(&f32::EPSILON)
            .then_some(())
    });

    let failed_volume_restore = worker
        .restore_installed_media_state(InstalledMediaStateRestore {
            request_id,
            media_instance_id: installed_instance,
            video_track: InstalledTrackRestore::KeepDefault,
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::KeepDefault,
            volume: crate::InstalledVolumeRestore::Set(f32::NAN),
            position: InstalledPositionRestore::KeepStart,
        })
        .expect("Volume Recovery Transportation to Take Command");
    assert!(matches!(
        failed_volume_restore
            .wait_for_outcome()
            .expect("Volume failure must be an authoritative owner resulting"),
        InstalledMediaStateRestoreOutcome::Failed {
            stage: crate::InstalledMediaRestoreFailureStage::Volume,
            ..
        }
    ));

    let failed_track_restore = worker
        .restore_installed_media_state(InstalledMediaStateRestore {
            request_id,
            media_instance_id: installed_instance,
            video_track: InstalledTrackRestore::Select(TrackId::new(100)),
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::KeepDefault,
            volume: crate::InstalledVolumeRestore::KeepCurrent,
            position: InstalledPositionRestore::KeepStart,
        })
        .expect("Track Recovery Transportation to Take Command");
    assert!(matches!(
        failed_track_restore
            .wait_for_outcome()
            .expect("track restore failure must be an authoritative owner outcome"),
        InstalledMediaStateRestoreOutcome::Failed {
            stage: crate::InstalledMediaRestoreFailureStage::VideoTrack,
            ..
        }
    ));

    let stale_release = worker
        .release_installed_media(InstalledMediaRelease {
            request_id,
            media_instance_id: placeholder_instance,
        })
        .expect("Stale exact release transport must be accepted");
    assert_eq!(
        stale_release
            .wait_for_outcome()
            .expect("The stale release must have the owner’s outcome."),
        InstalledMediaReleaseOutcome::StaleInstance
    );

    let exact_release = worker
        .release_installed_media(InstalledMediaRelease {
            request_id,
            media_instance_id: installed_instance,
        })
        .expect("The exact release transport must be accepted.");
    assert_eq!(
        exact_release
            .wait_for_outcome()
            .expect("Exact release of owner outcome is mandatory"),
        InstalledMediaReleaseOutcome::Applied {
            media_instance_id: installed_instance,
        }
    );
    wait_until(|| {
        worker
            .latest_snapshot(crate::FrameCounters::default())
            .media_instance_id
            .is_none()
            .then_some(())
    });

    let newer_request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(761).expect("newer test request id is non-zero"),
    );
    let newer_receipt = worker
        .stage_prepared_media_install(
            newer_request_id,
            PreparedMedia::from_external_label("newer candidate", Box::new(EmptyDemuxer)),
            PlaybackIntent::StartPaused,
            PlaybackIntentRevision::INITIAL,
            MediaInstallVideoResourcePort::any_playable(NoVideoResourcePort),
        )
        .expect("Newer stage command must be accepted.");
    assert!(matches!(
        newer_receipt
            .wait_for_signal()
            .expect("The newer candidate must reach ReadyToCommittee."),
        MediaInstallReceiptSignal::ReadyToCommit(_)
    ));
    let newer_authorization = worker
        .authorize_install_commit(AuthorizeInstallCommit {
            request_id: newer_request_id,
        })
        .expect("Newer authorization must be enqueued");
    assert_eq!(
        newer_authorization
            .wait_for_outcome()
            .expect("Newer Authorization Owner Outcome Required"),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    assert!(matches!(
        newer_receipt
            .wait_for_signal()
            .expect("Newer Installed Terminal is a must"),
        MediaInstallReceiptSignal::Terminal(MediaInstallCompletion::Installed {
            request_id: completion_request_id,
            ..
        }) if completion_request_id == newer_request_id
    ));

    let stale_restore = worker
        .restore_installed_media_state(InstalledMediaStateRestore {
            request_id,
            media_instance_id: installed_instance,
            video_track: InstalledTrackRestore::KeepDefault,
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::KeepDefault,
            volume: crate::InstalledVolumeRestore::KeepCurrent,
            position: InstalledPositionRestore::KeepStart,
        })
        .expect("Stale restore transport can still be accepted");
    assert_eq!(
        stale_restore
            .wait_for_outcome()
            .expect("Stale Recovery: Typed Owner Results"),
        InstalledMediaStateRestoreOutcome::StaleInstance
    );

    worker
        .shutdown()
        .expect("Worker shutdown must be completed");
}

#[test]
fn exact_restore_reports_typed_position_unavailable_for_non_seekable_source() {
    let mut worker =
        PlayerWorker::spawn(PlayerWorkerConfig::default()).expect("Player worker should start up");
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(762).expect("test request id is non-zero"),
    );
    let receipt = worker
        .stage_prepared_media_install(
            request_id,
            PreparedMedia::from_external_label("live candidate", Box::new(UnseekableDemuxer)),
            PlaybackIntent::StartPaused,
            PlaybackIntentRevision::INITIAL,
            MediaInstallVideoResourcePort::any_playable(NoVideoResourcePort),
        )
        .expect("Stage command must be accepted.");
    wait_until(|| receipt.try_take_ready_to_commit());
    let authorization = worker
        .authorize_install_commit(AuthorizeInstallCommit { request_id })
        .expect("Permission must be accepted");
    assert_eq!(
        authorization
            .wait_for_outcome()
            .expect("Owner’s Permission Outcome is Mandatory"),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    let installed_instance = match receipt
        .wait_for_signal()
        .expect("Installed terminal is required")
    {
        MediaInstallReceiptSignal::Terminal(MediaInstallCompletion::Installed {
            media_instance_id,
            ..
        }) => media_instance_id,
        unexpected => panic!("Installed terminal received {unexpected:?}"),
    };
    let installed_snapshot = wait_until(|| {
        let snapshot = worker.latest_snapshot(crate::FrameCounters::default());
        (snapshot.media_instance_id == Some(installed_instance)).then_some(snapshot)
    });
    assert_eq!(installed_snapshot.playback_state, PlaybackState::Paused);
    let requested_position = Duration::from_secs(40);
    let restore = worker
        .restore_installed_media_state(InstalledMediaStateRestore {
            request_id,
            media_instance_id: installed_instance,
            video_track: InstalledTrackRestore::KeepDefault,
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::KeepDefault,
            volume: crate::InstalledVolumeRestore::KeepCurrent,
            position: InstalledPositionRestore::SeekTo(requested_position),
        })
        .expect("Restore command must be accepted.");
    let restore_outcome = restore
        .wait_for_outcome()
        .expect("position unavailable outcome is mandatory");
    assert!(
        matches!(
            restore_outcome,
            InstalledMediaStateRestoreOutcome::PositionUnavailable {
                media_instance_id,
                requested_position: requested,
                reason: crate::InstalledPositionUnavailableReason::SourceNotSeekable,
                ..
            } if media_instance_id == installed_instance && requested == requested_position
        ),
        "unexpected restore outcome: {restore_outcome:?}"
    );
    assert_eq!(
        worker
            .latest_snapshot(crate::FrameCounters::default())
            .playback_state,
        PlaybackState::Paused,
        "non-seekable outcome can't turn on for a short time Playing before post-seek intent"
    );
    worker.shutdown().expect("worker shutdown");
}

#[test]
fn intent_update_before_ready_ignores_full_ordinary_queue() {
    let (command_tx, _command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    let (sender, _intent_wake_rx) = PlayerCommandSender::for_tests(command_tx.clone());
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(704).expect("test request id is non-zero"),
    );
    let receipt = sender
        .stage_prepared_media_install(
            request_id,
            PreparedMedia::from_external_label("pre-ready intent", Box::new(EmptyDemuxer)),
            PlaybackIntent::StartPlaying,
            intent_revision(1),
            MediaInstallVideoResourcePort::any_playable(NoVideoResourcePort),
        )
        .expect("Stage command takes the first ordinary queue slot.");

    for _ in 1..COMMAND_CHANNEL_CAPACITY {
        command_tx
            .try_send(WorkerCommand::Player(PlayerCommand::Pause))
            .expect("The test must be filled with ordinary command queue.");
    }

    let update_receipt = sender
        .update_playback_intent(PlaybackIntentUpdate {
            request_id,
            revision: intent_revision(2),
            intent: PlaybackIntent::StartPaused,
        })
        .expect("D52 Update does not depend on full ordinary queue");

    assert_eq!(
        update_receipt.try_outcome(),
        Some(PlaybackIntentUpdateOutcome::AppliedToStaged)
    );
    assert!(receipt.try_take_ready_to_commit().is_none());
}

#[test]
fn staged_pause_updates_old_current_and_cancel_preserves_latest_state() {
    let mut worker =
        PlayerWorker::spawn(PlayerWorkerConfig::default()).expect("Player worker should start up");
    let old_receipt = worker
        .load_prepared_media(
            PreparedMedia::from_external_label("old current", Box::new(EmptyDemuxer)),
            false,
        )
        .expect("Old compatibility install must be accepted");
    wait_until(|| old_receipt.try_take_completion());
    wait_until(|| {
        let snapshot = worker.latest_snapshot(crate::FrameCounters::default());
        (snapshot.playback_state == PlaybackState::Paused).then_some(())
    });

    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(707).expect("test request id is non-zero"),
    );
    let candidate_receipt = worker
        .stage_prepared_media_install(
            request_id,
            PreparedMedia::from_external_label("candidate", Box::new(EmptyDemuxer)),
            PlaybackIntent::StartPlaying,
            intent_revision(1),
            MediaInstallVideoResourcePort::any_playable(NoVideoResourcePort),
        )
        .expect("The candidate stage must be accepted.");
    wait_until(|| candidate_receipt.try_take_ready_to_commit());

    let pause_receipt = worker
        .update_playback_intent(PlaybackIntentUpdate {
            request_id,
            revision: intent_revision(2),
            intent: PlaybackIntent::StartPaused,
        })
        .expect("staged pause must be accepted");
    assert_eq!(
        pause_receipt.try_outcome(),
        Some(PlaybackIntentUpdateOutcome::AppliedToStaged)
    );
    wait_until(|| {
        let snapshot = worker.latest_snapshot(crate::FrameCounters::default());
        (snapshot.playback_state == PlaybackState::Paused).then_some(())
    });

    let cancel_receipt = worker
        .cancel_media_install(CancelMediaInstall {
            request_id,
            cause: MediaInstallCancellationCause::UserCancelled,
        })
        .expect("Pre-barrier cancel must be accepted.");
    assert_eq!(
        wait_until(|| match cancel_receipt.try_take_outcome() {
            Ok(outcome) => outcome,
            Err(error) => panic!("cancel lost owner outcome: {error}"),
        }),
        MediaInstallControlOutcome::CancellationAccepted
    );
    assert_eq!(
        worker
            .latest_snapshot(crate::FrameCounters::default())
            .playback_state,
        PlaybackState::Paused
    );

    worker
        .shutdown()
        .expect("Worker shutdown must be completed");
}

#[test]
fn highest_intent_commits_without_wrong_state_and_post_barrier_update_is_exact() {
    let mut worker =
        PlayerWorker::spawn(PlayerWorkerConfig::default()).expect("Player worker should start up");
    let first_request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(705).expect("test request id is non-zero"),
    );
    let first_receipt = worker
        .stage_prepared_media_install(
            first_request_id,
            PreparedMedia::from_external_label(
                "intent-linearization-first",
                Box::new(EmptyDemuxer),
            ),
            PlaybackIntent::StartPlaying,
            intent_revision(1),
            MediaInstallVideoResourcePort::any_playable(NoVideoResourcePort),
        )
        .expect("The first staged request must be accepted");
    wait_until(|| first_receipt.try_take_ready_to_commit());

    let staged_update = worker
        .update_playback_intent(PlaybackIntentUpdate {
            request_id: first_request_id,
            revision: intent_revision(3),
            intent: PlaybackIntent::StartPaused,
        })
        .expect("Pre-authorization update must be accepted");
    assert_eq!(
        staged_update.try_outcome(),
        Some(PlaybackIntentUpdateOutcome::AppliedToStaged)
    );

    let authorization = worker
        .authorize_install_commit(AuthorizeInstallCommit {
            request_id: first_request_id,
        })
        .expect("Authorization must be included in ordinary queue.");
    let authorization_outcome = wait_until(|| match authorization.try_take_outcome() {
        Ok(outcome) => outcome,
        Err(error) => panic!("Authorization Loses Owner Outcome: {error}"),
    });
    assert_eq!(
        authorization_outcome,
        MediaInstallControlOutcome::AuthorizationAccepted
    );

    let paused_snapshot = wait_until(|| {
        let snapshot = worker.latest_snapshot(crate::FrameCounters::default());
        (snapshot.playback_state == PlaybackState::Paused && snapshot.media_instance_id.is_some())
            .then(|| snapshot.clone())
    });
    let first_instance_id = paused_snapshot
        .media_instance_id
        .expect("Restoration of installed snapshot must be exact instance");
    assert_eq!(paused_snapshot.playback_state, PlaybackState::Paused);

    let post_barrier_update = worker
        .update_playback_intent(PlaybackIntentUpdate {
            request_id: first_request_id,
            revision: intent_revision(4),
            intent: PlaybackIntent::StartPlaying,
        })
        .expect("Post-barrier exact update should use a separate control path");
    assert_eq!(
        wait_until(|| post_barrier_update.try_outcome()),
        PlaybackIntentUpdateOutcome::AppliedToInstalled {
            media_instance_id: first_instance_id,
        }
    );

    let installed_completion = first_receipt
        .try_take_completion()
        .expect("Installed terminal still has to wait for explicit drain");
    let MediaInstallCompletion::Installed {
        media_instance_id: completed_instance_id,
        applied_intent_revision,
        applied_intent,
        ..
    } = installed_completion
    else {
        panic!("Authorization must be completed by Installed");
    };
    assert_eq!(completed_instance_id, first_instance_id);
    assert_eq!(applied_intent_revision, intent_revision(3));
    assert_eq!(applied_intent, PlaybackIntent::StartPaused);

    let second_request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(706).expect("test request id is non-zero"),
    );
    let second_receipt = worker
        .stage_prepared_media_install(
            second_request_id,
            PreparedMedia::from_external_label(
                "intent-linearization-second",
                Box::new(EmptyDemuxer),
            ),
            PlaybackIntent::StartPaused,
            intent_revision(1),
            MediaInstallVideoResourcePort::any_playable(NoVideoResourcePort),
        )
        .expect("Second staged request must be accepted");
    wait_until(|| second_receipt.try_take_ready_to_commit());
    let second_authorization = worker
        .authorize_install_commit(AuthorizeInstallCommit {
            request_id: second_request_id,
        })
        .expect("Second Authorization must be accepted");
    wait_until(|| match second_authorization.try_take_outcome() {
        Ok(outcome) => outcome,
        Err(error) => panic!("Second Authorization Loses Outcome: {error}"),
    });

    let stale_update = worker
        .update_playback_intent(PlaybackIntentUpdate {
            request_id: first_request_id,
            revision: intent_revision(5),
            intent: PlaybackIntent::StartPaused,
        })
        .expect("stale exact update gets typed owner result");
    assert_eq!(
        stale_update.try_outcome(),
        Some(PlaybackIntentUpdateOutcome::StaleInstance)
    );

    worker
        .shutdown()
        .expect("Worker shutdown must be completed");
}

#[test]
fn authorization_queue_backpressure_returns_command_without_false_acceptance() {
    let (command_tx, _command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    let sender = PlayerCommandSender::for_tests(command_tx.clone()).0;
    for _ in 0..COMMAND_CHANNEL_CAPACITY {
        command_tx
            .try_send(WorkerCommand::Player(PlayerCommand::Pause))
            .expect("Test queue must accept fill command");
    }
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(701).expect("test request id is non-zero"),
    );

    let error = sender
        .authorize_install_commit(AuthorizeInstallCommit { request_id })
        .expect_err("full queue must be rejected authorization delivery");
    assert_eq!(error, PlayerWorkerSendError::Full);
}

#[test]
fn authorization_disconnect_is_transport_failure_not_install_rejection() {
    let (command_tx, command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
    drop(command_rx);
    let sender = PlayerCommandSender::for_tests(command_tx).0;
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(703).expect("test request id is non-zero"),
    );

    let error = sender
        .authorize_install_commit(AuthorizeInstallCommit { request_id })
        .expect_err("disconnected queue cannot accept authorization");
    assert_eq!(error, PlayerWorkerSendError::Disconnected);
}

#[test]
fn dropped_owner_outcome_sender_is_fatal_missing_outcome_not_recoverable_reject() {
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(702).expect("test request id is non-zero"),
    );
    let (receipt, outcome_tx) = MediaInstallControlReceipt::new(request_id);
    drop(outcome_tx);

    assert_eq!(
        receipt.try_take_outcome(),
        Err(MediaInstallControlReceiptError::MissingOwnerOutcome)
    );
}
