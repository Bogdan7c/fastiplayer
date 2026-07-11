use std::path::Path;

use super::migrations::{REMOVED_FRAME_SERVER_HOVER_KEYS, REMOVED_HARDWARE_DECODE_ONLY_KEY};
use super::*;
use crate::{
    CURRENT_SCHEMA_VERSION, FrameServerConfig, FrameServerLiveScrubDecodeModeConfig,
    HdrToSdrOperatorConfig, LEGACY_SCHEMA_VERSION_2, LEGACY_SCHEMA_VERSION_3, PausedCommitBehavior,
    ToneMappingMode, VideoBackendPreference, YoutubeHdrSelection, validation,
};

/// Проверяет, что default schema остаётся самосогласованной.
#[test]
fn default_config_is_valid() {
    AppConfig::default()
        .validate()
        .expect("default config valid");
}

/// Старый schema v5 без нового ключа сохраняет прежнее SDR-only поведение.
#[test]
fn schema_v5_without_youtube_hdr_selection_defaults_to_sdr_only() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 5

[youtube]
enabled = true
prefer_account_session = true
resolve_timeout_ms = 30000
"#,
    )
    .expect("old schema v5 config written");

    let loaded = load_from_path(&config_path).expect("old schema v5 config loads");

    assert_eq!(
        loaded.config.youtube.hdr_selection,
        YoutubeHdrSelection::SdrOnly
    );
    assert_eq!(loaded.config.schema_version, 5);
}

/// Оба стабильных id читаются и записываются без изменения schema version.
#[test]
fn youtube_hdr_selection_stable_ids_roundtrip() {
    for (stable_id, expected_selection) in [
        ("sdr_only", YoutubeHdrSelection::SdrOnly),
        ("prefer_hdr", YoutubeHdrSelection::PreferHdrWhenAvailable),
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
schema_version = 5

[youtube]
hdr_selection = "{stable_id}"
"#
            ),
        )
        .expect("HDR selection config written");

        let loaded = load_from_path(&config_path).expect("HDR selection config loads");
        assert_eq!(loaded.config.youtube.hdr_selection, expected_selection);

        let generated = loaded
            .config
            .to_pretty_toml()
            .expect("HDR selection config serializes");
        assert!(generated.contains(&format!("hdr_selection = \"{stable_id}\"")));
        assert!(generated.contains("schema_version = 5"));
    }
}

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

/// Проверяет defaults текущей schema version 5.
#[test]
fn schema_version_5_defaults_include_seek_network_and_ui_skin() {
    let config = AppConfig::default();

    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(CURRENT_SCHEMA_VERSION, 5);
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
    assert_eq!(config.youtube.resolve_timeout_ms, 30_000);
    assert_eq!(config.ui.skin, "minimal");
    assert_eq!(config.ui.window.titlebar_height_px, 40);
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

/// Проверяет первый запуск без существующего config-файла.
#[test]
fn missing_config_is_created_with_defaults() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("rustiplayer").join("config.toml");

    let loaded = load_or_create_at(&config_path).expect("default config created");

    assert!(loaded.created);
    assert_eq!(loaded.path, config_path);
    assert_eq!(loaded.config, AppConfig::default());
    assert!(loaded.path.exists());
    assert!(loaded.config.render.color_adjustment.is_identity());

    let created_toml = fs::read_to_string(&loaded.path).expect("created config readable");
    assert!(created_toml.contains("schema_version = 5"));
    assert!(created_toml.contains("[player.seek]"));
    assert!(created_toml.contains("# Настройки seek commit"));
    assert!(created_toml.contains("commit_timeout_ms = 10000"));
    assert!(created_toml.contains("resume_audio_gate_timeout_ms = 250"));
    assert!(created_toml.contains("resume_video_min_ready_frames = 3"));
    assert!(created_toml.contains("fast_preroll_budget_ms = 48"));
    assert!(created_toml.contains("fast_preroll_video_packet_burst = 512"));
    assert!(created_toml.contains("[player.demux]"));
    assert!(created_toml.contains("# Fail-safe настройки demuxer-а."));
    assert!(created_toml.contains("max_consecutive_corrupted_packets = 64"));
    assert!(created_toml.contains("decoder_packet_channel_frames = 32"));
    assert!(created_toml.contains("# Bounded очередь packets"));
    assert!(created_toml.contains("[video.scheduler]"));
    assert!(created_toml.contains("# Настройки worker scheduler-а"));
    assert!(created_toml.contains("demux_packets_per_tick = 12"));
    assert!(created_toml.contains("present_queue_target_frames = 4"));
    assert!(created_toml.contains("decode_ahead_target_ms = 250"));
    assert!(created_toml.contains("surface_free_slots_target = 4"));
    assert!(created_toml.contains("# RAM cache budget"));
    assert!(created_toml.contains("memory_cache_mb = 128"));
    assert!(created_toml.contains("read_ahead_mb = 256"));
    assert!(created_toml.contains("prefetch_initial_chunk_kb = 64"));
    assert!(created_toml.contains("# Размер ПЕРВОГО prefetch-чтения"));
    assert!(created_toml.contains("prefetch_chunk_mb = 8"));
    assert!(created_toml.contains("# Timeout подготовки YouTube metadata"));
    assert!(created_toml.contains("resolve_timeout_ms = 30000"));
    assert!(!created_toml.contains("index_fingerprint_sample_kb"));
    assert!(created_toml.contains("# UI skin id"));
    assert!(created_toml.contains("skin = \"minimal\""));
    assert!(created_toml.contains("[ui.window]"));
    assert!(created_toml.contains("titlebar_height_px = 40"));
    assert!(created_toml.contains("[ui.settings]"));
    assert!(created_toml.contains("live_preview_max_hz = 60"));
    assert!(created_toml.contains("[render.hdr_to_sdr]"));
    assert!(created_toml.contains("operator = \"bt2446_c\""));
    assert!(!created_toml.contains(REMOVED_HARDWARE_DECODE_ONLY_KEY));

    let reparsed = toml::from_str::<AppConfig>(&created_toml)
        .expect("documented default config remains valid TOML");
    assert_eq!(reparsed, AppConfig::default());
}

