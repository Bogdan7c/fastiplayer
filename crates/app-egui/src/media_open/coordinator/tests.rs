use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use media_core::{DemuxSeekResult, Demuxer};
use playlist_discovery::{LocalMediaFingerprint, LocalMediaKind};
use video_backend_api::{
    DetachedVideoBackendCandidateCancellationCause, DetachedVideoBackendCandidateStatus,
    DetachedVideoBackendPortError, DetachedVideoBackendReply, DetachedVideoBackendRequest,
    DetachedVideoBackendResourcePort,
};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::media_open::{
    ActiveMediaSource, MAX_NON_CANCELLABLE_STALE_PREPARATIONS, PlayerDispatchRejection,
    PreparedWebMediaEnvelope, SafeMediaLabel, WebMediaSourceIntent,
};

mod same_lineage_tests;

#[derive(Default)]
struct FakeDemuxer;

impl Demuxer for FakeDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &[]
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
        Ok(media_core::DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        panic!("fake demuxer seek is outside media-open tests")
    }
}

struct UnusedVideoResourcePort;

impl DetachedVideoBackendResourcePort for UnusedVideoResourcePort {
    type RequestId = MediaInstallRequestId;

    fn request_detached_backend(
        &mut self,
        _request: DetachedVideoBackendRequest<Self::RequestId>,
    ) -> Result<DetachedVideoBackendReply<Self::RequestId>, DetachedVideoBackendPortError> {
        panic!("fake player port must not inspect app video resource port")
    }

    fn publish_candidate_status(
        &mut self,
        _status: DetachedVideoBackendCandidateStatus<Self::RequestId>,
    ) -> Result<(), DetachedVideoBackendPortError> {
        panic!("fake player port must not publish app candidate status")
    }

    fn cancel_candidate(
        &mut self,
        _request_id: Self::RequestId,
        _cause: DetachedVideoBackendCandidateCancellationCause,
    ) -> Result<(), DetachedVideoBackendPortError> {
        panic!("fake player port must not cancel app candidate")
    }
}

#[derive(Default)]
struct FakeInstallSlots {
    ready: Option<MediaInstallPhase>,
    completion: Option<MediaInstallCompletion>,
}

struct FakeInstallReceipt {
    slots: Arc<Mutex<FakeInstallSlots>>,
}

impl InstallReceiptPort for FakeInstallReceipt {
    fn take_ready(&self) -> Option<MediaInstallPhase> {
        self.slots.lock().expect("install slots").ready.take()
    }

    fn take_completion(&self) -> Option<MediaInstallCompletion> {
        self.slots.lock().expect("install slots").completion.take()
    }

    fn wait_until_signal_available(&self) -> Result<(), ()> {
        let slots = self.slots.lock().expect("install slots");
        if slots.ready.is_some() || slots.completion.is_some() {
            Ok(())
        } else {
            Err(())
        }
    }
}

enum FakeControlState {
    Pending,
    Outcome(MediaInstallControlOutcome),
    Missing,
}

struct FakeControlReceipt {
    state: Arc<Mutex<FakeControlState>>,
}

impl ControlReceiptPort for FakeControlReceipt {
    fn take_outcome(&self) -> Result<Option<MediaInstallControlOutcome>, ()> {
        let mut state = self.state.lock().expect("control state");
        match std::mem::replace(&mut *state, FakeControlState::Pending) {
            FakeControlState::Pending => Ok(None),
            FakeControlState::Outcome(outcome) => Ok(Some(outcome)),
            FakeControlState::Missing => Err(()),
        }
    }

    fn wait_until_outcome_available(&self) -> Result<(), ()> {
        match *self.state.lock().expect("control state") {
            FakeControlState::Outcome(_) => Ok(()),
            FakeControlState::Pending | FakeControlState::Missing => Err(()),
        }
    }
}

