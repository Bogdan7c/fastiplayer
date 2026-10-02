//! Пробуждения worker-а и применение playback intent.

use super::*;

#[test]
fn playback_intent_wakeup_applies_exact_installed_update_and_publishes_snapshot() {
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

    assert!(!runtime.wait_for_worker_wakeup_until_event(None));
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

#[test]
fn idle_worker_has_no_periodic_wakeup_timeout() {
    let runtime = runtime_for_tests(Instant::now());

    assert!(runtime.plan_next_worker_wakeup().is_none());
}

#[test]
fn active_worker_uses_media_plan_as_wakeup_timeout() {
    let mut runtime = runtime_for_tests(Instant::now());

    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::Play));

    assert!(runtime.plan_next_worker_wakeup().is_some());
}

/// Уже просроченный plan исполняется сразу и не входит в blocking wait.
#[test]
fn overdue_worker_wakeup_runs_tick_without_blocking_wait() {
    // Нулевой coarse interval создаёт гарантированно просроченный playback deadline.
    let mut runtime = runtime_for_tests(Instant::now());
    // Production planner обязан сохранить immediate-work семантику нулевой задержки.
    runtime.config.coarse_wakeup_interval = Duration::ZERO;
    // Playing intent включает media scheduling без отдельного test-only seam-а.
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::Play));
    // Предыдущее время позволяет доказать реальный tick, а не только возврат `false`.
    let previous_tick_at = runtime.last_tick_at;

    // Exact zero-timeout branch не блокируется на channel/select boundary.
    let shutdown_requested = runtime.wait_for_worker_wakeup();

    // Immediate playback work не является shutdown.
    assert!(!shutdown_requested);
    // Worker исполнил overdue tick синхронно до возврата.
    assert!(runtime.last_tick_at > previous_tick_at);
}

#[test]
fn command_batch_yields_to_overdue_tick_during_command_storm() {
    let (mut runtime, command_tx) = runtime_for_tests_with_command_sender(Instant::now());
    runtime.config.coarse_wakeup_interval = Duration::ZERO;
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::Play));

    for command_index in 0..MAX_COMMANDS_PER_LOOP * 2 {
        command_tx
            .try_send(WorkerCommand::SetSystemCapabilities(
                SystemCapabilities::empty(command_index as u64),
            ))
            .unwrap();
    }

    let previous_tick_at = runtime.last_tick_at;
    let processed_commands = runtime.drain_pending_command_batch();
    runtime.service_worker_fairness_checkpoint(processed_commands);

    assert_eq!(processed_commands, MAX_COMMANDS_PER_LOOP);
    assert_eq!(runtime.command_rx.len(), MAX_COMMANDS_PER_LOOP);
    assert!(runtime.last_tick_at > previous_tick_at);
}

#[test]
fn active_accurate_preroll_with_full_decoder_queue_parks_until_activity() {
    let (activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let (mut runtime, _command_tx, _shutdown_tx, _render_client) =
        runtime_for_tests_with_wakeup_handles(Instant::now());
    runtime.config.decoder_readiness_poll_interval = Duration::from_millis(150);
    install_active_decoder_activity_preroll(&mut runtime, activity_subscription.snapshot());
    let wait_plan = planned_decoder_activity_wait(&mut runtime);
    let previous_tick_at = runtime.last_tick_at;

    let notifier_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        let _ = activity_notifier.notify_activity();
    });
    let wait_started_at = Instant::now();
    let shutdown_requested = runtime.wait_for_worker_wakeup_with_timeout(wait_plan);
    let waited_for = wait_started_at.elapsed();

    notifier_thread
        .join()
        .expect("activity notifier thread should finish");
    assert!(!shutdown_requested);
    assert!(runtime.last_tick_at > previous_tick_at);
    assert!(
        waited_for < Duration::from_millis(100),
        "worker should wake from decoder activity before fallback timeout, waited {waited_for:?}"
    );
}