/// Проверяет atomic save happy path: файл заменяется generated TOML и потом читается обратно.
#[test]
fn save_validated_atomic_at_writes_roundtrippable_generated_toml() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        "# Пользовательский комментарий, который save не обязан сохранять.\nschema_version = 2\n",
    )
    .expect("old config written");

    let mut config = AppConfig::default();
    config.ui.settings.live_preview_max_hz = 144;

    save_validated_atomic_at(&config_path, &config).expect("valid config saved atomically");

    let saved_toml = fs::read_to_string(&config_path).expect("saved config readable");
    assert!(!saved_toml.contains("Пользовательский комментарий"));
    assert!(saved_toml.contains("[ui.settings]"));
    assert!(saved_toml.contains("live_preview_max_hz = 144"));
    assert_no_save_temp_files(temp_dir.path());

    let loaded = load_from_path(&config_path).expect("saved config loads");
    assert_eq!(loaded.config, config);
}

/// Проверяет, что invalid config отбрасывается до любых операций записи.
#[test]
fn save_validated_atomic_at_does_not_touch_file_when_config_is_invalid() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let original_toml = "это неважное старое содержимое, которое нельзя перетереть\n";
    fs::write(&config_path, original_toml).expect("old config written");

    let mut config = AppConfig::default();
    config.ui.settings.live_preview_max_hz = 0;

    let error = save_validated_atomic_at(&config_path, &config)
        .expect_err("invalid config rejected before write");

    assert!(
        error
            .to_string()
            .contains("ui.settings.live_preview_max_hz")
    );
    assert_eq!(
        fs::read_to_string(&config_path).expect("old config still readable"),
        original_toml
    );
    assert_no_save_temp_files(temp_dir.path());
}

/// Проверяет compatibility: старый `[ui]` без `[ui.settings]` получает defaults.
#[test]
fn existing_ui_config_without_settings_gets_live_preview_defaults() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[ui]
show_telemetry = false
language = "ru"
skin = "minimal"
"#,
    )
    .expect("legacy ui config written");

    let loaded = load_from_path(&config_path).expect("legacy ui config accepted");

    assert!(!loaded.config.ui.show_telemetry);
    assert_eq!(loaded.config.ui.window.titlebar_height_px, 40);
    assert_eq!(loaded.config.ui.settings.live_preview_max_hz, 60);
    assert_eq!(loaded.config.ui.animations.sidebar_slide_duration_ms, 500);
}

/// Проверяет, что старый config без `[frame_server]` получает V1 defaults.
#[test]
fn existing_config_without_frame_server_gets_frame_server_defaults() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 4

