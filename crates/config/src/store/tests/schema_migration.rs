//! Схема v2..v10: миграции, web-media/yt-dlp политики, плейлист и HDR-выбор.

use super::*;

/// Проверяет, что default schema остаётся самосогласованной.
#[test]
fn default_config_is_valid() {
    AppConfig::default()
        .validate()
        .expect("default config valid");
}

/// Additive output budgets schema v7 получают production defaults без обязательного rewrite.
#[test]
fn current_schema_without_yt_dlp_output_budgets_uses_safe_defaults() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let current_text = "schema_version = 10\n\n[yt_dlp]\nresolve_timeout_ms = 30000\n";
    fs::write(&config_path, current_text).expect("current config written");

    let loaded = load_from_path(&config_path).expect("current config without additive keys loads");
    let defaults = crate::YtDlpConfig::default();

    assert_eq!(
        loaded.config.yt_dlp.single_item_stdout_limit_bytes,
        defaults.single_item_stdout_limit_bytes
    );
    assert_eq!(
        loaded.config.yt_dlp.single_item_stderr_limit_bytes,
        defaults.single_item_stderr_limit_bytes
    );
    assert_eq!(
        loaded.config.yt_dlp.single_item_json_node_limit,
        defaults.single_item_json_node_limit
    );
    assert_eq!(loaded.origin, ConfigLoadOrigin::LoadedExisting);
    assert_eq!(
        fs::read_to_string(&config_path).expect("current file remains readable"),
        current_text
    );
}

/// Полный file boundary сохраняет пользовательские v9 policy/process values после v10 roundtrip.
#[test]
fn schema_v9_web_media_policy_load_migrate_validate_save_roundtrips_once() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 9

[yt_dlp]
enabled = false
hdr_selection = "prefer_hdr"
preferred_video_height = 1440
resolve_timeout_ms = 43210
single_item_stdout_limit_bytes = 70000000
single_item_stderr_limit_bytes = 9000000
single_item_json_node_limit = 1234567
vod_endpoint_recovery_enabled = false
vod_endpoint_recovery_max_consecutive_attempts = 7
vod_endpoint_recovery_initial_backoff_ms = 321
vod_endpoint_recovery_max_backoff_ms = 4321
vod_endpoint_recovery_stable_reset_ms = 54321
"#,
    )
    .expect("schema v9 fixture written");

    let migrated = load_from_path(&config_path).expect("schema v9 config migrates");
    assert_eq!(migrated.config.schema_version, 10);
    assert!(!migrated.config.yt_dlp.enabled);
    assert_eq!(migrated.config.yt_dlp.resolve_timeout_ms, 43_210);
    assert_eq!(
        migrated.config.web_media.hdr_selection,
        WebMediaHdrSelection::PreferHdrWhenAvailable
    );
    assert_eq!(
        migrated
            .config
            .web_media
            .preferred_video_height
            .map(PreferredVideoHeight::pixels),
        Some(1_440)
    );
    assert!(!migrated.config.web_media.vod_endpoint_recovery_enabled);
    assert_eq!(
        migrated
            .config
            .web_media
            .vod_endpoint_recovery_max_consecutive_attempts,
        7
    );
    assert_eq!(
        migrated
            .config
            .web_media
            .vod_endpoint_recovery_initial_backoff_ms,
        321
    );
    assert_eq!(
        migrated
            .config
            .web_media
            .vod_endpoint_recovery_max_backoff_ms,
        4_321
    );
    assert_eq!(
        migrated
            .config
            .web_media
            .vod_endpoint_recovery_stable_reset_ms,
        54_321
    );

    save_validated_atomic_at(&config_path, &migrated.config)
        .expect("migrated v10 config saves atomically");
    let reloaded = load_from_path(&config_path).expect("saved v10 config reloads");
    assert_eq!(reloaded.config, migrated.config);

    let saved_text = fs::read_to_string(&config_path).expect("saved v10 TOML readable");
    let saved_document: toml::Value = toml::from_str(&saved_text).expect("saved TOML parses");
    let web_media = saved_document
        .get("web_media")
        .and_then(toml::Value::as_table)
        .expect("saved v10 has web_media table");
    let yt_dlp = saved_document
        .get("yt_dlp")
        .and_then(toml::Value::as_table)
        .expect("saved v10 has yt_dlp table");
    for migrated_key in [
        "hdr_selection",
        "preferred_video_height",
        "vod_endpoint_recovery_enabled",
        "vod_endpoint_recovery_max_consecutive_attempts",
        "vod_endpoint_recovery_initial_backoff_ms",
        "vod_endpoint_recovery_max_backoff_ms",
        "vod_endpoint_recovery_stable_reset_ms",
    ] {
        assert!(web_media.contains_key(migrated_key));
        assert!(!yt_dlp.contains_key(migrated_key));
    }
}