#[test]
fn command_wakeup_wins_over_decoder_activity() {
    let (activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let (mut runtime, command_tx) = runtime_for_tests_with_command_sender(Instant::now());
    runtime.config.decoder_readiness_poll_interval = Duration::from_millis(100);
    install_active_decoder_activity_preroll(&mut runtime, activity_subscription.snapshot());
    let wait_plan = planned_decoder_activity_wait(&mut runtime);
    let previous_tick_at = runtime.last_tick_at;

    command_tx
        .try_send(WorkerCommand::SetSystemCapabilities(
            SystemCapabilities::empty(7),
        ))
        .expect("test command queue should accept command");
    let _ = activity_notifier.notify_activity();
    let shutdown_requested = runtime.wait_for_worker_wakeup_with_timeout(wait_plan);

    assert!(!shutdown_requested);
    assert_eq!(
        runtime.last_tick_at, previous_tick_at,
        "biased select must process command before simultaneous decoder activity"
    );
}

#[test]
fn render_feedback_does_not_postpone_playback_timeout() {
    let (mut runtime, _command_tx, _shutdown_tx, render_client) =
        runtime_for_tests_with_wakeup_handles(Instant::now());
    runtime.config.coarse_wakeup_interval = Duration::from_millis(5);
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::Play));
    let wakeup = runtime
        .plan_next_worker_wakeup()
        .expect("active playback should plan a worker wakeup");
    assert!(
        !wakeup.timeout().is_zero(),
        "test must exercise a delayed playback deadline"
    );
    let previous_tick_at = runtime.last_tick_at;

    render_client.report_gpu_submit_present_latency(Duration::from_millis(1));
    let wait_started_at = Instant::now();
    let shutdown_requested = runtime.wait_for_worker_wakeup_with_timeout(PlannedWorkerWait {
        wakeup,
        decoder_activity: None,
        timeline_activity: None,
    });
    let waited_for = wait_started_at.elapsed();

    assert!(!shutdown_requested);
    assert!(runtime.last_tick_at > previous_tick_at);
    assert!(
        waited_for < Duration::from_millis(50),
        "render feedback must not slide the original playback deadline, waited {waited_for:?}"
    );
}

