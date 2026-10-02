//! Старт worker-а, snapshot и compatibility media install.

use super::*;

#[test]
fn worker_starts_accepts_commands_publishes_snapshot_and_shutdowns() {
    let mut worker = PlayerWorker::spawn(worker_config_for_tests()).unwrap();

    worker.try_send_command(PlayerCommand::Play).unwrap();
    let snapshot = wait_for_snapshot(&mut worker, |snapshot| {
        snapshot.playback_state == PlaybackState::Playing
    });

    assert_eq!(snapshot.playback_state, PlaybackState::Playing);
    worker.shutdown().unwrap();
}

#[test]
fn paused_idle_worker_wakes_on_sliding_live_window_and_publishes_latest_snapshot() {
    #[derive(Clone)]
    struct TestTimelineWake {
        wake_tx: crossbeam_channel::Sender<()>,
    }

    impl PlayerWorkerTimelineWake for TestTimelineWake {
        fn wake_player_timeline(&self) {
            let _ = self.wake_tx.try_send(());
        }
    }

    let (wake_tx, wake_rx) = bounded(1);
    let config = worker_config_for_tests()
        .with_timeline_activity_wake(Arc::new(TestTimelineWake { wake_tx }));
    let mut worker = PlayerWorker::spawn(config).expect("worker starts");
    let initial_range = media_core::TimelineRange::new(
        media_core::MediaTime::from_secs(20),
        media_core::MediaTime::from_secs(60),
    )
    .expect("initial DVR range");
    let (port, publisher) =
        media_core::dynamic_media_timeline(media_core::DynamicMediaTimelineInitial {
            port_generation: media_core::DynamicMediaTimelinePortGeneration::new(
                NonZeroU64::new(70).expect("port generation"),
            ),
            source_epoch: media_core::DynamicMediaTimelineEpoch::new(1),
            state: media_core::DynamicMediaTimelineState::with_dvr(
                media_core::MediaTime::from_secs(60),
                initial_range,
            )
            .expect("initial DVR state"),
        });
    let demuxer = WorkerFakeDemuxer {
        tracks: Vec::new(),
        duration: None,
        seek_request_log: Arc::new(Mutex::new(Vec::new())),
    };
    let prepared_media = PreparedMedia::from_external_label("worker-live", Box::new(demuxer))
        .with_dynamic_timeline(port)
        .expect("duration-less worker media accepts live timeline");
    let _receipt = worker
        .load_prepared_media(prepared_media, false)
        .expect("live install command accepted");
    let _initial = wait_for_snapshot(&mut worker, |snapshot| {
        snapshot.timeline.live_revision.is_some()
            && snapshot.playback_state == PlaybackState::Paused
    });

    let moved_range = media_core::TimelineRange::new(
        media_core::MediaTime::from_secs(30),
        media_core::MediaTime::from_secs(70),
    )
    .expect("moved DVR range");
    publisher
        .publish(
            media_core::DynamicMediaTimelineEpoch::new(2),
            media_core::DynamicMediaTimelineState::with_dvr(
                media_core::MediaTime::from_secs(70),
                moved_range,
            )
            .expect("moved DVR state"),
        )
        .expect("publish moved DVR range");

    wake_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("paused worker emits app wake");
    let moved = worker.latest_snapshot(FrameCounters::default());
    assert_eq!(moved.timeline.seekable_range, Some(moved_range));
    assert_eq!(
        moved.timeline.live_edge,
        Some(media_core::MediaTime::from_secs(70))
    );

    worker.shutdown().expect("worker shutdown");
}

#[test]
fn compatibility_media_install_preserves_snapshot_and_correlates_completion_and_event() {
    let mut worker = PlayerWorker::spawn(worker_config_for_tests()).unwrap();
    let demuxer =
        WorkerFakeDemuxer::seekable_with_tracks(Vec::new(), Arc::new(Mutex::new(Vec::new())));
    let prepared_media =
        PreparedMedia::from_external_label("compatibility-media".to_owned(), Box::new(demuxer));

    let receipt = worker.load_prepared_media(prepared_media, false).unwrap();
    let request_id = receipt.request_id();
    let snapshot = wait_for_snapshot(&mut worker, |snapshot| {
        snapshot.source_label.as_deref() == Some("compatibility-media")
            && snapshot.media_instance_id.is_some()
    });

    let ready_phase = receipt
        .try_take_ready_to_commit()
        .expect("compatibility adapter must publish ready before terminal");
    let completion = receipt
        .try_take_completion()
        .expect("compatibility adapter must preserve terminal completion");
    let installed_instance_id = match completion {
        MediaInstallCompletion::Installed {
            request_id: completed_request_id,
            media_instance_id,
            ..
        } => {
            assert_eq!(completed_request_id, request_id);
            media_instance_id
        }
        other => panic!("expected installed completion, got {other:?}"),
    };

    assert_eq!(ready_phase, MediaInstallPhase::ReadyToCommit { request_id });
    assert_eq!(snapshot.media_instance_id, Some(installed_instance_id));
    assert_eq!(snapshot.playback_state, PlaybackState::Paused);
    assert_eq!(snapshot.duration, Some(Duration::from_secs(30)));

    let events = drain_events_until(&worker, |events| {
        events.iter().any(|event| {
            matches!(
                event,
                PlayerWorkerEvent::Player(CorrelatedPlayerEvent {
                    event: PlayerEvent::MediaOpened(_),
                    ..
                })
            )
        })
    });
    let media_opened_instance_id = events.iter().find_map(|event| match event {
        PlayerWorkerEvent::Player(CorrelatedPlayerEvent {
            media_instance_id,
            event: PlayerEvent::MediaOpened(_),
        }) => *media_instance_id,
        _ => None,
    });
    assert_eq!(media_opened_instance_id, Some(installed_instance_id));

    worker.shutdown().unwrap();
}

#[test]
fn compatibility_media_install_sender_preserves_backpressure_and_disconnect() {
    let (command_sender, command_rx) = command_sender_for_tests();
    for _ in 0..COMMAND_CHANNEL_CAPACITY {
        command_sender.try_send(PlayerCommand::Play).unwrap();
    }

    let full_error = command_sender
        .load_prepared_media_compatibility(
            MediaInstallRequestId::new_unique(),
            PreparedMedia::from_external_label(
                "full".to_owned(),
                Box::new(WorkerFakeDemuxer::seekable_with_tracks(
                    Vec::new(),
                    Arc::new(Mutex::new(Vec::new())),
                )),
            ),
            false,
        )
        .expect_err("full command queue must reject install acceptance");
    assert_eq!(full_error, PlayerWorkerSendError::Full);

    drop(command_rx);
    let (disconnected_sender, disconnected_rx) = command_sender_for_tests();
    drop(disconnected_rx);
    let disconnected_error = disconnected_sender
        .load_prepared_media_compatibility(
            MediaInstallRequestId::new_unique(),
            PreparedMedia::from_external_label(
                "disconnected".to_owned(),
                Box::new(WorkerFakeDemuxer::seekable_with_tracks(
                    Vec::new(),
                    Arc::new(Mutex::new(Vec::new())),
                )),
            ),
            false,
        )
        .expect_err("disconnected command queue must reject install acceptance");
    assert_eq!(disconnected_error, PlayerWorkerSendError::Disconnected);
}