struct FakePlayerState {
    install_slots: Arc<Mutex<FakeInstallSlots>>,
    control_states: VecDeque<Arc<Mutex<FakeControlState>>>,
    authorize_rejection: Option<PlayerDispatchRejection>,
    authorize_calls: usize,
    prepare_position_calls: usize,
    cancel_calls: Vec<MediaInstallCancellationCause>,
    staged_request_id: Option<MediaInstallRequestId>,
    intent_updates: Vec<PlaybackIntentUpdate>,
}

struct FakePlayerPort {
    state: Arc<Mutex<FakePlayerState>>,
}

impl MediaOpenPlayerPort for FakePlayerPort {
    fn stage(
        &self,
        request_id: MediaInstallRequestId,
        _prepared_media: player_core::PreparedMedia,
        _intent: MediaOpenInstallIntent,
        _video_resource_port: MediaInstallVideoResourcePort,
        _position_preparation: MediaOpenPositionPreparation,
    ) -> Result<Box<dyn InstallReceiptPort>, PlayerDispatchRejection> {
        let mut state = self.state.lock().expect("fake player state");
        state.staged_request_id = Some(request_id);
        Ok(Box::new(FakeInstallReceipt {
            slots: Arc::clone(&state.install_slots),
        }))
    }

    fn prepare_position(
        &self,
        _request_id: MediaInstallRequestId,
    ) -> Result<(), PlayerDispatchRejection> {
        self.state
            .lock()
            .expect("fake player state")
            .prepare_position_calls += 1;
        Ok(())
    }

    fn authorize(
        &self,
        _request_id: MediaInstallRequestId,
    ) -> Result<Box<dyn ControlReceiptPort>, PlayerDispatchRejection> {
        let mut state = self.state.lock().expect("fake player state");
        state.authorize_calls += 1;
        if let Some(rejection) = state.authorize_rejection {
            return Err(rejection);
        }
        let control_state = state
            .control_states
            .pop_front()
            .expect("authorization control state queued");
        Ok(Box::new(FakeControlReceipt {
            state: control_state,
        }))
    }

    fn cancel(
        &self,
        _request_id: MediaInstallRequestId,
        cause: MediaInstallCancellationCause,
    ) -> Result<Box<dyn ControlReceiptPort>, PlayerDispatchRejection> {
        let mut state = self.state.lock().expect("fake player state");
        state.cancel_calls.push(cause);
        let control_state = state
            .control_states
            .pop_front()
            .expect("cancellation control state queued");
        Ok(Box::new(FakeControlReceipt {
            state: control_state,
        }))
    }

    fn update_intent(
        &self,
        update: PlaybackIntentUpdate,
    ) -> Result<PlaybackIntentUpdateReceipt, PlayerDispatchRejection> {
        self.state
            .lock()
            .expect("fake player state")
            .intent_updates
            .push(update);
        Err(PlayerDispatchRejection::Disconnected)
    }
}

fn coordinator() -> MediaOpenCoordinator {
    MediaOpenCoordinator::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime))
}

fn client(value: u64) -> MediaOpenClientKey {
    MediaOpenClientKey::from_non_zero(NonZeroU64::new(value).expect("non-zero client"))
}

fn fake_prepared_with_descriptor(descriptor: PreparedMediaDescriptor) -> PreparedMediaOpen {
    PreparedMediaOpen {
        prepared_media: player_core::PreparedMedia::from_external_label(
            "safe.test",
            Box::new(FakeDemuxer),
        ),
        descriptor,
    }
}

fn fake_prepared() -> PreparedMediaOpen {
    fake_prepared_with_descriptor(PreparedMediaDescriptor::Local {
        media_kind: LocalMediaKind::AudioOnly,
        tracks: Vec::new(),
        duration: None,
        metadata: media_core::MediaTagMetadata::default(),
        fingerprint: LocalMediaFingerprint::new(7, SystemTime::UNIX_EPOCH),
        source: ActiveMediaSource::LocalFile("fixture.wav".into()),
        safe_label: SafeMediaLabel::from_service_safe_label("fixture.wav"),
        fingerprint_validation: crate::media_open::LocalFingerprintValidation::Matched,
    })
}

