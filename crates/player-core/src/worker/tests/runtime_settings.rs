//! Применение runtime-настроек и rebuild-ов, отчёты и backpressure.

use super::*;

#[test]
fn player_worker_exposes_decoder_thread_config_for_backend_factory() {
    let decoder_thread_config = PlayerVideoDecoderThreadConfig {
        packet_channel_frames: 2,
        frame_channel_frames: 3,
        control_channel_frames: 4,
        decoder_ready_queue_frames: 5,
        decoder_surface_pool_frames: 6,
        software_frame_pool_frames: 8,
        software_decode_thread_budget: video_core::SoftwareDecodeThreadBudget::auto(),
        zero_copy_surface_pool_slots: 7,
        flush_timeout: Duration::from_millis(75),
    };
    let mut config = worker_config_for_tests();
    config.decoder_thread_config = decoder_thread_config;

    let mut worker = PlayerWorker::spawn(config).unwrap();

    assert_eq!(worker.decoder_thread_config(), decoder_thread_config);
    worker.shutdown().unwrap();
}

#[test]
fn decoder_thread_config_maps_software_surface_pool_independently() {
    // sw_decoder_surface_pool_frames должен попадать именно в software_frame_pool_frames,
    // не затрагивая hardware decoder_surface_pool_frames.
    let mut config = fastiplayer_config::AppConfig::default();
    config.video.decoder_surface_pool_frames = 24;
    config.video.sw_decoder_surface_pool_frames = 6;

    let thread_config = PlayerWorkerConfig::decoder_thread_config_from_app_config(&config);

    assert_eq!(thread_config.software_frame_pool_frames, 6);
    assert_eq!(thread_config.decoder_surface_pool_frames, 24);
}

#[test]
fn runtime_apply_tick_config_updates_worker_owned_config() {
    let mut runtime = runtime_for_tests(Instant::now());
    let mut tick_config = runtime.config.tick_config;
    tick_config.max_demux_packets_per_tick += 1;

    let report =
        runtime.apply_runtime_settings(PlayerRuntimeSettingsUpdate::empty().with_tick_config(
            tick_config,
            [PlayerRuntimeSettingId::VideoSchedulerDemuxPacketsPerTick],
        ));

    assert_eq!(runtime.config.tick_config, tick_config);
    let tick_report = apply_group_report(&report, PlayerRuntimeApplyGroup::TickConfig);
    assert_eq!(
        tick_report.outcome,
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Applied)
    );
    assert_eq!(
        tick_report.affected_settings,
        vec![PlayerRuntimeSettingId::VideoSchedulerDemuxPacketsPerTick]
    );
}

#[test]
fn runtime_apply_frame_server_policy_updates_worker_and_session_owned_config() {
    let mut runtime = runtime_for_tests(Instant::now());
    let requested_frame_server_config = frame_server_core::FrameServerConfig {
        live_scrub_max_hz: 120,
        ..frame_server_core::FrameServerConfig::default()
    }
    .validate()
    .expect("test frame-server policy must validate");

    let report = runtime.apply_runtime_settings(
        PlayerRuntimeSettingsUpdate::empty().with_frame_server_policy(
            requested_frame_server_config,
            [PlayerRuntimeSettingId::FrameServerLiveScrubMaxHz],
        ),
    );

    assert_eq!(
        runtime.config.frame_server_config,
        requested_frame_server_config
    );
    assert_eq!(
        runtime.session.frame_server_policy_config(),
        requested_frame_server_config
    );
    let frame_server_report =
        apply_group_report(&report, PlayerRuntimeApplyGroup::FrameServerPolicy);
    assert_eq!(
        frame_server_report.outcome,
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Applied)
    );
    assert_eq!(
        frame_server_report.affected_settings,
        vec![PlayerRuntimeSettingId::FrameServerLiveScrubMaxHz]
    );
}

#[test]
fn runtime_apply_default_volume_does_not_mutate_current_playback_volume() {
    let mut runtime = runtime_for_tests(Instant::now());
    runtime
        .session
        .dispatch_command(PlayerCommand::SetVolume(0.25))
        .unwrap();

    let report = runtime.apply_runtime_settings(
        PlayerRuntimeSettingsUpdate::empty()
            .with_default_volume(0.75, [PlayerRuntimeSettingId::AudioDefaultVolume]),
    );

    assert_eq!(runtime.config.default_volume, 0.75);
    assert_eq!(runtime.session.snapshot().volume, 0.25);
    let volume_report = apply_group_report(&report, PlayerRuntimeApplyGroup::DefaultVolume);
    assert_eq!(
        volume_report.outcome,
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Applied)
    );
}

#[test]
fn runtime_audio_output_recreate_without_active_media_updates_no_unrelated_owner() {
    let mut runtime = runtime_for_tests(Instant::now());
    let original_tick_config = runtime.config.tick_config;
    let report = runtime.apply_runtime_settings(
        PlayerRuntimeSettingsUpdate::empty()
            .with_audio_output_recreate([PlayerRuntimeSettingId::AudioOutputDevice]),
    );

    assert_eq!(runtime.config.tick_config, original_tick_config);
    let audio_report = apply_group_report(&report, PlayerRuntimeApplyGroup::AudioOutput);
    assert_eq!(
        audio_report.outcome,
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Unchanged)
    );
}

#[test]
fn worker_playback_rate_reject_stays_non_fatal_on_direct_command_path() {
    let mut runtime = runtime_for_tests(Instant::now());
    let requested_rate =
        PlaybackRate::new(1.5).expect("worker playback-rate test value must validate");

    runtime.handle_worker_command(WorkerCommand::Player(PlayerCommand::SetPlaybackRate(
        requested_rate,
    )));

    assert_eq!(
        runtime.session.snapshot().playback_state,
        PlaybackState::Idle
    );
    assert_eq!(
        runtime.session.snapshot().playback_rate,
        PlaybackRate::NORMAL
    );
    assert!(runtime.session.snapshot().last_error.is_none());
    assert!(
        !runtime
            .session
            .take_events()
            .iter()
            .any(|event| matches!(event, PlayerEvent::FatalError(_)))
    );
}