[ui]
language = "ru"
"#,
    )
    .expect("old config written");

    let loaded = load_from_path(&config_path).expect("old config loads with defaults");

    assert_default_frame_server_config(&loaded.config.frame_server);

    let generated_toml = loaded
        .config
        .to_pretty_toml()
        .expect("defaulted config serializes");
    assert_generated_frame_server_toml_documents_live_scrub_knobs(&generated_toml);
}

/// Проверяет strict schema отказ от запрещённых/legacy frame-server knobs.
#[test]
fn forbidden_frame_server_keys_are_rejected_by_strict_schema() {
    for forbidden_key in [
        "enabled",
        "network_prepare_enabled",
        "preview_debounce_ms",
        "warm_cache_frames",
        "global_cache_frames",
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
schema_version = 5

[frame_server]
{forbidden_key} = true
"#
            ),
        )
        .expect("invalid frame_server config written");

        let error =
            load_from_path(&config_path).expect_err(&format!("{forbidden_key} must be rejected"));

        assert!(error.to_string().contains("TOML-схеме"));
        assert!(error.to_string().contains(forbidden_key));
    }
}

/// Проверяет edge values live-scrub ranges для `[frame_server]`.
#[test]
fn frame_server_range_edges_are_accepted() {
    let mut config = AppConfig::default();
    config.frame_server.live_scrub_max_hz = validation::MIN_FRAME_SERVER_LIVE_SCRUB_MAX_HZ;

    config.validate().expect("minimum edge config valid");

    config.frame_server.live_scrub_max_hz = validation::MAX_FRAME_SERVER_LIVE_SCRUB_MAX_HZ;

    config.validate().expect("maximum edge config valid");
}

/// Проверяет validation отказ от out-of-range `[frame_server]` значений.
#[test]
fn invalid_frame_server_ranges_fail_validation() {
    let invalid_fields = [("live_scrub_max_hz", "0"), ("live_scrub_max_hz", "241")];

    for (field, value) in invalid_fields {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
schema_version = 5

[frame_server]
{field} = {value}
"#
            ),
        )
        .expect("invalid frame_server config written");

        let error = load_from_path(&config_path).expect_err(&format!("{field} must fail"));

        assert!(error.to_string().contains(&format!("frame_server.{field}")));
    }
}

/// Проверяет, что удалённые hover/predecode ключи не ломают загрузку старого файла.
#[test]
fn removed_frame_server_hover_keys_are_stripped_before_strict_parse() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 4

[frame_server]
hover_preview_enabled = true
hover_pool_frames = "auto"
hover_thread_count = 2
hover_prepare_window_slots = 1
software_hover_prepare_window_slots = 1
recent_superseded_prepare_slots = 1
software_recent_superseded_prepare_slots = 1
hover_leave_grace_ms = 500
network_hover_prepare_throttle_ms = 300
live_scrub_enabled = true
"#,
    )
    .expect("legacy frame_server config written");

    let loaded = load_from_path(&config_path).expect("removed hover keys are ignored");
    assert_default_frame_server_config(&loaded.config.frame_server);

    let generated_toml = loaded
        .config
        .to_pretty_toml()
        .expect("cleaned config serializes");
    for removed_key in REMOVED_FRAME_SERVER_HOVER_KEYS {
        assert!(
            !generated_toml.contains(removed_key),
            "generated TOML must not write removed key {removed_key:?}",
        );
    }
}

/// Проверяет, что текущая schema не принимает удалённые hover/predecode ключи как валидные.
#[test]
fn removed_frame_server_hover_keys_are_rejected_in_current_schema() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 5

[frame_server]
hover_preview_enabled = true
"#,
    )
    .expect("current schema config with removed key written");

    let error = load_from_path(&config_path).expect_err("current schema rejects removed key");

    assert!(error.to_string().contains("TOML-схеме"));
    assert!(error.to_string().contains("hover_preview_enabled"));
}