fn wait_until_prepared(coordinator: &mut MediaOpenCoordinator) -> MediaOpenRequestId {
    for _ in 0..1_000 {
        coordinator.drain();
        let snapshot = coordinator.snapshot().expect("current request");
        if snapshot.phase == MediaOpenPhase::Prepared {
            return snapshot.request_id;
        }
        thread::sleep(Duration::from_millis(1));
    }
    panic!("fake preparation did not complete")
}

fn attach_fake_player(
    coordinator: &mut MediaOpenCoordinator,
    authorize_rejection: Option<PlayerDispatchRejection>,
    control_states: Vec<Arc<Mutex<FakeControlState>>>,
) -> Arc<Mutex<FakePlayerState>> {
    let state = Arc::new(Mutex::new(FakePlayerState {
        install_slots: Arc::new(Mutex::new(FakeInstallSlots::default())),
        control_states: control_states.into(),
        authorize_rejection,
        authorize_calls: 0,
        prepare_position_calls: 0,
        cancel_calls: Vec::new(),
        staged_request_id: None,
        intent_updates: Vec::new(),
    }));
    coordinator.attach_fake_player(Arc::new(FakePlayerPort {
        state: Arc::clone(&state),
    }));
    state
}

#[test]
fn ready_passes_through_without_auto_authorization_then_enqueue_wins() {
    let mut coordinator = coordinator();
    coordinator
        .start_fake(
            client(1),
            SafeMediaLabel::from_service_safe_label("safe.test"),
            || Ok(fake_prepared()),
        )
        .expect("start accepted");
    let request_id = wait_until_prepared(&mut coordinator);
    let authorization_state = Arc::new(Mutex::new(FakeControlState::Pending));
    let player_state = attach_fake_player(
        &mut coordinator,
        None,
        vec![Arc::clone(&authorization_state)],
    );
    let player_request_id = coordinator
        .stage_at_player(
            request_id,
            MediaOpenInstallIntent {
                intent: player_core::PlaybackIntent::StartPaused,
                revision: player_core::PlaybackIntentRevision::INITIAL,
            },
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
        )
        .expect("stage accepted");
    player_state
        .lock()
        .expect("player state")
        .install_slots
        .lock()
        .expect("install slots")
        .ready = Some(MediaInstallPhase::ReadyToCommit {
        request_id: player_request_id,
    });
    assert_eq!(
        coordinator.wait_for_progress(request_id),
        Ok(MediaOpenPhase::ReadyToCommit)
    );
    assert_eq!(
        coordinator.snapshot().expect("snapshot").phase,
        MediaOpenPhase::ReadyToCommit
    );
    assert_eq!(
        player_state.lock().expect("player state").authorize_calls,
        0
    );

    assert_eq!(
        coordinator.authorize_ready(request_id),
        Ok(AuthorizationDispatchResolution::EnqueuedAtPlayerOwner)
    );
    assert_eq!(
        coordinator.snapshot().expect("snapshot").phase,
        MediaOpenPhase::EnqueuedAtPlayerOwner
    );
    coordinator.suspend_player_binding();
    assert_eq!(
        coordinator.snapshot().expect("snapshot").phase,
        MediaOpenPhase::EnqueuedAtPlayerOwner
    );
    assert_eq!(
        coordinator.cancel_request(request_id, MediaInstallCancellationCause::TransportStop,),
        Ok(CancellationDispatchOutcome::CommitMustFinish)
    );

    let installed = MediaInstallCompletion::Installed {
        request_id: player_request_id,
        media_instance_id: player_core::MediaInstanceId::from_non_zero(
            NonZeroU64::new(9).expect("non-zero instance"),
        ),
        applied_intent_revision: player_core::PlaybackIntentRevision::INITIAL,
        applied_intent: player_core::PlaybackIntent::StartPaused,
    };
    player_state
        .lock()
        .expect("player state")
        .install_slots
        .lock()
        .expect("install slots")
        .completion = Some(installed);
    *authorization_state.lock().expect("authorization state") =
        FakeControlState::Outcome(MediaInstallControlOutcome::AuthorizationAccepted);
    assert_eq!(
        coordinator.wait_for_progress(request_id),
        Ok(MediaOpenPhase::Installed)
    );
    assert!(matches!(
        coordinator.take_terminal(request_id),
        Ok(Some(MediaOpenTerminalOutcome::Installed { .. }))
    ));
}