/// Два источника одного v9 policy не объединяются молча.
#[test]
fn schema_v9_conflicting_web_media_and_yt_dlp_policy_is_rejected() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        r#"
schema_version = 9

[web_media]
hdr_selection = "sdr_only"

[yt_dlp]
hdr_selection = "prefer_hdr"
"#,
    )
    .expect("conflicting schema v9 fixture written");

    let error = load_from_path(&config_path).expect_err("conflicting policy must fail closed");
    assert!(error.to_string().contains("hdr_selection"));
}

/// Current schema не сохраняет legacy alias-поля внутри process-only `[yt_dlp]`.
#[test]
fn schema_v10_rejects_legacy_web_media_key_inside_yt_dlp() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        "schema_version = 10\n\n[yt_dlp]\npreferred_video_height = 1080\n",
    )
    .expect("strict schema v10 fixture written");

    let error = load_from_path(&config_path).expect_err("legacy alias must be rejected");
    assert!(error.to_string().contains("preferred_video_height"));
}

/// Provider-neutral section остаётся fail-closed для неизвестной будущей policy.
#[test]
fn schema_v10_web_media_section_rejects_unknown_fields() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(
        &config_path,
        "schema_version = 10\n\n[web_media]\nfuture_recovery_magic = true\n",
    )
    .expect("strict web-media fixture written");

    let error = load_from_path(&config_path).expect_err("unknown web-media field must fail");
    assert!(error.to_string().contains("future_recovery_magic"));
}

/// Output budgets принимают обе границы и отвергают соседние значения.
#[test]
fn yt_dlp_output_budget_bounds_are_inclusive_and_reject_neighbors() {
    fn assert_bounds(set_budget: impl Fn(&mut AppConfig, u64), maximum: u64) {
        for accepted_value in [1, maximum] {
            let mut config = AppConfig::default();
            set_budget(&mut config, accepted_value);
            config.validate().expect("inclusive budget bound accepted");
        }
        for rejected_value in [0, maximum + 1] {
            let mut config = AppConfig::default();
            set_budget(&mut config, rejected_value);
            assert!(config.validate().is_err(), "out-of-range budget accepted");
        }
    }

    assert_bounds(
        |config, value| config.yt_dlp.single_item_stdout_limit_bytes = value,
        validation::MAX_YT_DLP_SINGLE_ITEM_STDOUT_BYTES,
    );
    assert_bounds(
        |config, value| config.yt_dlp.single_item_stderr_limit_bytes = value,
        validation::MAX_YT_DLP_SINGLE_ITEM_STDERR_BYTES,
    );
    assert_bounds(
        |config, value| config.yt_dlp.single_item_json_node_limit = value,
        validation::MAX_YT_DLP_SINGLE_ITEM_JSON_NODES,
    );
}