#[test]
fn disconnected_and_fatal_decoder_activity_notifiers_do_not_tight_loop() {
    let (activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let (mut disconnected_runtime, _command_tx, _shutdown_tx, _render_client) =
        runtime_for_tests_with_wakeup_handles(Instant::now());
    disconnected_runtime.config.decoder_readiness_poll_interval = Duration::from_millis(20);
    install_active_decoder_activity_preroll(
        &mut disconnected_runtime,
        activity_subscription.snapshot(),
    );
    let disconnected_wait = planned_decoder_activity_wait(&mut disconnected_runtime);
    let disconnected_previous_tick_at = disconnected_runtime.last_tick_at;
    drop(activity_notifier);

    let disconnected_wait_started_at = Instant::now();
    let shutdown_requested =
        disconnected_runtime.wait_for_worker_wakeup_with_timeout(disconnected_wait);
    let disconnected_waited_for = disconnected_wait_started_at.elapsed();

    assert!(!shutdown_requested);
    assert!(disconnected_runtime.last_tick_at > disconnected_previous_tick_at);
    assert!(
        disconnected_waited_for >= Duration::from_millis(10),
        "disconnected activity receiver must fall back to bounded poll, waited {disconnected_waited_for:?}"
    );

    let (mut fatal_runtime, _command_tx, _shutdown_tx, _render_client) =
        runtime_for_tests_with_wakeup_handles(Instant::now());
    fatal_runtime.config.decoder_readiness_poll_interval = Duration::from_millis(20);
    install_active_decoder_activity_preroll(
        &mut fatal_runtime,
        VideoDecoderActivitySnapshot::unavailable(
            VideoDecoderActivityUnavailableReason::FatalNotifier(
                video_core::DecodeThreadError::new("worker activity fatal"),
            ),
        ),
    );
    let fatal_wait = fatal_runtime
        .plan_next_worker_wakeup_with_decoder_activity()
        .expect("fatal notifier should still use bounded fallback wakeup");
    let WorkerWakeupDeadline::Playback { plan, .. } = fatal_wait.deadline();
    assert_eq!(plan.reason, crate::WorkerWakeupReason::DecodeReadiness);
    assert!(!plan.wait_for_decoder_activity);
    assert!(fatal_wait.decoder_activity.is_none());
    let fatal_previous_tick_at = fatal_runtime.last_tick_at;

    let fatal_wait_started_at = Instant::now();
    let shutdown_requested = fatal_runtime.wait_for_worker_wakeup_with_timeout(fatal_wait);
    let fatal_waited_for = fatal_wait_started_at.elapsed();

    assert!(!shutdown_requested);
    assert!(fatal_runtime.last_tick_at > fatal_previous_tick_at);
    assert!(
        fatal_waited_for >= Duration::from_millis(10),
        "fatal activity notifier must fall back to bounded poll, waited {fatal_waited_for:?}"
    );
}

#[test]
fn lost_decoder_activity_between_planning_and_select_wakes_without_full_fallback() {
    let (activity_notifier, activity_subscription) =
        video_core::VideoDecoderActivityNotifier::new();
    let (mut runtime, _command_tx, _shutdown_tx, _render_client) =
        runtime_for_tests_with_wakeup_handles(Instant::now());
    runtime.config.decoder_readiness_poll_interval = Duration::from_millis(150);
    install_active_decoder_activity_preroll(&mut runtime, activity_subscription.snapshot());
    let wait_plan = planned_decoder_activity_wait(&mut runtime);
    let previous_tick_at = runtime.last_tick_at;

    let _ = activity_notifier.notify_activity();
    let wait_started_at = Instant::now();
    let shutdown_requested = runtime.wait_for_worker_wakeup_with_timeout(wait_plan);
    let waited_for = wait_started_at.elapsed();

    assert!(!shutdown_requested);
    assert!(runtime.last_tick_at > previous_tick_at);
    assert!(
        waited_for < Duration::from_millis(30),
        "pre-select activity_since check should close the lost-wakeup window, waited {waited_for:?}"
    );
}

#[test]
fn render_release_ack_is_drained_before_latest_publish() {
    let mut runtime = runtime_for_tests(Instant::now());
    runtime
        .session
        .register_render_lease(0, video_core::FrameResourceHandle(7));
    runtime
        .render_bridge
        .release_sender_for_tests()
        .try_send(RenderLeaseRelease {
            render_generation: 0,
            resource_handle: video_core::FrameResourceHandle(7),
            resource_provider: None,
            submitted_to_renderer: false,
            released_at: Instant::now(),
        })
        .unwrap();

    runtime
        .render_bridge
        .publish_latest_present_frame(&mut runtime.session);

    assert_eq!(runtime.session.render_lease_count(), 0);
    assert!(matches!(
        runtime.render_bridge.try_clone_latest_for_tests(),
        LatestPresentFrameAcquire::Empty
    ));
}

#[test]
fn latest_present_frame_handoff_reuses_one_drop_ack_until_replaced() {
    let handoff = LatestPresentFrameHandoff::new();
    let (release_tx, release_rx) = unbounded();
    let first_frame =
        present_frame_lease_for_tests(2, FrameResourceHandle(12), false, release_tx.clone());
    let second_frame = present_frame_lease_for_tests(2, FrameResourceHandle(13), false, release_tx);

    handoff.publish(Some(first_frame));
    let first_render_clone = match handoff.try_clone_latest() {
        LatestPresentFrameAcquire::Acquired(frame) => frame,
        LatestPresentFrameAcquire::Empty | LatestPresentFrameAcquire::Busy => {
            panic!("latest frame should be available")
        }
    };
    let repeated_render_clone = match handoff.try_clone_latest() {
        LatestPresentFrameAcquire::Acquired(frame) => frame,
        LatestPresentFrameAcquire::Empty | LatestPresentFrameAcquire::Busy => {
            panic!("latest frame should be reusable")
        }
    };

    drop(first_render_clone);
    drop(repeated_render_clone);
    assert!(release_rx.try_recv().is_err());

    handoff.publish(Some(second_frame));
    let release = release_rx.try_recv().unwrap();
    assert_eq!(release.render_generation, 2);
    assert_eq!(release.resource_handle, FrameResourceHandle(12));
    assert!(release_rx.try_recv().is_err());
}

#[test]
fn latest_present_frame_handoff_keeps_generation_safe_stale_identity() {
    let handoff = LatestPresentFrameHandoff::new();
    let (release_tx, release_rx) = unbounded();
    let old_generation_frame =
        present_frame_lease_for_tests(4, FrameResourceHandle(31), false, release_tx);

    handoff.publish(Some(old_generation_frame));
    let acquired_frame = match handoff.try_clone_latest() {
        LatestPresentFrameAcquire::Acquired(frame) => frame,
        LatestPresentFrameAcquire::Empty | LatestPresentFrameAcquire::Busy => {
            panic!("old generation frame should be observable as stale")
        }
    };

    assert!(acquired_frame.stale_for_generation(5));

    drop(acquired_frame);
    handoff.clear();
    let release = release_rx.try_recv().unwrap();
    assert_eq!(release.render_generation, 4);
    assert_eq!(release.resource_handle, FrameResourceHandle(31));
}

#[test]
fn player_worker_try_acquire_present_frame_reads_latest_slot_without_reply_wait() {
    let latest_present_frame_handoff = Arc::new(LatestPresentFrameHandoff::new());
    let (release_tx, _release_rx) = unbounded();
    let expected_resource_handle = FrameResourceHandle(44);
    let frame =
        present_frame_lease_for_tests(3, expected_resource_handle, false, release_tx.clone());
    latest_present_frame_handoff.publish(Some(frame));
    let (
        worker,
        render_acquire_sample_rx,
        _render_timing_sample_rx,
        _render_resource_previous_frame_reuse_sample_rx,
    ) = worker_with_latest_handoff_for_tests(Arc::clone(&latest_present_frame_handoff));

    let acquired_frame = worker.try_acquire_present_frame().unwrap();

    assert_eq!(acquired_frame.render_generation(), 3);
    assert_eq!(acquired_frame.resource_handle(), expected_resource_handle);
    assert!(render_acquire_sample_rx.try_recv().is_ok());
}

#[test]
fn player_worker_scrub_visual_override_handoff_stays_separate_from_playback_slot() {
    let playback_handoff = Arc::new(LatestPresentFrameHandoff::new());
    let scrub_override_handoff = Arc::new(LatestPresentFrameHandoff::new());
    let (release_tx, _release_rx) = unbounded();
    let playback_handle = FrameResourceHandle(44);
    let scrub_override_handle = FrameResourceHandle(45);
    playback_handoff.publish(Some(present_frame_lease_for_tests(
        3,
        playback_handle,
        false,
        release_tx.clone(),
    )));
    scrub_override_handoff.publish(Some(present_frame_lease_for_tests(
        3,
        scrub_override_handle,
        false,
        release_tx,
    )));
    let (
        worker,
        _render_acquire_sample_rx,
        _render_timing_sample_rx,
        _render_resource_previous_frame_reuse_sample_rx,
    ) = worker_with_latest_handoffs_for_tests(playback_handoff, scrub_override_handoff);

    let playback_frame = worker.try_acquire_present_frame().unwrap();
    let scrub_override_frame = worker.try_acquire_scrub_visual_override_frame().unwrap();

    assert_eq!(playback_frame.resource_handle(), playback_handle);
    assert_eq!(
        scrub_override_frame.resource_handle(),
        scrub_override_handle
    );
}

#[test]
fn player_worker_reports_gpu_submit_present_latency_without_command_queue() {
    let latest_present_frame_handoff = Arc::new(LatestPresentFrameHandoff::new());
    let (
        worker,
        _render_acquire_sample_rx,
        render_timing_sample_rx,
        _render_resource_previous_frame_reuse_sample_rx,
    ) = worker_with_latest_handoff_for_tests(latest_present_frame_handoff);

    worker.report_gpu_submit_present_latency(Duration::from_millis(1));

    let sample = render_timing_sample_rx
        .try_recv()
        .expect("render timing sample should be queued");
    assert_eq!(sample.submit_present_elapsed, Duration::from_millis(1));
}

#[test]
fn player_worker_reports_render_resource_previous_frame_reuse_without_command_queue() {
    let latest_present_frame_handoff = Arc::new(LatestPresentFrameHandoff::new());
    let (
        worker,
        _render_acquire_sample_rx,
        _render_timing_sample_rx,
        render_resource_previous_frame_reuse_sample_rx,
    ) = worker_with_latest_handoff_for_tests(latest_present_frame_handoff);

    worker.report_render_resource_previous_frame_reuse();

    render_resource_previous_frame_reuse_sample_rx
        .try_recv()
        .expect("render resource previous-frame reuse sample should be queued");
}

#[test]
fn tick_runs_while_render_lease_is_active() {
    let mut runtime = runtime_for_tests(Instant::now());
    runtime
        .session
        .register_render_lease(0, video_core::FrameResourceHandle(11));
    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::Play));
    let previous_tick_at = runtime.last_tick_at;
    let plan = runtime.session.worker_wakeup_plan(
        Instant::now(),
        &runtime.config.tick_config,
        runtime.config.decoder_readiness_poll_interval,
        runtime.config.coarse_wakeup_interval,
    );

    runtime.run_tick_for_wakeup_plan(plan, Instant::now());

    assert!(runtime.last_tick_at > previous_tick_at);
}