#[test]
fn missing_player_install_resolution_is_fatal_before_ready() {
    let mut coordinator = coordinator();
    coordinator
        .start_fake(
            client(15),
            SafeMediaLabel::from_service_safe_label("missing-install.test"),
            || Ok(fake_prepared()),
        )
        .expect("start accepted");
    let request_id = wait_until_prepared(&mut coordinator);
    attach_fake_player(&mut coordinator, None, Vec::new());
    coordinator
        .stage_at_player(
            request_id,
            MediaOpenInstallIntent {
                intent: player_core::PlaybackIntent::StartPaused,
                revision: player_core::PlaybackIntentRevision::INITIAL,
            },
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
        )
        .expect("stage accepted");

    assert_eq!(
        coordinator.wait_for_progress(request_id),
        Err(MediaOpenCompletionDriveError::MissingPlayerResolution)
    );
    assert!(matches!(
        coordinator.take_terminal(request_id),
        Ok(Some(MediaOpenTerminalOutcome::FatalInvariant {
            violation: MediaOpenInvariantViolation::MissingPlayerInstallResolution,
            ..
        }))
    ));
}

#[test]
fn accepted_phase_is_observable_before_preparation_drain() {
    let mut coordinator = coordinator();
    let accepted = coordinator
        .start_fake(
            client(1),
            SafeMediaLabel::from_service_safe_label("safe.test"),
            || Ok(fake_prepared()),
        )
        .expect("start accepted");
    let MediaOpenStartOutcome::Accepted { request_id } = accepted else {
        panic!("idle coordinator cannot coalesce first request");
    };

    let snapshot = coordinator.snapshot().expect("accepted request snapshot");
    assert_eq!(snapshot.request_id, request_id);
    assert_eq!(snapshot.phase, MediaOpenPhase::Accepted);
}

#[test]
fn caller_prepared_ingress_enters_same_protocol_without_auto_authorization() {
    let mut coordinator = coordinator();
    let safe_label = SafeMediaLabel::from_service_safe_label("fixture.wav");
    let prepared_open = PreparedMediaOpen::from_caller_prepared(
        player_core::PreparedMedia::from_external_label("fixture.wav", Box::new(FakeDemuxer)),
        ActiveMediaSource::LocalFile("fixture.wav".into()),
        safe_label.clone(),
    );

    let accepted = coordinator
        .start_prepared(client(1), prepared_open, safe_label)
        .expect("caller-prepared request accepted");
    let MediaOpenStartOutcome::Accepted { request_id } = accepted else {
        panic!("idle coordinator cannot coalesce prepared compatibility request");
    };
    let snapshot = coordinator.snapshot().expect("prepared request snapshot");

    assert_eq!(snapshot.request_id, request_id);
    assert_eq!(snapshot.phase, MediaOpenPhase::Prepared);
    assert!(snapshot.authorization_resolution.is_none());
    assert!(matches!(
        snapshot.descriptor,
        Some(PreparedMediaDescriptor::CallerPrepared { .. })
    ));
    assert!(coordinator.take_terminal(request_id).unwrap().is_none());
}

#[test]
fn preparation_panic_becomes_typed_terminal_instead_of_lost_result() {
    let mut coordinator = coordinator();
    coordinator
        .start_fake(
            client(1),
            SafeMediaLabel::from_service_safe_label("safe.test"),
            || panic!("synthetic preparation panic"),
        )
        .expect("request accepted before worker executes task");
    let request_id = coordinator.snapshot().expect("accepted request").request_id;

    for _ in 0..1_000 {
        coordinator.drain();
        if matches!(
            coordinator.take_terminal(request_id),
            Ok(Some(MediaOpenTerminalOutcome::PreparationFailed {
                kind: MediaPreparationFailureKind::WorkerPanicked,
                ..
            }))
        ) {
            return;
        }
        thread::sleep(Duration::from_millis(1));
    }
    panic!("typed worker-panic terminal was not published");
}