/// Bounded VOD recovery policy принимает edges и запрещает инвертированный backoff.
#[test]
fn web_media_vod_recovery_policy_bounds_are_validated_as_one_contract() {
    for accepted_attempts in [1, validation::MAX_WEB_MEDIA_VOD_RECOVERY_ATTEMPTS] {
        let mut config = AppConfig::default();
        config
            .web_media
            .vod_endpoint_recovery_max_consecutive_attempts = accepted_attempts;
        config.validate().expect("inclusive attempt bound accepted");
    }
    for rejected_attempts in [0, validation::MAX_WEB_MEDIA_VOD_RECOVERY_ATTEMPTS + 1] {
        let mut config = AppConfig::default();
        config
            .web_media
            .vod_endpoint_recovery_max_consecutive_attempts = rejected_attempts;
        assert!(
            config.validate().is_err(),
            "invalid attempt budget accepted"
        );
    }

    let mut inverted_backoff = AppConfig::default();
    inverted_backoff
        .web_media
        .vod_endpoint_recovery_initial_backoff_ms = 2_001;
    inverted_backoff
        .web_media
        .vod_endpoint_recovery_max_backoff_ms = 2_000;
    assert!(
        inverted_backoff.validate().is_err(),
        "initial backoff larger than cap must be rejected"
    );

    let mut maximum_edges = AppConfig::default();
    maximum_edges
        .web_media
        .vod_endpoint_recovery_initial_backoff_ms =
        validation::MAX_WEB_MEDIA_VOD_RECOVERY_BACKOFF_MS;
    maximum_edges.web_media.vod_endpoint_recovery_max_backoff_ms =
        validation::MAX_WEB_MEDIA_VOD_RECOVERY_BACKOFF_MS;
    maximum_edges
        .web_media
        .vod_endpoint_recovery_stable_reset_ms =
        validation::MAX_WEB_MEDIA_VOD_RECOVERY_STABLE_RESET_MS;
    maximum_edges
        .validate()
        .expect("inclusive recovery time bounds accepted");
}

#[test]
fn playlist_defaults_include_bounded_next_item_preload() {
    let playlist = AppConfig::default().playlist;
    assert!(playlist.next_item_preload_enabled);
    assert_eq!(playlist.next_item_preload_budget_mb, 64);
    assert_eq!(playlist.next_item_preload_lead_time_ms, 30_000);
    assert_eq!(playlist.next_item_preload_max_hold_ms, 120_000);
    assert!(playlist.load_siblings);
    assert_eq!(
        playlist.sibling_media_filter,
        crate::PlaylistSiblingMediaFilter::SameAsOpened
    );
    assert_eq!(
        playlist.playback_behavior,
        crate::PlaylistPlaybackBehavior::StopAfterLast
    );
    assert_eq!(playlist.error_behavior, crate::PlaylistErrorBehavior::Stop);
    assert_eq!(playlist.state_save_debounce_ms, 2_000);
    assert_eq!(playlist.resume_checkpoint_interval_ms, 5_000);
    assert_eq!(playlist.previous_restart_threshold_ms, 5_000);
}

#[test]
fn playlist_next_item_preload_policy_is_bounded_as_one_contract() {
    for budget_mebibytes in [16, 512] {
        let mut config = AppConfig::default();
        config.playlist.next_item_preload_budget_mb = budget_mebibytes;
        config.validate().expect("inclusive preload RAM bound");
    }
    for budget_mebibytes in [15, 513] {
        let mut config = AppConfig::default();
        config.playlist.next_item_preload_budget_mb = budget_mebibytes;
        assert!(config.validate().is_err());
    }

    let mut invalid_freshness = AppConfig::default();
    invalid_freshness.playlist.next_item_preload_lead_time_ms = 120_001;
    invalid_freshness.playlist.next_item_preload_max_hold_ms = 120_000;
    let error = invalid_freshness
        .validate()
        .expect_err("hold shorter than lead must fail");
    assert!(
        error
            .to_string()
            .contains("playlist.next_item_preload_max_hold_ms")
    );
}