/// Проверяет TOML roundtrip и strict варианты live scrub decode mode.
#[test]
fn frame_server_live_scrub_decode_mode_roundtrips_and_rejects_unknown() {
    for (toml_value, expected_mode) in [
        (
            "throttled_latest",
            FrameServerLiveScrubDecodeModeConfig::ThrottledLatest,
        ),
        (
            "every_drag_event",
            FrameServerLiveScrubDecodeModeConfig::EveryDragEvent,
        ),
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
schema_version = 5

[frame_server]
live_scrub_decode_mode = "{toml_value}"
"#
            ),
        )
        .expect("valid mode config written");

        let loaded = load_from_path(&config_path).expect("valid mode loads");
        assert_eq!(
            loaded.config.frame_server.live_scrub_decode_mode,
            expected_mode
        );

        let generated_toml = loaded
            .config
            .to_pretty_toml()
            .expect("valid mode serializes");
        assert!(generated_toml.contains(&format!("live_scrub_decode_mode = \"{toml_value}\"")));
    }

    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 5

[frame_server]
live_scrub_decode_mode = "nearest_frame"
"#,
    )
    .expect("invalid mode config written");

    let error = load_from_path(&config_path).expect_err("unknown live mode rejected");

    assert!(error.to_string().contains("live_scrub_decode_mode"));
}

fn assert_default_frame_server_config(frame_server: &FrameServerConfig) {
    assert!(frame_server.live_scrub_enabled);
    assert_eq!(
        frame_server.live_scrub_decode_mode,
        FrameServerLiveScrubDecodeModeConfig::ThrottledLatest,
    );
    assert_eq!(frame_server.live_scrub_max_hz, 60);
}

fn assert_generated_frame_server_toml_documents_live_scrub_knobs(generated_toml: &str) {
    for expected_fragment in [
        "[frame_server]",
        "# Настройки Frame Server",
        "live_scrub_enabled = true",
        "# Включает live drag preview updates",
        "live_scrub_decode_mode = \"throttled_latest\"",
        "# Политика live scrub: throttled_latest или every_drag_event",
        "live_scrub_max_hz = 60",
        "# Максимальная частота live scrub decode-work",
    ] {
        assert!(
            generated_toml.contains(expected_fragment),
            "generated frame_server TOML must contain {expected_fragment:?}",
        );
    }

    for forbidden_fragment in ["frame_server.enabled", "warm", "global", "thumbnail_cache"] {
        assert!(
            !generated_toml.contains(forbidden_fragment),
            "generated frame_server TOML must not contain {forbidden_fragment:?}",
        );
    }
}

/// Проверяет, что кастомный titlebar остаётся в понятном desktop диапазоне.
#[test]
fn invalid_ui_window_titlebar_height_fails_validation() {
    for invalid_titlebar_height_px in [31_u16, 97_u16] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
schema_version = 2

[ui.window]
titlebar_height_px = {invalid_titlebar_height_px}
"#
            ),
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid titlebar height rejected");

        assert!(error.to_string().contains("ui.window.titlebar_height_px"));
    }
}

/// Проверяет валидацию времени анимации sidebar: 0 валиден («без анимации»),
/// значение выше верхней границы отклоняется до записи.
#[test]
fn sidebar_slide_duration_validation_accepts_zero_and_rejects_above_max() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");

    let mut config = AppConfig::default();
    config.ui.animations.sidebar_slide_duration_ms = 0;
    save_validated_atomic_at(&config_path, &config).expect("0 = «без анимации» валиден");

    config.ui.animations.sidebar_slide_duration_ms = 5001;
    let error = save_validated_atomic_at(&config_path, &config)
        .expect_err("слишком долгая анимация отклоняется");
    assert!(
        error
            .to_string()
            .contains("ui.animations.sidebar_slide_duration_ms")
    );
}

/// Проверяет, что старые index-only network поля больше не принимаются.
#[test]
fn legacy_index_only_network_config_fields_are_rejected() {
    for legacy_field in [
        "index_fingerprint_sample_kb = 512",
        "indexer_io_budget_mb_per_sec = 32",
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
schema_version = 2

[network]
{legacy_field}
"#
            ),
        )
        .expect("legacy config written");

        let error = load_from_path(&config_path).expect_err("legacy config rejected");
        let field_name = legacy_field
            .split_once(" = ")
            .expect("test legacy field format")
            .0;

        assert!(error.to_string().contains(field_name));
    }
}

/// Проверяет backward compatibility: старый network config без новых prefetch-полей получает defaults.
#[test]
fn existing_network_config_without_prefetch_fields_gets_defaults() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[network]
memory_cache_mb = 128
read_ahead_mb = 64
"#,
    )
    .expect("legacy network config written");

    let loaded = load_from_path(&config_path).expect("legacy network config accepted");

    assert_eq!(loaded.config.network.read_ahead_mb, 64);
    assert_eq!(loaded.config.network.prefetch_initial_chunk_kb, 64);
    assert_eq!(loaded.config.network.prefetch_chunk_mb, 8);
}

