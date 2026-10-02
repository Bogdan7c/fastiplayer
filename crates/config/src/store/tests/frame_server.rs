//! Секция frame_server: дефолты, диапазоны, удалённые ключи, live scrub режим.

use super::*;

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
