//! Валидация цветокоррекции, HDR->SDR и лимитов декодера/планировщика.

use super::*;

/// Проверяет, что render.color_adjustment defaults являются identity.
#[test]
fn render_color_adjustment_defaults_are_identity() {
    let config = AppConfig::default();

    assert!(config.render.color_adjustment.is_identity());
    assert_eq!(config.render.color_adjustment.brightness, 0.0);
    assert_eq!(config.render.color_adjustment.contrast, 1.0);
    assert_eq!(config.render.color_adjustment.saturation, 1.0);
    assert_eq!(config.render.color_adjustment.exposure, 0.0);
    assert_eq!(config.render.color_adjustment.rgb_gain, [1.0, 1.0, 1.0]);
    assert_eq!(config.render.color_adjustment.rgb_offset, [0.0, 0.0, 0.0]);
}

/// Проверяет, что render color metadata ranges являются authoritative validation.
#[test]
fn invalid_render_color_adjustment_range_fails_validation() {
    let mut config = AppConfig::default();
    config.render.color_adjustment.brightness = validation::MAX_RENDER_COLOR_BRIGHTNESS + 0.1;

    let error = config
        .validate()
        .expect_err("brightness above metadata range rejected");

    assert!(
        error
            .to_string()
            .contains("render.color_adjustment.brightness")
    );
}

/// Проверяет, что RGB channels проверяются не только по длине, но и по range.
#[test]
fn invalid_render_rgb_channel_range_fails_validation() {
    let mut config = AppConfig::default();
    config.render.color_adjustment.rgb_gain = vec![validation::MAX_RENDER_RGB_GAIN + 0.1, 1.0, 1.0];

    let error = config
        .validate()
        .expect_err("RGB gain channel above metadata range rejected");

    assert!(
        error
            .to_string()
            .contains("render.color_adjustment.rgb_gain")
    );
}

/// Проверяет documented HDR-to-SDR defaults для Phase 10.
#[test]
fn render_hdr_to_sdr_defaults_are_valid_phase10_baseline() {
    let config = AppConfig::default();

    assert!(config.render.hdr_to_sdr.enabled);
    assert_eq!(
        config.render.hdr_to_sdr.operator,
        HdrToSdrOperatorConfig::Bt2446C
    );
    assert_eq!(config.render.hdr_to_sdr.sdr_reference_white_nits, 100.0);
    assert_eq!(config.render.hdr_to_sdr.hdr_reference_peak_nits, 1_000.0);
    assert_eq!(config.render.tone_mapping, ToneMappingMode::Disabled);
}