#[test]
fn local_and_direct_descriptors_follow_the_same_prepared_phase() {
    let direct_locator = service_direct_media::parse_direct_media_url(
        "https://media.example.test/movie.mp4?token=secret",
    )
    .expect("direct locator");
    let descriptors = [
        PreparedMediaDescriptor::Local {
            media_kind: LocalMediaKind::AudioOnly,
            tracks: Vec::new(),
            duration: None,
            metadata: media_core::MediaTagMetadata::default(),
            fingerprint: LocalMediaFingerprint::new(7, SystemTime::UNIX_EPOCH),
            source: ActiveMediaSource::LocalFile("fixture.wav".into()),
            safe_label: SafeMediaLabel::from_service_safe_label("fixture.wav"),
            fingerprint_validation: crate::media_open::LocalFingerprintValidation::Matched,
        },
        PreparedMediaDescriptor::Web(PreparedWebMediaEnvelope::new(
            Vec::new(),
            None,
            media_core::MediaTagMetadata::default(),
            WebMediaSourceIntent::direct(direct_locator),
            SafeMediaLabel::from_service_safe_label("media.example.test"),
            None,
            None,
        )),
    ];

    for (index, descriptor) in descriptors.into_iter().enumerate() {
        let mut coordinator = coordinator();
        coordinator
            .start_fake(
                client((index + 1) as u64),
                SafeMediaLabel::from_service_safe_label("safe.test"),
                move || Ok(fake_prepared_with_descriptor(descriptor)),
            )
            .expect("source-neutral request accepted");
        let _request_id = wait_until_prepared(&mut coordinator);
        assert!(matches!(
            coordinator
                .snapshot()
                .expect("prepared snapshot")
                .descriptor,
            Some(PreparedMediaDescriptor::Local { .. }) | Some(PreparedMediaDescriptor::Web(_))
        ));
    }
}

#[test]
fn downstream_authorization_rejection_is_pre_enqueue_resolution() {
    let mut coordinator = coordinator();
    coordinator
        .start_fake(
            client(1),
            SafeMediaLabel::from_service_safe_label("safe.test"),
            || Ok(fake_prepared()),
        )
        .expect("start accepted");
    let request_id = wait_until_prepared(&mut coordinator);
    let cancellation_state = Arc::new(Mutex::new(FakeControlState::Pending));
    let player_state = attach_fake_player(
        &mut coordinator,
        Some(PlayerDispatchRejection::Backpressure),
        vec![Arc::clone(&cancellation_state)],
    );
    let player_request_id = coordinator
        .stage_at_player(
            request_id,
            MediaOpenInstallIntent {
                intent: player_core::PlaybackIntent::StartPlaying,
                revision: player_core::PlaybackIntentRevision::INITIAL,
            },
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
        )
        .expect("stage accepted");
    player_state
        .lock()
        .expect("player state")
        .install_slots
        .lock()
        .expect("install slots")
        .ready = Some(MediaInstallPhase::ReadyToCommit {
        request_id: player_request_id,
    });
    coordinator.drain();

    assert_eq!(
        coordinator.authorize_ready(request_id),
        Err(MediaOpenCommandError::PlayerDispatch(
            PlayerDispatchRejection::Backpressure
        ))
    );
    let snapshot = coordinator.snapshot().expect("snapshot");
    assert_eq!(snapshot.phase, MediaOpenPhase::ReadyToCommit);
    assert_eq!(
        snapshot.authorization_resolution,
        Some(
            AuthorizationDispatchResolution::DownstreamRejectedBeforeEnqueue {
                rejection: PlayerDispatchRejection::Backpressure
            }
        )
    );

    assert_eq!(
        coordinator.cancel_request_lossless(
            request_id,
            MediaInstallCancellationCause::StructuralInvalidation,
        ),
        Ok(CancellationDispatchOutcome::DispatchPending)
    );
    player_state
        .lock()
        .expect("player state")
        .install_slots
        .lock()
        .expect("install slots")
        .completion = Some(MediaInstallCompletion::Cancelled {
        request_id: player_request_id,
        cause: MediaInstallCancellationCause::StructuralInvalidation,
    });
    *cancellation_state.lock().expect("cancellation state") =
        FakeControlState::Outcome(MediaInstallControlOutcome::CancellationAccepted);
    assert_eq!(
        coordinator.wait_for_progress(request_id),
        Ok(MediaOpenPhase::Failed)
    );
    assert!(matches!(
        coordinator.take_terminal(request_id),
        Ok(Some(MediaOpenTerminalOutcome::Cancelled {
            cause: MediaInstallCancellationCause::StructuralInvalidation,
            ..
        }))
    ));
}