/// Проверяет, что старые preview-настройки seek не остаются молча принятыми.
#[test]
fn legacy_scrub_config_fields_are_rejected() {
    for legacy_field in [
        format!("{} = 33", concat!("live", "_interval_ms")),
        format!("{} = 100", concat!("live", "_preview_budget_ms")),
        format!(
            "{} = \"visible-preview\"",
            concat!("timeline", "_release_policy")
        ),
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
schema_version = 2

[player.seek]
{legacy_field}
"#
            ),
        )
        .expect("legacy config written");

        let error = load_from_path(&config_path).expect_err("legacy config rejected");
        let field_name = legacy_field
            .split_once(" = ")
            .expect("test legacy field format")
            .0;

        assert!(error.to_string().contains(field_name));
    }
}

/// Проверяет, что старый config без color_adjustment получает identity defaults.
#[test]
fn existing_config_without_color_adjustment_gets_identity_defaults() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[render]
profile = "vulkan"
"#,
    )
    .expect("legacy config written");

    let loaded = load_from_path(&config_path).expect("legacy config accepted");

    assert!(loaded.config.render.color_adjustment.is_identity());
    assert!(loaded.config.render.hdr_to_sdr.enabled);
}

/// Проверяет compatibility-путь для старого scalar placeholder `render.hdr_to_sdr`.
#[test]
fn legacy_scalar_hdr_to_sdr_defaults_to_phase10_table_config() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[render]
profile = "vulkan"
hdr_to_sdr = false
"#,
    )
    .expect("legacy config written");

    let loaded = load_from_path(&config_path).expect("legacy scalar accepted");

    assert_eq!(loaded.config.render.hdr_to_sdr, Default::default());
}

/// Проверяет, что alternative tone mapping operator не проходит TOML-схему.
#[test]
fn invalid_hdr_to_sdr_operator_is_rejected() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[render.hdr_to_sdr]
operator = "reinhard"
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid operator rejected");

    assert!(error.to_string().contains("TOML-схеме"));
}

/// Проверяет, что удалённый Vulkan video backend preference получает понятную подсказку.
#[test]
fn removed_vulkan_video_backend_preference_has_suggested_fix() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        concat!(
            "\nschema_version = 2\n\n[video]\n",
            "preferred_backend = \"vul",
            "kan\"\n"
        ),
    )
    .expect("removed backend config written");

    let error = load_from_path(&config_path).expect_err("removed backend rejected");
    let message = error.to_string();

    assert!(message.contains("video.preferred_backend"));
    assert!(message.contains("\"vulkan\""));
    assert!(message.contains("\"auto\""));
    assert!(message.contains("\"hardware\""));
    assert!(message.contains("удал"));
    assert!(message.contains("замените"));
}

/// Проверяет migration boundary: v2 `vaapi` становится v3 `hardware`.
#[test]
fn legacy_vaapi_video_backend_preference_migrates_to_hardware() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[video]
preferred_backend = "vaapi"
"#,
    )
    .expect("legacy backend config written");

    let loaded = load_from_path(&config_path).expect("legacy backend migrated");

    assert_eq!(loaded.config.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(
        loaded.config.video.preferred_backend,
        VideoBackendPreference::Hardware
    );
}

/// Проверяет migration boundary: старая duplicate-галка больше не ломает strict schema.
#[test]
fn legacy_hardware_decode_only_field_is_removed_before_strict_parse() {
    for legacy_schema_version in [LEGACY_SCHEMA_VERSION_2, LEGACY_SCHEMA_VERSION_3] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
schema_version = {legacy_schema_version}

[video]
hardware_decode_only = false
preferred_backend = "hardware"
"#
            ),
        )
        .expect("legacy config written");

        let loaded =
            load_from_path(&config_path).expect("removed hardware flag ignored for migration");

        assert_eq!(loaded.config.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(
            loaded.config.video.preferred_backend,
            VideoBackendPreference::Hardware
        );
    }
}

