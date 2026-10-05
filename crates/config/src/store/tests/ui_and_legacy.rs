//! UI-геометрия и отклонение/миграция legacy-ключей сети, scrub и backend-а.

use super::*;

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

/// Радиус принимает обе границы и default, но отвергает первое значение вне диапазона.
#[test]
fn ui_window_corner_radius_validation_uses_zero_through_twenty_four_range() {
    for valid_radius in [0_u16, 12_u16, 24_u16] {
        let mut config = AppConfig::default();
        config.ui.window.corner_radius_px = valid_radius;
        config
            .validate()
            .expect("valid window corner radius accepted");
    }

    let mut config = AppConfig::default();
    config.ui.window.corner_radius_px = 25;
    let error = config
        .validate()
        .expect_err("oversized corner radius rejected");

    assert!(error.to_string().contains("ui.window.corner_radius_px"));
}

/// Additive поле текущей схемы получает default без отдельной миграции.
#[test]
fn current_schema_without_window_corner_radius_loads_default() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let legacy_document = include_str!("../../../tests/fixtures/current_schema_v11.toml")
        .replace(
            "# Радиус прозрачного контура окна; 0 отключает скругление, диапазон 0..24 px.\ncorner_radius_px = 12\n",
            "",
        );
    fs::write(&config_path, legacy_document).expect("current schema fixture written");

    let loaded =
        load_from_path(&config_path).expect("current schema without additive field loaded");

    assert_eq!(loaded.config.ui.window.corner_radius_px, 12);
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

/// Проверяет inclusive границы sidebar и отказ от ближайших значений снаружи диапазона.
#[test]
fn sidebar_width_bounds_accept_edges_and_reject_neighbors() {
    for accepted_width in [
        crate::MIN_SIDEBAR_WIDTH_POINTS,
        crate::MAX_SIDEBAR_WIDTH_POINTS,
    ] {
        let mut config = AppConfig::default();
        config.ui.sidebar.width_points = accepted_width;
        config
            .validate()
            .expect("inclusive sidebar width boundary must be valid");
    }

    for rejected_width in [
        crate::MIN_SIDEBAR_WIDTH_POINTS - 1,
        crate::MAX_SIDEBAR_WIDTH_POINTS + 1,
    ] {
        let mut config = AppConfig::default();
        config.ui.sidebar.width_points = rejected_width;
        let error = config
            .validate()
            .expect_err("sidebar width outside the range must be rejected");
        assert!(error.to_string().contains("ui.sidebar.width_points"));
    }
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