/// Additive `[playlist]` в schema v5 default-ится без rewrite существующего файла.
#[test]
fn legacy_v5_without_playlist_uses_defaults_without_startup_rewrite() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let legacy_text = "schema_version = 5\n\n[ui]\nlanguage = \"ru\"\n";
    fs::write(&config_path, legacy_text).expect("legacy config written");

    let loaded = load_from_path(&config_path).expect("legacy config loads");

    assert_eq!(loaded.config.playlist, crate::PlaylistConfig::default());
    assert_eq!(loaded.origin, ConfigLoadOrigin::LoadedExisting);
    assert_eq!(
        fs::read_to_string(&config_path).expect("legacy file remains readable"),
        legacy_text
    );
}

#[test]
fn playlist_bounds_are_inclusive_and_reject_neighbors() {
    for value in [250, 30_000] {
        let mut config = AppConfig::default();
        config.playlist.state_save_debounce_ms = value;
        config
            .validate()
            .expect("inclusive debounce bound accepted");
    }
    for value in [249, 30_001] {
        let mut config = AppConfig::default();
        config.playlist.state_save_debounce_ms = value;
        assert!(config.validate().is_err());
    }
    for value in [0, 60_000] {
        let mut config = AppConfig::default();
        config.playlist.previous_restart_threshold_ms = value;
        config
            .validate()
            .expect("inclusive Previous bound accepted");
    }
    let mut config = AppConfig::default();
    config.playlist.previous_restart_threshold_ms = 60_001;
    assert!(config.validate().is_err());

    for value in [1_000, 60_000] {
        let mut config = AppConfig::default();
        config.playlist.resume_checkpoint_interval_ms = value;
        config
            .validate()
            .expect("inclusive resume checkpoint interval accepted");
    }
    for value in [999, 60_001] {
        let mut config = AppConfig::default();
        config.playlist.resume_checkpoint_interval_ms = value;
        assert!(config.validate().is_err());
    }
}

#[test]
fn playlist_section_rejects_unknown_fields_and_roundtrips_stable_enum_ids() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let unknown = "schema_version = 5\n\n[playlist]\nunknown = true\n";
    fs::write(&config_path, unknown).expect("unknown fixture written");
    let error =
        load_from_path(&config_path).expect_err("strict playlist section rejects unknown field");
    assert!(error.to_string().contains("unknown"));

    let generated = AppConfig::default()
        .to_pretty_toml()
        .expect("defaults serialize");
    for stable_id in ["same_as_opened", "stop_after_last", "stop"] {
        assert!(generated.contains(stable_id));
    }
}

/// Старый schema v5 без нового ключа сохраняет прежнее SDR-only поведение.
#[test]
fn schema_v5_without_yt_dlp_hdr_selection_defaults_to_sdr_only() {
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
        loaded.config.web_media.hdr_selection,
        WebMediaHdrSelection::SdrOnly
    );
    assert_eq!(loaded.config.schema_version, 10);
}

/// Все поддерживаемые legacy-схемы переименовывают `[youtube]` без потери значений.
#[test]
fn legacy_v2_through_v5_migrate_youtube_section_to_yt_dlp() {
    for legacy_schema_version in 2..=5 {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
schema_version = {legacy_schema_version}

[youtube]
enabled = false
prefer_account_session = true
hdr_selection = "prefer_hdr"
resolve_timeout_ms = 4321
"#
            ),
        )
        .expect("legacy yt-dlp config written");

        let loaded = load_from_path(&config_path).expect("legacy yt-dlp config loads");

        assert_eq!(loaded.config.schema_version, 10);
        assert_eq!(loaded.config.web_media.preferred_video_height, None);
        assert!(!loaded.config.yt_dlp.enabled);
        assert_eq!(
            loaded.config.web_media.hdr_selection,
            WebMediaHdrSelection::PreferHdrWhenAvailable
        );
        assert_eq!(loaded.config.yt_dlp.resolve_timeout_ms, 4321);
        let generated = loaded
            .config
            .to_pretty_toml()
            .expect("migrated config serializes");
        assert!(generated.contains("[yt_dlp]"));
        assert!(!generated.contains("[youtube]"));
        assert!(!generated.contains("prefer_account_session"));
    }
}

