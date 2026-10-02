//! Граничная валидация отдельных полей и строгий парсинг схемы.

use super::*;

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

/// Проверяет положительность timeout-а подготовки YtDlp metadata.
#[test]
fn invalid_yt_dlp_resolve_timeout_fails_validation() {
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

    assert!(error.to_string().contains("yt_dlp.resolve_timeout_ms"));
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