/// Проверяет defaults текущей schema version 10.
#[test]
fn schema_version_10_defaults_include_web_media_and_existing_policies() {
    let config = AppConfig::default();

    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(CURRENT_SCHEMA_VERSION, 10);
    assert!(config.playlist.next_item_preload_enabled);
    assert_eq!(config.playlist.next_item_preload_budget_mb, 64);
    assert_eq!(config.player.seek.commit_timeout_ms, 10_000);
    assert_eq!(config.player.seek.resume_audio_min_buffer_ms, 50);
    assert_eq!(config.player.seek.resume_audio_gate_timeout_ms, 250);
    assert_eq!(config.player.seek.resume_video_min_ready_frames, 3);
    assert_eq!(config.player.seek.fast_preroll_budget_ms, 48);
    assert_eq!(config.player.seek.fast_preroll_video_packet_burst, 512);
    assert_eq!(
        config.player.seek.paused_commit_behavior,
        PausedCommitBehavior::StayPaused
    );
    assert_eq!(config.player.seek.hotkey_small_step_secs, 5);
    assert_eq!(config.player.seek.hotkey_large_step_secs, 30);
    assert_eq!(config.player.demux.max_consecutive_corrupted_packets, 64);
    assert_eq!(config.video.decoder_packet_channel_frames, 32);
    assert_eq!(config.video.decoder_frame_channel_frames, 8);
    assert_eq!(config.video.decoder_ready_queue_frames, 8);
    assert_eq!(config.video.decoder_surface_pool_frames, 24);
    assert_eq!(config.video.zero_copy_surface_pool_slots, 24);
    assert_eq!(config.video.preferred_backend, VideoBackendPreference::Auto);
    assert_eq!(config.video.scheduler.demux_packets_per_tick, 12);
    assert_eq!(config.video.scheduler.video_packets_per_tick, 8);
    assert_eq!(config.video.scheduler.decoded_frames_per_tick, 8);
    assert_eq!(config.video.scheduler.catch_up_budget_ms, 4);
    assert_eq!(config.video.scheduler.present_queue_min_frames, 2);
    assert_eq!(config.video.scheduler.present_queue_target_frames, 4);
    assert_eq!(config.video.scheduler.decode_ahead_target_ms, 250);
    assert_eq!(config.video.scheduler.surface_free_slots_min, 2);
    assert_eq!(config.video.scheduler.surface_free_slots_target, 4);
    assert_eq!(config.network.memory_cache_mb, 128);
    assert_eq!(config.network.read_ahead_mb, 256);
    assert_eq!(config.network.prefetch_initial_chunk_kb, 64);
    assert_eq!(config.network.prefetch_chunk_mb, 8);
    assert_eq!(config.network.connect_timeout_ms, 15_000);
    assert_eq!(config.network.read_timeout_ms, 15_000);
    assert_eq!(config.yt_dlp.resolve_timeout_ms, 30_000);
    assert_eq!(config.web_media.preferred_video_height, None);
    assert!(config.web_media.vod_endpoint_recovery_enabled);
    assert_eq!(
        config
            .web_media
            .vod_endpoint_recovery_max_consecutive_attempts,
        3
    );
    assert_eq!(
        config.web_media.vod_endpoint_recovery_initial_backoff_ms,
        250
    );
    assert_eq!(config.web_media.vod_endpoint_recovery_max_backoff_ms, 2_000);
    assert_eq!(
        config.web_media.vod_endpoint_recovery_stable_reset_ms,
        30_000
    );
    assert_eq!(config.ui.skin, "minimal");
    assert_eq!(config.ui.window.titlebar_height_px, 40);
    assert_eq!(config.ui.window.corner_radius_px, 12);
    assert_eq!(config.ui.settings.live_preview_max_hz, 60);
}

/// Проверяет, что demux skip-window не может быть нулевым.
#[test]
fn invalid_demux_corrupted_packet_limit_fails_validation() {
    let mut config = AppConfig::default();
    config.player.demux.max_consecutive_corrupted_packets = 0;

    let error = config
        .validate()
        .expect_err("zero demux corrupted packet limit rejected");

    assert!(
        error
            .to_string()
            .contains("player.demux.max_consecutive_corrupted_packets")
    );
}

/// Проверяет, что decoder queues остаются bounded и не могут быть нулевыми.
#[test]
fn invalid_decoder_queue_limit_fails_validation() {
    let mut config = AppConfig::default();
    config.video.decoder_packet_channel_frames = 0;

    let error = config
        .validate()
        .expect_err("zero decoder packet channel rejected");

    assert!(
        error
            .to_string()
            .contains("video.decoder_packet_channel_frames")
    );
}

/// Проверяет, что scheduler budget не может быть нулевым.
#[test]
fn invalid_scheduler_budget_fails_validation() {
    let mut config = AppConfig::default();
    config.video.scheduler.demux_packets_per_tick = 0;

    let error = config
        .validate()
        .expect_err("zero scheduler demux budget rejected");

    assert!(
        error
            .to_string()
            .contains("video.scheduler.demux_packets_per_tick")
    );
}

/// Проверяет cross-field min/target/max для presentation queue.
#[test]
fn invalid_scheduler_present_queue_target_fails_validation() {
    let mut config = AppConfig::default();
    config.video.present_queue_frames = 4;
    config.video.scheduler.present_queue_min_frames = 3;
    config.video.scheduler.present_queue_target_frames = 5;

    let error = config
        .validate()
        .expect_err("present queue target above max rejected");

    assert!(
        error
            .to_string()
            .contains("video.scheduler.present_queue_target_frames")
    );
}

/// Проверяет, что decode-ahead target не может превышать max.
#[test]
fn invalid_scheduler_decode_ahead_target_fails_validation() {
    let mut config = AppConfig::default();
    config.video.max_decode_ahead_ms = 100;
    config.video.scheduler.decode_ahead_target_ms = 200;

    let error = config
        .validate()
        .expect_err("decode ahead target above max rejected");

    assert!(
        error
            .to_string()
            .contains("video.scheduler.decode_ahead_target_ms")
    );
}