#[test]
fn present_frame_lease_drop_releases_frame_exactly_once() {
    let (release_tx, release_rx) = unbounded();
    let lease =
        present_frame_lease_for_tests(2, FrameResourceHandle(12), false, release_tx.clone());
    let lease_clone = lease.clone();

    drop(lease);
    assert!(release_rx.try_recv().is_err());

    drop(lease_clone);
    let release = release_rx.try_recv().unwrap();

    assert_eq!(release.render_generation, 2);
    assert_eq!(release.resource_handle, FrameResourceHandle(12));
    assert!(release_rx.try_recv().is_err());
}

#[test]
fn present_frame_lease_drop_times_out_when_release_queue_stays_full() {
    let (release_tx, release_rx) = bounded(1);
    release_tx
        .try_send(RenderLeaseRelease {
            render_generation: 1,
            resource_handle: FrameResourceHandle(1),
            resource_provider: None,
            submitted_to_renderer: false,
            released_at: Instant::now(),
        })
        .unwrap();
    let lease = present_frame_lease_for_tests(2, FrameResourceHandle(12), false, release_tx);
    let drop_started_at = Instant::now();

    drop(lease);

    assert!(drop_started_at.elapsed() < Duration::from_secs(1));
    assert_eq!(release_rx.len(), 1);
    let queued_release = release_rx.try_recv().unwrap();
    assert_eq!(queued_release.render_generation, 1);
    assert_eq!(queued_release.resource_handle, FrameResourceHandle(1));
}