#[test]
fn cancellation_causes_remain_distinct_before_player_staging() {
    let causes = [
        MediaInstallCancellationCause::UserCancelled,
        MediaInstallCancellationCause::Superseded,
        MediaInstallCancellationCause::TransportStop,
        MediaInstallCancellationCause::StructuralInvalidation,
        MediaInstallCancellationCause::LifecycleSuspended,
        MediaInstallCancellationCause::LifecycleShutdown,
    ];
    for (index, cause) in causes.into_iter().enumerate() {
        let mut coordinator = coordinator();
        coordinator
            .start_fake(
                client((index + 1) as u64),
                SafeMediaLabel::from_service_safe_label("safe.test"),
                || Ok(fake_prepared()),
            )
            .expect("start accepted");
        let request_id = coordinator.snapshot().expect("accepted request").request_id;
        assert_eq!(
            coordinator.cancel_request(request_id, cause),
            Ok(CancellationDispatchOutcome::CancelledBeforePlayerStaging)
        );
        assert!(matches!(
            coordinator.take_terminal(request_id),
            Ok(Some(MediaOpenTerminalOutcome::Cancelled { cause: actual, .. })) if actual == cause
        ));
    }
}

#[test]
fn caller_supersede_starts_latest_while_non_cancellable_stale_work_is_blocked() {
    let mut coordinator = coordinator();
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let first = coordinator
        .start_fake(
            client(1),
            SafeMediaLabel::from_service_safe_label("first"),
            move || {
                started_tx.send(()).expect("publish stale work start");
                release_rx.recv().expect("release stale work");
                Ok(fake_prepared())
            },
        )
        .expect("first start");
    let first_id = match first {
        MediaOpenStartOutcome::Accepted { request_id } => request_id,
        MediaOpenStartOutcome::Coalesced { .. } => panic!("first request cannot coalesce"),
    };
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("stale work must occupy its worker before supersede");
    let coalesced = coordinator
        .start_with_task(
            client(1),
            MediaOpenStartMode::CoalesceMatchingClient,
            SafeMediaLabel::from_service_safe_label("ignored"),
            |_cancellation| Ok(fake_prepared()),
        )
        .expect("coalesce accepted");
    assert_eq!(
        coalesced,
        MediaOpenStartOutcome::Coalesced {
            request_id: first_id
        }
    );
    coordinator
        .supersede_fake(
            first_id,
            client(2),
            SafeMediaLabel::from_service_safe_label("second"),
            || Ok(fake_prepared()),
        )
        .expect("supersede accepted");

    // Первый fake намеренно не проверяет cancellation. Latest request обязан
    // подготовиться на bounded соседнем worker-е до освобождения stale open.
    let latest_id = wait_until_prepared(&mut coordinator);
    assert_ne!(latest_id, first_id);
    release_tx.send(()).expect("release stale work");
    assert_eq!(MAX_NON_CANCELLABLE_STALE_PREPARATIONS, 1);
}

#[test]
fn stale_request_command_is_not_reported_as_player_backpressure() {
    let mut coordinator = coordinator();
    coordinator
        .start_fake(
            client(1),
            SafeMediaLabel::from_service_safe_label("first"),
            || Ok(fake_prepared()),
        )
        .expect("first request accepted");
    let first_request_id = wait_until_prepared(&mut coordinator);
    coordinator
        .supersede_fake(
            first_request_id,
            client(2),
            SafeMediaLabel::from_service_safe_label("second"),
            || Ok(fake_prepared()),
        )
        .expect("second request supersedes first");

    assert_eq!(
        coordinator.cancel_request(
            first_request_id,
            MediaInstallCancellationCause::UserCancelled,
        ),
        Err(MediaOpenCommandError::StaleRequest)
    );
}

