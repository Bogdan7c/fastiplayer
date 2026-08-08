use super::*;

#[test]
fn same_lineage_position_phase_precedes_true_ready_and_owner_accepted_enqueue() {
    let mut coordinator = coordinator();
    let request_id = match coordinator
        .start_prepared(
            client(77),
            fake_prepared(),
            SafeMediaLabel::from_service_safe_label("same-lineage.test"),
        )
        .expect("prepared request accepted")
    {
        MediaOpenStartOutcome::Accepted { request_id } => request_id,
        MediaOpenStartOutcome::Coalesced { .. } => panic!("unexpected coalesce"),
    };
    let authorization_state = Arc::new(Mutex::new(FakeControlState::Pending));
    let player_state = attach_fake_player(
        &mut coordinator,
        None,
        vec![Arc::clone(&authorization_state)],
    );
    let expected_old_media_instance_id =
        player_core::MediaInstanceId::from_non_zero(NonZeroU64::new(770).expect("old instance id"));
    let player_request_id = coordinator
        .stage_same_lineage_at_player(
            request_id,
            MediaOpenInstallIntent {
                intent: player_core::PlaybackIntent::StartPlaying,
                revision: player_core::PlaybackIntentRevision::INITIAL,
            },
            MediaInstallVideoResourcePort::any_playable(UnusedVideoResourcePort),
            expected_old_media_instance_id,
        )
        .expect("same-lineage stage");
    {
        let state = player_state.lock().expect("fake player state");
        state.install_slots.lock().expect("install slots").ready =
            Some(MediaInstallPhase::ReadyForPositionPreparation {
                request_id: player_request_id,
            });
    }
    assert!(coordinator.drain());
    assert_eq!(
        coordinator.snapshot().unwrap().same_lineage_position,
        SameLineagePositionPreparationPhase::ReadyForPositionPreparation
    );
    coordinator
        .prepare_same_lineage_position(request_id)
        .expect("position command accepted");
    assert_eq!(
        player_state
            .lock()
            .expect("fake player state")
            .prepare_position_calls,
        1
    );
    {
        let state = player_state.lock().expect("fake player state");
        state.install_slots.lock().expect("install slots").ready =
            Some(MediaInstallPhase::ReadyToCommit {
                request_id: player_request_id,
            });
    }
    assert!(coordinator.drain());
    assert_eq!(
        coordinator.snapshot().unwrap().phase,
        MediaOpenPhase::ReadyToCommit
    );
    coordinator
        .authorize_ready_same_lineage(request_id)
        .expect("authorization dispatched");
    let pending = coordinator.snapshot().unwrap();
    assert_eq!(pending.phase, MediaOpenPhase::AuthorizationDispatchPending);
    assert_eq!(pending.authorization_resolution, None);

    let new_instance_id =
        player_core::MediaInstanceId::from_non_zero(NonZeroU64::new(771).expect("new instance id"));
    {
        let state = player_state.lock().expect("fake player state");
        state
            .install_slots
            .lock()
            .expect("install slots")
            .completion = Some(MediaInstallCompletion::Installed {
            request_id: player_request_id,
            media_instance_id: new_instance_id,
            applied_intent_revision: player_core::PlaybackIntentRevision::INITIAL,
            applied_intent: player_core::PlaybackIntent::StartPlaying,
        });
    }
    *authorization_state.lock().expect("authorization state") =
        FakeControlState::Outcome(MediaInstallControlOutcome::AuthorizationAccepted);
    assert!(coordinator.drain());
    let installed = coordinator.snapshot().unwrap();
    assert_eq!(installed.phase, MediaOpenPhase::Installed);
    assert_eq!(
        installed.authorization_resolution,
        Some(AuthorizationDispatchResolution::EnqueuedAtPlayerOwner)
    );
}
