//! Валидация цветокоррекции, HDR->SDR и лимитов декодера/планировщика.

use super::*;

/// Проверяет, что render.color_adjustment defaults являются identity.
#[test]
fn render_color_adjustment_defaults_are_identity() {
    let config = AppConfig::default();

    // Точные значения дефолтов закрепляет golden-файл `current_schema_v12.toml`
    // (тест `current_schema_toml_roundtrip_is_textually_stable`); здесь — смысл: identity.
    assert!(config.render.color_adjustment.is_identity());
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