#[test]
fn runtime_apply_invalid_settings_are_reported_without_mutation() {
    let mut runtime = runtime_for_tests(Instant::now());
    let original_tick_config = runtime.config.tick_config;
    let mut invalid_tick_config = original_tick_config;
    invalid_tick_config.max_demux_packets_per_tick = 0;

    let report =
        runtime.apply_runtime_settings(PlayerRuntimeSettingsUpdate::empty().with_tick_config(
            invalid_tick_config,
            [PlayerRuntimeSettingId::VideoSchedulerDemuxPacketsPerTick],
        ));

    assert_eq!(runtime.config.tick_config, original_tick_config);
    let tick_report = apply_group_report(&report, PlayerRuntimeApplyGroup::TickConfig);
    assert_eq!(tick_report.outcome, PlayerRuntimeApplyOutcome::Invalid);
}

#[test]
fn runtime_apply_decoder_thread_config_accepts_controlled_rebuild() {
    let mut runtime = runtime_for_tests(Instant::now());
    let original_decoder_thread_config = runtime.config.decoder_thread_config;
    let requested_decoder_thread_config = PlayerVideoDecoderThreadConfig {
        packet_channel_frames: original_decoder_thread_config.packet_channel_frames + 1,
        ..original_decoder_thread_config
    };

    let report = runtime.apply_runtime_settings(
        PlayerRuntimeSettingsUpdate::empty().with_decoder_thread_config(
            requested_decoder_thread_config,
            [PlayerRuntimeSettingId::VideoDecoderPacketChannelFrames],
        ),
    );

    assert_eq!(
        runtime.config.decoder_thread_config,
        requested_decoder_thread_config
    );
    let decoder_report = apply_group_report(&report, PlayerRuntimeApplyGroup::DecoderThreadConfig);
    assert_eq!(
        decoder_report.outcome,
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Applied)
    );
}

#[test]
fn runtime_pipeline_reconfigure_returns_scrub_busy_before_any_owner_mutation() {
    let mut runtime = runtime_for_tests(Instant::now());
    let original_decoder_thread_config = runtime.config.decoder_thread_config;
    let original_default_volume = runtime.config.default_volume;
    runtime.session.set_simple_scrub_state_for_tests(
        true,
        Some(SeekRequest::absolute(MediaTime::from_secs(3))),
    );

    let requested_decoder_thread_config = PlayerVideoDecoderThreadConfig {
        packet_channel_frames: original_decoder_thread_config.packet_channel_frames + 1,
        ..original_decoder_thread_config
    };
    let report = runtime.apply_runtime_settings(
        PlayerRuntimeSettingsUpdate::empty()
            .with_default_volume(
                (original_default_volume - 0.1).max(0.0),
                [PlayerRuntimeSettingId::AudioDefaultVolume],
            )
            .with_decoder_thread_config(
                requested_decoder_thread_config,
                [PlayerRuntimeSettingId::VideoDecoderPacketChannelFrames],
            ),
    );

    assert_eq!(
        runtime.config.decoder_thread_config,
        original_decoder_thread_config
    );
    assert_eq!(runtime.config.default_volume, original_default_volume);
    assert_eq!(report.groups.len(), 1);
    assert_eq!(report.groups[0].group, PlayerRuntimeApplyGroup::Request);
    assert_eq!(
        report.groups[0].outcome,
        PlayerRuntimeApplyOutcome::RuntimeBusy(PlayerRuntimeBoundaryActivity::Scrub)
    );
}

#[test]
fn worker_apply_runtime_settings_command_sends_real_report_response() {
    let mut runtime = runtime_for_tests(Instant::now());
    let (response_tx, response_rx) = bounded(1);

    runtime.handle_worker_command(WorkerCommand::ApplyRuntimeSettings {
        update: Box::new(
            PlayerRuntimeSettingsUpdate::empty()
                .with_default_volume(0.5, [PlayerRuntimeSettingId::AudioDefaultVolume]),
        ),
        response_tx,
    });

    let report = response_rx.recv().unwrap();
    let volume_report = apply_group_report(&report, PlayerRuntimeApplyGroup::DefaultVolume);
    assert_eq!(
        volume_report.outcome,
        PlayerRuntimeApplyOutcome::Accepted(PlayerRuntimeAcceptedChange::Applied)
    );
}

#[test]
fn apply_runtime_settings_sender_distinguishes_backpressure_and_disconnected() {
    let (full_command_tx, _full_command_rx) = bounded(1);
    let full_command_sender = PlayerCommandSender::for_tests(full_command_tx).0;
    full_command_sender.try_send(PlayerCommand::Play).unwrap();

    let update = PlayerRuntimeSettingsUpdate::empty()
        .with_default_volume(0.5, [PlayerRuntimeSettingId::AudioDefaultVolume]);
    let full_result = full_command_sender.apply_runtime_settings(update.clone());

    assert_eq!(full_result, Err(PlayerRuntimeApplyError::Backpressure));

    let (disconnected_command_tx, disconnected_command_rx) = bounded(1);
    drop(disconnected_command_rx);
    let disconnected_command_sender = PlayerCommandSender::for_tests(disconnected_command_tx).0;

    let disconnected_result = disconnected_command_sender.apply_runtime_settings(update);

    assert_eq!(
        disconnected_result,
        Err(PlayerRuntimeApplyError::Disconnected)
    );
}