/// Проверяет, что другие неизвестные backend id остаются обычной schema error.
#[test]
fn unknown_video_backend_preference_stays_generic_parse_error() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[video]
preferred_backend = "cuda"
"#,
    )
    .expect("unknown backend config written");

    let error = load_from_path(&config_path).expect_err("unknown backend rejected");
    let message = error.to_string();

    assert!(message.contains("TOML-схеме"));
    assert!(message.contains("cuda"));
    assert!(message.contains("auto"));
    assert!(message.contains("hardware"));
    assert!(message.contains("software"));
    assert!(!message.contains("удал"));
    assert!(!message.contains("замените"));
}

/// Проверяет validation для нулевого SDR reference white.
#[test]
fn invalid_hdr_to_sdr_sdr_white_nits_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[render.hdr_to_sdr]
sdr_reference_white_nits = 0.0
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid SDR white rejected");

    assert!(
        error
            .to_string()
            .contains("render.hdr_to_sdr.sdr_reference_white_nits")
    );
}

/// Проверяет validation для HDR peak, который не выше SDR reference white.
#[test]
fn invalid_hdr_to_sdr_peak_nits_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[render.hdr_to_sdr]
sdr_reference_white_nits = 100.0
hdr_reference_peak_nits = 100.0
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid HDR peak rejected");

    assert!(
        error
            .to_string()
            .contains("render.hdr_to_sdr.hdr_reference_peak_nits")
    );
}

/// Проверяет понятную ошибку validation для некорректной громкости.
#[test]
fn invalid_volume_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[audio]
volume = 1.5
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid volume rejected");

    assert!(error.to_string().contains("audio.volume"));
}

/// Проверяет, что RAM cache можно явно отключить нулём.
#[test]
fn network_memory_cache_zero_is_valid() {
    let mut config = AppConfig::default();
    config.network.memory_cache_mb = 0;

    config
        .validate()
        .expect("zero memory cache disables RAM cache");
}

/// Проверяет верхнюю границу RAM cache.
#[test]
fn invalid_network_memory_cache_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[network]
memory_cache_mb = 4097
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid memory cache rejected");

    assert!(error.to_string().contains("network.memory_cache_mb"));
}

/// Проверяет, что prefetch chunk нельзя отключить нулём.
#[test]
fn invalid_network_prefetch_chunk_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[network]
prefetch_chunk_mb = 0
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid prefetch chunk rejected");

    assert!(error.to_string().contains("network.prefetch_chunk_mb"));
}

/// Проверяет, что initial prefetch chunk нельзя отключить нулём.
#[test]
fn invalid_network_prefetch_initial_chunk_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[network]
prefetch_initial_chunk_kb = 0
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid initial chunk rejected");

    assert!(
        error
            .to_string()
            .contains("network.prefetch_initial_chunk_kb")
    );
}

/// Проверяет, что initial prefetch chunk не может быть больше обычного chunk-а.
#[test]
fn invalid_network_prefetch_initial_chunk_larger_than_chunk_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[network]
prefetch_initial_chunk_kb = 2048
prefetch_chunk_mb = 1
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid initial/chunk ratio rejected");

    assert!(
        error
            .to_string()
            .contains("network.prefetch_initial_chunk_kb")
    );
}

/// Проверяет, что prefetch window не может быть меньше одного chunk-а.
#[test]
fn invalid_network_prefetch_window_smaller_than_chunk_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[network]
read_ahead_mb = 4
prefetch_chunk_mb = 8
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid prefetch window rejected");

    assert!(error.to_string().contains("network.read_ahead_mb"));
}

/// Проверяет положительность network timeout-ов.
#[test]
fn invalid_network_timeout_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[network]
connect_timeout_ms = 0
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid timeout rejected");

    assert!(error.to_string().contains("network.connect_timeout_ms"));
}

/// Проверяет положительность timeout-а подготовки YouTube metadata.
#[test]
fn invalid_youtube_resolve_timeout_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[youtube]
resolve_timeout_ms = 0
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid timeout rejected");

    assert!(error.to_string().contains("youtube.resolve_timeout_ms"));
}

/// Проверяет положительность seek commit timeout-а.
#[test]
fn invalid_seek_commit_timeout_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[player.seek]
commit_timeout_ms = 0
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid seek timeout rejected");

    assert!(error.to_string().contains("player.seek.commit_timeout_ms"));
}

/// Проверяет положительность soft timeout-а audio gate перед seek resume.
#[test]
fn invalid_seek_audio_gate_timeout_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[player.seek]
resume_audio_gate_timeout_ms = 0
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid audio gate timeout rejected");

    assert!(
        error
            .to_string()
            .contains("player.seek.resume_audio_gate_timeout_ms")
    );
}

