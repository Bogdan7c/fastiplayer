//! Consumer-проверка накопленного intent до выбора worker wake.

use super::*;

#[test]
fn queued_playback_intent_is_drained_and_publishes_exact_paused_snapshot() {
    let (mut runtime, _command_sender, _shutdown_sender, _render_bridge_client) =
        runtime_for_tests_with_wakeup_handles(Instant::now());
    let seek_request_log = Arc::new(Mutex::new(Vec::new()));
    install_worker_video_media(&mut runtime, seek_request_log);

    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::Play));
    assert_eq!(
        runtime.session.snapshot().playback_state,
        PlaybackState::Playing
    );

    let request_id = MediaInstallRequestId::new_unique();
    let media_instance_id = runtime
        .session
        .snapshot()
        .media_instance_id
        .expect("worker fake media must have an exact instance identity");
    let initial_revision = PlaybackIntentRevision::from_non_zero(
        NonZeroU64::new(1).expect("playback intent revision is non-zero"),
    );
    runtime.playback_intent_control.register_staged_request(
        request_id,
        AcceptedPlaybackIntent {
            revision: initial_revision,
            intent: PlaybackIntent::StartPlaying,
        },
    );
    runtime
        .playback_intent_control
        .commit_staged_request(request_id, media_instance_id, |_| {});

    let pause_revision = PlaybackIntentRevision::from_non_zero(
        NonZeroU64::new(2).expect("playback intent revision is non-zero"),
    );
    let submitted = runtime
        .playback_intent_control
        .submit_update(PlaybackIntentUpdate {
            request_id,
            revision: pause_revision,
            intent: PlaybackIntent::StartPaused,
        });
    assert!(submitted.wake_player_owner);
    assert_eq!(submitted.receipt.try_outcome(), None);

    let published_snapshot_rx = runtime
        .snapshot_publisher
        .snapshot_rx_for_drain_latest
        .clone();
    runtime
        ._playback_intent_wake_tx_guard
        .try_send(())
        .expect("coalesced playback intent wake channel must accept the first wake");

    // Intent уже в очереди до входа owner-а: drain обязан поглотить coalesced wake.
    runtime.drain_playback_intent_updates();
    assert!(runtime.playback_intent_wake_rx.is_empty());
    assert_eq!(
        submitted.receipt.wait_for_outcome(),
        PlaybackIntentUpdateOutcome::AppliedToInstalled { media_instance_id }
    );
    assert_eq!(
        runtime.session.snapshot().playback_state,
        PlaybackState::Paused
    );
    let published_snapshot = published_snapshot_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("playback intent wake must publish the consumer-visible snapshot");
    assert_eq!(
        published_snapshot.media_instance_id,
        Some(media_instance_id)
    );
    assert_eq!(published_snapshot.playback_state, PlaybackState::Paused);
}