#[test]
fn leased_frame_release_is_deferred_until_renderer_drops_lease() {
    let mut runtime = runtime_for_tests(Instant::now());
    let resource_handle = FrameResourceHandle(21);

    assert!(runtime.session.register_render_lease(0, resource_handle));
    runtime.session.release_video_texture(resource_handle);

    assert_eq!(runtime.session.render_lease_count(), 1);
    assert!(
        runtime
            .session
            .has_deferred_video_texture_release(resource_handle)
    );

    runtime.session.release_render_lease(0, resource_handle);

    assert_eq!(runtime.session.render_lease_count(), 0);
    assert_eq!(runtime.session.deferred_video_texture_release_count(), 0);
}

#[test]
fn new_generation_makes_old_lease_stale_without_dropping_it() {
    let (release_tx, release_rx) = unbounded();
    let lease = present_frame_lease_for_tests(4, FrameResourceHandle(31), false, release_tx);

    assert!(lease.stale_for_generation(5));
    assert!(release_rx.try_recv().is_err());

    drop(lease);

    let release = release_rx.try_recv().unwrap();
    assert_eq!(release.render_generation, 4);
    assert_eq!(release.resource_handle, FrameResourceHandle(31));
}

#[test]
fn render_error_command_updates_player_error_snapshot() {
    let mut runtime = runtime_for_tests(Instant::now());
    let render_error = PlayerRenderError {
        kind: PlayerRenderErrorKind::MissingRenderResources,
        render_generation: Some(6),
        frame_handle: Some(42),
        message: "missing Y/UV views for test frame".into(),
    };

    runtime.handle_worker_command(WorkerCommand::RenderError(render_error));

    let snapshot_error = runtime.session.snapshot().last_error.as_ref().unwrap();
    assert_eq!(
        snapshot_error.kind,
        PlayerErrorKind::UnsupportedRenderFormat
    );
    assert!(
        snapshot_error
            .message
            .contains("missing Y/UV views for test frame")
    );
    assert_eq!(runtime.session.playback_state(), PlaybackState::Failed);
    assert!(
        runtime
            .session
            .take_events()
            .iter()
            .any(|event| matches!(event, PlayerEvent::FatalError(error)
                if error.kind == PlayerErrorKind::UnsupportedRenderFormat))
    );
}

/// Persisted config и frame-server-core держат свои границы live_scrub_max_hz
/// отдельными константами; маппинг обязан принимать обе границы валидного
/// persisted диапазона, иначе frame_server_config_from_app_config упадёт.
#[test]
fn frame_server_mapping_accepts_persisted_live_scrub_rate_bounds() {
    for live_scrub_max_hz in [1, 240] {
        let mut config = fastiplayer_config::AppConfig::default();
        config.frame_server.live_scrub_max_hz = live_scrub_max_hz;
        assert!(
            config.validate().is_ok(),
            "{live_scrub_max_hz} Hz is a valid persisted value"
        );

        let mapped = PlayerWorkerConfig::frame_server_config_from_app_config(&config);

        assert_eq!(mapped.live_scrub_max_hz(), live_scrub_max_hz);
    }
}