/// Проверяет положительность video preroll перед seek resume.
#[test]
fn invalid_seek_resume_video_ready_frames_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[player.seek]
resume_video_min_ready_frames = 0
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid video preroll rejected");

    assert!(
        error
            .to_string()
            .contains("player.seek.resume_video_min_ready_frames")
    );
}

/// Проверяет bounded окно fast-preroll work для accurate seek.
#[test]
fn invalid_seek_fast_preroll_budget_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[player.seek]
fast_preroll_budget_ms = 0
"#,
    )
    .expect("invalid config written");

    let error =
        load_from_path(&config_path).expect_err("invalid seek fast-preroll budget rejected");

    assert!(
        error
            .to_string()
            .contains("player.seek.fast_preroll_budget_ms")
    );
}

/// Проверяет bounded burst video packets для accurate seek preroll.
#[test]
fn invalid_seek_fast_preroll_video_packet_burst_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[player.seek]
fast_preroll_video_packet_burst = 0
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid seek fast-preroll burst rejected");

    assert!(
        error
            .to_string()
            .contains("player.seek.fast_preroll_video_packet_burst")
    );
}

/// Проверяет положительность hotkey step-ов.
#[test]
fn invalid_seek_hotkey_step_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[player.seek]
hotkey_small_step_secs = 0
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid hotkey step rejected");

    assert!(
        error
            .to_string()
            .contains("player.seek.hotkey_small_step_secs")
    );
}

/// Проверяет, что неизвестный skin не мапится молча на default.
#[test]
fn invalid_ui_skin_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[ui]
skin = "dense"
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid skin rejected");

    assert!(error.to_string().contains("ui.skin"));
}

/// Проверяет, что Settings UI не принимает нулевой или чрезмерный preview rate.
#[test]
fn invalid_ui_settings_live_preview_hz_fails_validation() {
    for invalid_live_preview_max_hz in [0_u16, 241_u16] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
schema_version = 2

[ui.settings]
live_preview_max_hz = {invalid_live_preview_max_hz}
"#
            ),
        )
        .expect("invalid config written");

        let error = load_from_path(&config_path).expect_err("invalid preview Hz rejected");

        assert!(
            error
                .to_string()
                .contains("ui.settings.live_preview_max_hz")
        );
    }
}

/// Проверяет, что новые nested settings тоже сохраняют strict deny_unknown_fields.
#[test]
fn unknown_ui_settings_field_is_parse_error() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[ui.settings]
live_preview_max_hz = 60
unexpected = true
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("unknown ui settings field rejected");

    assert!(error.to_string().contains("TOML-схеме"));
    assert!(error.to_string().contains("unexpected"));
}

/// Проверяет validation error для RGB-массива неверной длины.
#[test]
fn invalid_rgb_gain_array_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[render.color_adjustment]
rgb_gain = [1.0, 1.0]
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid rgb_gain rejected");

    assert!(
        error
            .to_string()
            .contains("render.color_adjustment.rgb_gain")
    );
}

/// Проверяет validation error для RGB offset неверной длины.
#[test]
fn invalid_rgb_offset_array_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2

[render.color_adjustment]
rgb_offset = [0.0, 0.0, 0.0, 0.0]
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("invalid rgb_offset rejected");

    assert!(
        error
            .to_string()
            .contains("render.color_adjustment.rgb_offset")
    );
}

/// Проверяет отказ от неподдержанной версии схемы.
#[test]
fn unsupported_schema_version_fails_validation() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, "schema_version = 999\n").expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("schema version rejected");

    assert!(error.to_string().contains("schema_version"));
}

/// Проверяет, что неизвестные поля не игнорируются молча.
#[test]
fn unknown_field_is_parse_error() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 2
unexpected = true
"#,
    )
    .expect("invalid config written");

    let error = load_from_path(&config_path).expect_err("unknown field rejected");

    assert!(error.to_string().contains("TOML-схеме"));
}

/// Проверяет, что atomic save не оставил временных файлов рядом с config.
fn assert_no_save_temp_files(config_directory: &Path) {
    let leftover_temp_file_count = fs::read_dir(config_directory)
        .expect("config directory readable")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".config.toml.")
        })
        .count();

    assert_eq!(leftover_temp_file_count, 0);
}