/// Current v8 не принимает старую секцию и удалённый placeholder.
#[test]
fn schema_v8_strictly_rejects_legacy_youtube_section_and_placeholder() {
    for legacy_fragment in [
        "[youtube]\nenabled = true\n",
        "[yt_dlp]\nprefer_account_session = true\n",
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!("schema_version = 8\n\n{legacy_fragment}"),
        )
        .expect("strict v7 fixture written");

        let error = load_from_path(&config_path).expect_err("legacy v7 key rejected");
        assert!(
            error.to_string().contains("youtube")
                || error.to_string().contains("prefer_account_session")
        );
    }
}

/// Оба стабильных id читаются и записываются без изменения schema version.
#[test]
fn web_media_hdr_selection_stable_ids_roundtrip() {
    for (stable_id, expected_selection) in [
        ("sdr_only", WebMediaHdrSelection::SdrOnly),
        ("prefer_hdr", WebMediaHdrSelection::PreferHdrWhenAvailable),
    ] {
        let temp_dir = tempfile::tempdir().expect("temp dir created");
        let config_path = temp_dir.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
schema_version = 10

[web_media]
hdr_selection = "{stable_id}"
"#
            ),
        )
        .expect("HDR selection config written");

        let loaded = load_from_path(&config_path).expect("HDR selection config loads");
        assert_eq!(loaded.config.web_media.hdr_selection, expected_selection);

        let generated = loaded
            .config
            .to_pretty_toml()
            .expect("HDR selection config serializes");
        assert!(generated.contains(&format!("hdr_selection = \"{stable_id}\"")));
        assert!(generated.contains("schema_version = 10"));
    }
}

/// Schema v6 получает новый optional default и поднимается до v7 без startup rewrite.
#[test]
fn schema_v6_without_preferred_height_migrates_to_best_playable() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let legacy_text = r#"
schema_version = 6

[yt_dlp]
enabled = true
hdr_selection = "sdr_only"
resolve_timeout_ms = 30000
"#;
    fs::write(&config_path, legacy_text).expect("schema v6 config written");

    let loaded = load_from_path(&config_path).expect("schema v6 config loads");

    assert_eq!(loaded.config.schema_version, 10);
    assert_eq!(loaded.config.web_media.preferred_video_height, None);
    assert_eq!(loaded.origin, ConfigLoadOrigin::LoadedExisting);
    assert_eq!(
        fs::read_to_string(&config_path).expect("legacy file remains readable"),
        legacy_text
    );
}

/// Preferred height roundtrip сохраняет scalar, а bounds проверяются при parse.
#[test]
fn preferred_video_height_roundtrips_and_rejects_invalid_bounds() {
    let valid_height = PreferredVideoHeight::new(2160).expect("2160 валидно");
    let mut config = AppConfig::default();
    config.web_media.preferred_video_height = Some(valid_height);
    let serialized = config.to_pretty_toml().expect("config serializes");
    assert!(serialized.contains("preferred_video_height = 2160"));

    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, serialized).expect("preferred height config written");
    let loaded = load_from_path(&config_path).expect("preferred height config loads");
    assert_eq!(
        loaded.config.web_media.preferred_video_height,
        Some(valid_height)
    );
    assert!(
        !loaded
            .config
            .to_pretty_toml()
            .expect("loaded config serializes")
            .contains("item_override")
    );

    for invalid_height in [0, MAX_PREFERRED_VIDEO_HEIGHT + 1] {
        fs::write(
            &config_path,
            format!(
                "schema_version = 10\n\n[web_media]\npreferred_video_height = {invalid_height}\n"
            ),
        )
        .expect("invalid preferred height fixture written");
        let error = load_from_path(&config_path).expect_err("invalid height rejected");
        assert!(error.to_string().contains("предпочитаемая высота видео"));
    }

    fs::write(
        &config_path,
        "schema_version = 10\n\n[web_media]\nitem_video_height_override = 720\n",
    )
    .expect("runtime-only override fixture written");
    let error = load_from_path(&config_path).expect_err("runtime-only override rejected in TOML");
    assert!(error.to_string().contains("item_video_height_override"));
}