#[test]
fn repeated_cancel_does_not_replace_authoritative_control_receipt() {
    let mut coordinator = coordinator();
    coordinator
        .start_fake(
            client(1),
            SafeMediaLabel::from_service_safe_label("safe.test"),
            || Ok(fake_prepared()),
        )
        .expect("start accepted");
    let request_id = wait_until_prepared(&mut coordinator);
    let control_state = Arc::new(Mutex::new(FakeControlState::Pending));
    let player_state = attach_fake_player(&mut coordinator, None, vec![Arc::clone(&control_state)]);
    coordinator
        .stage_at_player(
            request_id,
            MediaOpenInstallIntent {
                intent: player_core::PlaybackIntent::StartPaused,
                revision: player_core::PlaybackIntentRevision::INITIAL,
            },
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
        )
        .expect("stage accepted");

    assert_eq!(
        coordinator.cancel_request(request_id, MediaInstallCancellationCause::UserCancelled,),
        Ok(CancellationDispatchOutcome::DispatchPending)
    );
    assert_eq!(
        coordinator.cancel_request(
            request_id,
            MediaInstallCancellationCause::LifecycleSuspended,
        ),
        Ok(CancellationDispatchOutcome::DispatchPending)
    );
    assert_eq!(
        player_state
            .lock()
            .expect("player state")
            .cancel_calls
            .len(),
        1
    );
}

#[test]
fn d52_update_forwards_exact_player_request_revision_and_intent() {
    let mut coordinator = coordinator();
    coordinator
        .start_fake(
            client(1),
            SafeMediaLabel::from_service_safe_label("safe.test"),
            || Ok(fake_prepared()),
        )
        .expect("start accepted");
    let request_id = wait_until_prepared(&mut coordinator);
    let player_state = attach_fake_player(&mut coordinator, None, Vec::new());
    let player_request_id = coordinator
        .stage_at_player(
            request_id,
            MediaOpenInstallIntent {
                intent: player_core::PlaybackIntent::StartPaused,
                revision: player_core::PlaybackIntentRevision::INITIAL,
            },
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
        )
        .expect("stage accepted");

    assert!(matches!(
        coordinator.update_playback_intent(
            request_id,
            player_core::PlaybackIntentRevision::INITIAL,
            player_core::PlaybackIntent::StartPlaying,
        ),
        Err(MediaOpenCommandError::PlayerDispatch(
            PlayerDispatchRejection::Disconnected,
        ))
    ));
    assert_eq!(
        player_state.lock().expect("player state").intent_updates,
        vec![PlaybackIntentUpdate {
            request_id: player_request_id,
            revision: player_core::PlaybackIntentRevision::INITIAL,
            intent: player_core::PlaybackIntent::StartPlaying,
        }]
    );
}

