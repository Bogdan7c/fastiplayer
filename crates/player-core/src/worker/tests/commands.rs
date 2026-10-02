//! Порядок и маршрутизация команд, scrub API и отключение.

use super::*;

#[test]
fn command_ordering_for_play_pause_stop_open_shutdown_is_preserved() {
    let mut worker = PlayerWorker::spawn(worker_config_for_tests()).unwrap();
    let request = MediaOpenRequest::new(MediaSource::ExternalLabel("sample".into()), false);

    worker.try_send_command(PlayerCommand::Play).unwrap();
    worker.try_send_command(PlayerCommand::Pause).unwrap();
    worker.try_send_command(PlayerCommand::Stop).unwrap();
    worker
        .try_send_command(PlayerCommand::OpenMedia(request.clone()))
        .unwrap();
    worker.try_send_command(PlayerCommand::Shutdown).unwrap();

    let events = drain_events_until(&worker, |events| {
        events.iter().any(|event| {
            matches!(
                event,
                PlayerWorkerEvent::Player(CorrelatedPlayerEvent {
                    event: PlayerEvent::ShutdownRequested,
                    ..
                })
            )
        })
    });
    let player_events = events
        .iter()
        .filter_map(|event| match event {
            PlayerWorkerEvent::Player(event) => Some(&event.event),
            PlayerWorkerEvent::Scrub(_) => None,
            PlayerWorkerEvent::RenderError(_) => None,
            PlayerWorkerEvent::Tick(_) => None,
        })
        .collect::<Vec<_>>();

    let playing_index = player_events
        .iter()
        .position(|event| {
            matches!(
                event,
                PlayerEvent::PlaybackStateChanged(PlaybackState::Playing)
            )
        })
        .expect("missing Playing event");
    let paused_index = player_events
        .iter()
        .position(|event| {
            matches!(
                event,
                PlayerEvent::PlaybackStateChanged(PlaybackState::Paused)
            )
        })
        .expect("missing Paused event");
    let open_index = player_events
        .iter()
        .position(|event| {
            matches!(
                event,
                PlayerEvent::MediaOpenRequested(open_request) if *open_request == request
            )
        })
        .expect("missing OpenMedia event");
    let shutdown_index = player_events
        .iter()
        .position(|event| matches!(event, PlayerEvent::ShutdownRequested))
        .expect("missing Shutdown event");

    assert!(playing_index < paused_index);
    assert!(paused_index < open_index);
    assert!(open_index < shutdown_index);
    worker.shutdown().unwrap();
}

#[test]
fn command_sender_routes_player_commands_through_worker_queue() {
    let (command_sender, command_rx) = command_sender_for_tests();
    let open_request = MediaOpenRequest::new(MediaSource::ExternalLabel("sample".into()), false);
    let seek_request = seek_to_millis(500);

    command_sender.try_send(PlayerCommand::Play).unwrap();
    assert_eq!(receive_player_command(&command_rx), PlayerCommand::Play);

    command_sender
        .try_send(PlayerCommand::OpenMedia(open_request.clone()))
        .unwrap();
    assert_eq!(
        receive_player_command(&command_rx),
        PlayerCommand::OpenMedia(open_request)
    );

    command_sender
        .try_send(PlayerCommand::begin_scrub())
        .unwrap();
    assert_eq!(
        receive_player_command(&command_rx),
        PlayerCommand::begin_scrub()
    );

    command_sender
        .try_send(PlayerCommand::UpdateScrub(seek_request))
        .unwrap();
    assert_eq!(
        receive_player_command(&command_rx),
        PlayerCommand::UpdateScrub(seek_request)
    );

    command_sender
        .try_send(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitLatestTarget,
        ))
        .unwrap();
    assert_eq!(
        receive_player_command(&command_rx),
        PlayerCommand::end_scrub(ScrubCommitPolicy::CommitLatestTarget)
    );
}

#[test]
fn exact_reset_receipt_delivers_the_owner_result() {
    let (command_sender, command_rx) = command_sender_for_tests();
    let media_instance_id = MediaInstanceId::new_unique();
    let request = ExactMediaTransportRequest {
        media_instance_id,
        action: ExactMediaTransportAction::ResetMedia,
    };
    let receipt = command_sender
        .exact_media_transport(request)
        .expect("exact reset command must enter the worker queue");

    let WorkerCommand::ExactMediaTransport {
        request: queued_request,
        outcome_tx,
    } = command_rx
        .try_recv()
        .expect("worker queue must retain the exact reset command")
    else {
        panic!("exact reset must use WorkerCommand::ExactMediaTransport");
    };
    assert_eq!(queued_request, request);
    let owner_outcome = ExactMediaTransportOutcome::Applied { media_instance_id };
    outcome_tx
        .send(owner_outcome.clone())
        .expect("request-owned receipt must still be alive");

    assert_eq!(
        receipt
            .wait_for_outcome()
            .expect("worker owner must publish one terminal outcome"),
        owner_outcome
    );
}

#[test]
fn public_scrub_api_uses_session_seek_landing_route() {
    let mut runtime = runtime_for_tests(Instant::now());
    let seek_request_log = Arc::new(Mutex::new(Vec::new()));

    install_worker_video_media(&mut runtime, Arc::clone(&seek_request_log));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::begin_scrub()));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::UpdateScrub(
        seek_to_millis(20_000),
    )));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::end_scrub(
        ScrubCommitPolicy::CommitLatestTarget,
    )));

    let expected_request = DemuxSeekRequest::decode_point_before(Duration::from_secs(20));
    assert_eq!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .as_slice(),
        &[expected_request]
    );
    assert!(runtime.session.has_active_seek_commit());
    assert!(!runtime.session.snapshot().timeline.seeking);
    assert!(runtime.session.snapshot().timeline.scrubbing);
    assert_eq!(
        runtime.session.snapshot().timeline.preview_state,
        media_core::TimelinePreviewState::Pending
    );
}

#[test]
fn stop_during_direct_scrub_is_plain_session_stop() {
    let mut runtime = runtime_for_tests(Instant::now());
    let seek_request_log = Arc::new(Mutex::new(Vec::new()));

    install_worker_video_media(&mut runtime, Arc::clone(&seek_request_log));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::begin_scrub()));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::UpdateScrub(
        seek_to_millis(900),
    )));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::Stop));

    assert_eq!(
        runtime.session.snapshot().playback_state,
        PlaybackState::Stopped
    );
    assert!(!runtime.session.snapshot().timeline.scrubbing);
    assert!(
        seek_request_log
            .lock()
            .expect("seek request log lock")
            .is_empty()
    );
}

#[test]
fn command_sender_returns_disconnected_after_worker_shutdown() {
    let mut worker = PlayerWorker::spawn(worker_config_for_tests()).unwrap();
    let command_sender = worker.command_sender();

    worker.shutdown().unwrap();
    let result = command_sender.try_send(PlayerCommand::Play);

    assert_eq!(result, Err(PlayerWorkerSendError::Disconnected));
}