#[test]
fn cancel_control_and_missing_resolution_are_authoritative() {
    let mut cancel_coordinator = coordinator();
    cancel_coordinator
        .start_fake(
            client(1),
            SafeMediaLabel::from_service_safe_label("safe.test"),
            || Ok(fake_prepared()),
        )
        .expect("start accepted");
    let request_id = wait_until_prepared(&mut cancel_coordinator);
    let cancel_state = Arc::new(Mutex::new(FakeControlState::Pending));
    let player_state = attach_fake_player(
        &mut cancel_coordinator,
        None,
        vec![Arc::clone(&cancel_state)],
    );
    cancel_coordinator
        .stage_at_player(
            request_id,
            MediaOpenInstallIntent {
                intent: player_core::PlaybackIntent::StartPaused,
                revision: player_core::PlaybackIntentRevision::INITIAL,
            },
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
        )
        .expect("stage accepted");
    assert_eq!(
        cancel_coordinator.cancel_request(
            request_id,
            MediaInstallCancellationCause::LifecycleSuspended,
        ),
        Ok(CancellationDispatchOutcome::DispatchPending)
    );
    let cancel_player_request_id = player_state
        .lock()
        .expect("player state")
        .staged_request_id
        .expect("staged request id");
    player_state
        .lock()
        .expect("player state")
        .install_slots
        .lock()
        .expect("install slots")
        .completion = Some(MediaInstallCompletion::Cancelled {
        request_id: cancel_player_request_id,
        cause: MediaInstallCancellationCause::LifecycleSuspended,
    });
    *cancel_state.lock().expect("cancel state") =
        FakeControlState::Outcome(MediaInstallControlOutcome::CancellationAccepted);
    assert_eq!(
        cancel_coordinator.wait_for_progress(request_id),
        Ok(MediaOpenPhase::Failed)
    );
    assert!(matches!(
        cancel_coordinator.take_terminal(request_id),
        Ok(Some(MediaOpenTerminalOutcome::Cancelled {
            cause: MediaInstallCancellationCause::LifecycleSuspended,
            ..
        }))
    ));

    let mut missing = coordinator();
    missing
        .start_fake(
            client(2),
            SafeMediaLabel::from_service_safe_label("safe.test"),
            || Ok(fake_prepared()),
        )
        .expect("start accepted");
    let request_id = wait_until_prepared(&mut missing);
    let missing_state = Arc::new(Mutex::new(FakeControlState::Missing));
    let player_state = attach_fake_player(&mut missing, None, vec![Arc::clone(&missing_state)]);
    missing
        .stage_at_player(
            request_id,
            MediaOpenInstallIntent {
                intent: player_core::PlaybackIntent::StartPaused,
                revision: player_core::PlaybackIntentRevision::INITIAL,
            },
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
        )
        .expect("stage accepted");
    missing
        .cancel_request(request_id, MediaInstallCancellationCause::UserCancelled)
        .expect("cancel dispatched");
    assert_eq!(
        missing.wait_for_progress(request_id),
        Err(MediaOpenCompletionDriveError::MissingPlayerResolution)
    );
    assert!(matches!(
        missing.take_terminal(request_id),
        Ok(Some(MediaOpenTerminalOutcome::FatalInvariant {
            violation: MediaOpenInvariantViolation::MissingPlayerControlResolution,
            ..
        }))
    ));
    drop(player_state);
}

#[test]
fn authorization_ack_without_installed_terminal_is_fatal() {
    let mut coordinator = coordinator();
    coordinator
        .start_fake(
            client(3),
            SafeMediaLabel::from_service_safe_label("safe.test"),
            || Ok(fake_prepared()),
        )
        .expect("start accepted");
    let request_id = wait_until_prepared(&mut coordinator);
    let authorization_state = Arc::new(Mutex::new(FakeControlState::Outcome(
        MediaInstallControlOutcome::AuthorizationAccepted,
    )));
    let player_state = attach_fake_player(
        &mut coordinator,
        None,
        vec![Arc::clone(&authorization_state)],
    );
    let player_request_id = coordinator
        .stage_at_player(
            request_id,
            MediaOpenInstallIntent {
                intent: player_core::PlaybackIntent::StartPaused,
                revision: player_core::PlaybackIntentRevision::INITIAL,
            },
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
        )
        .expect("stage accepted");
    player_state
        .lock()
        .expect("player state")
        .install_slots
        .lock()
        .expect("install slots")
        .ready = Some(MediaInstallPhase::ReadyToCommit {
        request_id: player_request_id,
    });
    coordinator.drain();
    coordinator
        .authorize_ready(request_id)
        .expect("authorization enqueued");

    assert!(coordinator.drain());
    assert!(matches!(
        coordinator.take_terminal(request_id),
        Ok(Some(MediaOpenTerminalOutcome::FatalInvariant {
            violation: MediaOpenInvariantViolation::MissingInstalledAfterPlayerEnqueue,
            ..
        }))
    ));
}
