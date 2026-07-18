use std::collections::BTreeSet;

use settings_core::{
    DefaultBehavior, NumericRange, SelectDescriptor, SelectListDescriptor, SettingAccess,
    SettingApplyMode, SettingEditor, SettingId, SettingOptionId, SettingValue, SettingsError,
    SettingsRegistry, SettingsSchema, TextFormat,
};

use super::*;
use crate::validation;

const EXPECTED_SETTING_IDS: &[&str] = &[
    "schema_version",
    "player.start_paused",
    "player.resume_last_position",
    "player.seek.commit_timeout_ms",
    "player.seek.resume_audio_min_buffer_ms",
    "player.seek.resume_audio_gate_timeout_ms",
    "player.seek.resume_video_min_ready_frames",
    "player.seek.fast_preroll_budget_ms",
    "player.seek.fast_preroll_video_packet_burst",
    "player.seek.paused_commit_behavior",
    "player.seek.hotkey_small_step_secs",
    "player.seek.hotkey_large_step_secs",
    "player.demux.max_consecutive_corrupted_packets",
    "player.preferred_video_codec_order",
    "playlist.load_siblings",
    "playlist.sibling_media_filter",
    "playlist.playback_behavior",
    "playlist.error_behavior",
    "playlist.state_save_debounce_ms",
    "playlist.previous_restart_threshold_ms",
    "video.preferred_backend",
    "video.max_decode_ahead_ms",
    "video.present_queue_frames",
    "video.decoder_packet_channel_frames",
    "video.decoder_frame_channel_frames",
    "video.decoder_ready_queue_frames",
    "video.decoder_surface_pool_frames",
    "video.sw_decoder_surface_pool_frames",
    "video.sw_decode_threads",
    "video.zero_copy_surface_pool_slots",
    "video.scheduler.demux_packets_per_tick",
    "video.scheduler.video_packets_per_tick",
    "video.scheduler.decoded_frames_per_tick",
    "video.scheduler.catch_up_budget_ms",
    "video.scheduler.present_queue_min_frames",
    "video.scheduler.present_queue_target_frames",
    "video.scheduler.decode_ahead_target_ms",
    "video.scheduler.surface_free_slots_min",
    "video.scheduler.surface_free_slots_target",
    "frame_server.live_scrub_enabled",
    "frame_server.live_scrub_decode_mode",
    "frame_server.live_scrub_max_hz",
    "render.profile",
    "render.hdr_to_sdr.enabled",
    "render.hdr_to_sdr.operator",
    "render.hdr_to_sdr.sdr_reference_white_nits",
    "render.hdr_to_sdr.hdr_reference_peak_nits",
    "render.tone_mapping",
    "render.color_adjustment.brightness",
    "render.color_adjustment.contrast",
    "render.color_adjustment.saturation",
    "render.color_adjustment.exposure",
    "render.color_adjustment.rgb_gain",
    "render.color_adjustment.rgb_offset",
    "render.vulkan.present_mode",
    "render.vulkan.max_frame_latency",
    "render.opengles.enabled",
    "render.opengles.simple_ui",
    "audio.volume",
    "audio.output_device",
    "audio.buffer_target_ms",
    "network.memory_cache_mb",
    "network.read_ahead_mb",
    "network.prefetch_initial_chunk_kb",
    "network.prefetch_chunk_mb",
    "network.connect_timeout_ms",
    "network.read_timeout_ms",
    "yt_dlp.enabled",
    "yt_dlp.hdr_selection",
    "yt_dlp.resolve_timeout_ms",
    "ui.show_telemetry",
    "ui.language",
    "ui.skin",
    "ui.window.titlebar_height_px",
    "ui.sidebar.width_points",
    "ui.settings.live_preview_max_hz",
    "ui.animations.reduced_motion",
    "ui.animations.sidebar_slide_duration_ms",
];

#[test]
fn app_config_registry_covers_all_current_leaf_fields() {
    let registry = registry();
    let actual_ids = registry
        .descriptors()
        .map(|descriptor| descriptor.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let expected_ids = EXPECTED_SETTING_IDS
        .iter()
        .map(|id| (*id).to_owned())
        .collect::<BTreeSet<_>>();

    assert_eq!(actual_ids, expected_ids);
}

#[test]
fn schema_version_is_read_only_system_setting() {
    let registry = registry();
    let descriptor = descriptor(&registry, "schema_version");

    assert_eq!(descriptor.access, SettingAccess::ReadOnly);
    assert_eq!(descriptor.default_behavior, DefaultBehavior::NoReset);
    assert_eq!(descriptor.placement.section.as_str(), "system");
    assert_eq!(descriptor.placement.group.as_str(), "schema");
    assert!(matches!(descriptor.editor, SettingEditor::ReadOnly));

    let mut config = AppConfig::default();
    let error = registry
        .set_value(
            &mut config,
            &SettingId::from("schema_version"),
            SettingValue::Integer(3),
        )
        .expect_err("schema_version must reject writes");
    assert_eq!(
        error,
        SettingsError::ReadOnlySetting {
            id: SettingId::from("schema_version"),
        }
    );
    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn user_visible_metadata_has_text_and_placement() {
    let registry = registry();

    for descriptor in registry.descriptors() {
        assert!(
            descriptor
                .text
                .label
                .text_id
                .as_str()
                .starts_with("settings."),
            "{} label text id must be stable",
            descriptor.id,
        );
        assert!(
            !descriptor.text.label.fallback_ru.trim().is_empty(),
            "{} label fallback must be present",
            descriptor.id,
        );
        assert!(
            !descriptor.placement.section.as_str().is_empty(),
            "{} section must be present",
            descriptor.id,
        );
        assert!(
            !descriptor.placement.group.as_str().is_empty(),
            "{} group must be present",
            descriptor.id,
        );
        assert_eq!(
            descriptor.placement.preferred_surface.as_str(),
            "main-settings-window"
        );
    }
}

#[test]
fn audio_volume_metadata_names_default_volume_not_current_runtime_volume() {
    let registry = registry();
    let descriptor = descriptor(&registry, "audio.volume");

    assert!(descriptor.text.label.fallback_ru.contains("по умолчанию"));
    let description = descriptor
        .text
        .description
        .as_ref()
        .expect("audio.volume description must exist");
    assert!(description.fallback_ru.contains("Стартовая громкость"));
    let help = descriptor
        .text
        .help
        .as_ref()
        .expect("audio.volume help must exist");
    assert!(
        help.fallback_ru
            .contains("не перезаписывает текущую громкость")
    );
}

#[test]
fn live_preview_mode_is_limited_to_render_color_and_hdr_fields() {
    let registry = registry();
    let preview_ids = registry
        .descriptors()
        .filter(|descriptor| descriptor.apply_mode == SettingApplyMode::ImmediatePreview)
        .map(|descriptor| descriptor.id.as_str())
        .collect::<Vec<_>>();

    assert!(!preview_ids.is_empty());
    for id in preview_ids {
        assert!(
            id.starts_with("render.color_adjustment.") || id.starts_with("render.hdr_to_sdr."),
            "{id} must not be live preview"
        );
    }

    assert_eq!(
        descriptor(&registry, "ui.settings.live_preview_max_hz").apply_mode,
        SettingApplyMode::CommittedApply
    );
}

#[test]
fn rgb_fields_are_fixed_length_three_vectors() {
    let registry = registry();

    assert_vector_range(
        &registry,
        "render.color_adjustment.rgb_gain",
        validation::MIN_RENDER_RGB_GAIN,
        validation::MAX_RENDER_RGB_GAIN,
        validation::RGB_CHANNEL_COUNT,
    );
    assert_vector_range(
        &registry,
        "render.color_adjustment.rgb_offset",
        validation::MIN_RENDER_RGB_OFFSET,
        validation::MAX_RENDER_RGB_OFFSET,
        validation::RGB_CHANNEL_COUNT,
    );
}

#[test]
fn metadata_ranges_match_validation_constants() {
    let registry = registry();

    assert_integer_range(
        &registry,
        "playlist.state_save_debounce_ms",
        validation::MIN_PLAYLIST_STATE_SAVE_DEBOUNCE_MS,
        validation::MAX_PLAYLIST_STATE_SAVE_DEBOUNCE_MS,
    );
    assert_integer_range(
        &registry,
        "playlist.previous_restart_threshold_ms",
        validation::MIN_PLAYLIST_PREVIOUS_RESTART_THRESHOLD_MS,
        validation::MAX_PLAYLIST_PREVIOUS_RESTART_THRESHOLD_MS,
    );
    assert_integer_range(
        &registry,
        "player.seek.commit_timeout_ms",
        validation::MIN_POSITIVE_U64_SETTING_VALUE,
        validation::MAX_POSITIVE_U64_SETTING_VALUE,
    );
    assert_integer_range(
        &registry,
        "player.seek.resume_video_min_ready_frames",
        1_usize,
        validation::MAX_SEEK_RESUME_VIDEO_READY_FRAMES,
    );
    assert_integer_range(
        &registry,
        "player.seek.resume_audio_min_buffer_ms",
        validation::MIN_POSITIVE_U64_SETTING_VALUE,
        validation::MAX_POSITIVE_U64_SETTING_VALUE,
    );
    assert_integer_range(
        &registry,
        "player.seek.resume_audio_gate_timeout_ms",
        validation::MIN_POSITIVE_U64_SETTING_VALUE,
        validation::MAX_POSITIVE_U64_SETTING_VALUE,
    );
    assert_integer_range(
        &registry,
        "player.seek.fast_preroll_budget_ms",
        1_u64,
        validation::MAX_SEEK_FAST_PREROLL_BUDGET_MS,
    );
    assert_integer_range(
        &registry,
        "player.seek.fast_preroll_video_packet_burst",
        1_usize,
        validation::MAX_SEEK_FAST_PREROLL_VIDEO_PACKET_BURST,
    );
    assert_integer_range(
        &registry,
        "player.seek.hotkey_small_step_secs",
        validation::MIN_POSITIVE_U64_SETTING_VALUE,
        validation::MAX_POSITIVE_U64_SETTING_VALUE,
    );
    assert_integer_range(
        &registry,
        "player.seek.hotkey_large_step_secs",
        validation::MIN_POSITIVE_U64_SETTING_VALUE,
        validation::MAX_POSITIVE_U64_SETTING_VALUE,
    );
    assert_integer_range(
        &registry,
        "player.demux.max_consecutive_corrupted_packets",
        1_usize,
        validation::MAX_CONSECUTIVE_CORRUPTED_PACKETS,
    );
    assert_integer_range(
        &registry,
        "video.max_decode_ahead_ms",
        validation::MIN_DECODE_AHEAD_MS,
        validation::MAX_DECODE_AHEAD_MS,
    );
    assert_integer_range(
        &registry,
        "video.present_queue_frames",
        validation::MIN_PRESENT_QUEUE_FRAMES,
        validation::MAX_PRESENT_QUEUE_FRAMES,
    );
    assert_integer_range(
        &registry,
        "video.decoder_packet_channel_frames",
        validation::MIN_DECODER_QUEUE_FRAMES,
        validation::MAX_DECODER_QUEUE_FRAMES,
    );
    assert_integer_range(
        &registry,
        "video.decoder_frame_channel_frames",
        validation::MIN_DECODER_QUEUE_FRAMES,
        validation::MAX_DECODER_QUEUE_FRAMES,
    );
    assert_integer_range(
        &registry,
        "video.decoder_ready_queue_frames",
        validation::MIN_DECODER_QUEUE_FRAMES,
        validation::MAX_DECODER_QUEUE_FRAMES,
    );
    assert_integer_range(
        &registry,
        "video.decoder_surface_pool_frames",
        validation::MIN_DECODER_QUEUE_FRAMES,
        validation::MAX_DECODER_SURFACE_POOL_FRAMES,
    );
    assert_integer_range(
        &registry,
        "video.sw_decoder_surface_pool_frames",
        validation::MIN_DECODER_QUEUE_FRAMES,
        validation::MAX_DECODER_SURFACE_POOL_FRAMES,
    );
    assert_integer_range(
        &registry,
        "video.sw_decode_threads",
        validation::MIN_SW_DECODE_THREADS,
        validation::MAX_SW_DECODE_THREADS,
    );
    assert_integer_range(
        &registry,
        "video.zero_copy_surface_pool_slots",
        validation::MIN_DECODER_QUEUE_FRAMES,
        validation::MAX_ZERO_COPY_SURFACE_POOL_SLOTS,
    );
    assert_integer_range(
        &registry,
        "video.scheduler.demux_packets_per_tick",
        1_usize,
        validation::MAX_SCHEDULER_DEMUX_PACKETS_PER_TICK,
    );
    assert_integer_range(
        &registry,
        "video.scheduler.video_packets_per_tick",
        1_usize,
        validation::MAX_SCHEDULER_VIDEO_PACKETS_PER_TICK,
    );
    assert_integer_range(
        &registry,
        "video.scheduler.decoded_frames_per_tick",
        1_usize,
        validation::MAX_SCHEDULER_DECODED_FRAMES_PER_TICK,
    );
    assert_integer_range(
        &registry,
        "video.scheduler.catch_up_budget_ms",
        1_u64,
        validation::MAX_SCHEDULER_CATCH_UP_BUDGET_MS,
    );
    assert_integer_range(
        &registry,
        "video.scheduler.present_queue_min_frames",
        1_usize,
        validation::MAX_PRESENT_QUEUE_FRAMES,
    );
    assert_integer_range(
        &registry,
        "video.scheduler.present_queue_target_frames",
        1_usize,
        validation::MAX_PRESENT_QUEUE_FRAMES,
    );
    assert_integer_range(
        &registry,
        "video.scheduler.decode_ahead_target_ms",
        validation::MIN_DECODE_AHEAD_MS,
        validation::MAX_DECODE_AHEAD_MS,
    );
    assert_integer_range(
        &registry,
        "video.scheduler.surface_free_slots_min",
        0_usize,
        validation::MAX_ZERO_COPY_SURFACE_POOL_SLOTS,
    );
    assert_integer_range(
        &registry,
        "video.scheduler.surface_free_slots_target",
        0_usize,
        validation::MAX_ZERO_COPY_SURFACE_POOL_SLOTS,
    );
    assert_integer_range(
        &registry,
        "frame_server.live_scrub_max_hz",
        validation::MIN_FRAME_SERVER_LIVE_SCRUB_MAX_HZ,
        validation::MAX_FRAME_SERVER_LIVE_SCRUB_MAX_HZ,
    );
    assert_float_range(
        &registry,
        "render.hdr_to_sdr.sdr_reference_white_nits",
        validation::MIN_HDR_TO_SDR_REFERENCE_NITS,
        validation::MAX_HDR_TO_SDR_REFERENCE_NITS,
    );
    assert_float_range(
        &registry,
        "render.hdr_to_sdr.hdr_reference_peak_nits",
        validation::MIN_HDR_TO_SDR_REFERENCE_NITS,
        validation::MAX_HDR_TO_SDR_REFERENCE_NITS,
    );
    assert_float_range(
        &registry,
        "render.color_adjustment.brightness",
        validation::MIN_RENDER_COLOR_BRIGHTNESS,
        validation::MAX_RENDER_COLOR_BRIGHTNESS,
    );
    assert_float_range(
        &registry,
        "render.color_adjustment.contrast",
        validation::MIN_RENDER_COLOR_CONTRAST,
        validation::MAX_RENDER_COLOR_CONTRAST,
    );
    assert_float_range(
        &registry,
        "render.color_adjustment.saturation",
        validation::MIN_RENDER_COLOR_SATURATION,
        validation::MAX_RENDER_COLOR_SATURATION,
    );
    assert_float_range(
        &registry,
        "render.color_adjustment.exposure",
        validation::MIN_RENDER_COLOR_EXPOSURE,
        validation::MAX_RENDER_COLOR_EXPOSURE,
    );
    assert_vector_range(
        &registry,
        "render.color_adjustment.rgb_gain",
        validation::MIN_RENDER_RGB_GAIN,
        validation::MAX_RENDER_RGB_GAIN,
        validation::RGB_CHANNEL_COUNT,
    );
    assert_vector_range(
        &registry,
        "render.color_adjustment.rgb_offset",
        validation::MIN_RENDER_RGB_OFFSET,
        validation::MAX_RENDER_RGB_OFFSET,
        validation::RGB_CHANNEL_COUNT,
    );
    assert_integer_range(
        &registry,
        "render.vulkan.max_frame_latency",
        1_u32,
        validation::MAX_VULKAN_FRAME_LATENCY,
    );
    assert_float_range(
        &registry,
        "audio.volume",
        validation::MIN_AUDIO_VOLUME,
        validation::MAX_AUDIO_VOLUME,
    );
    assert_integer_range(
        &registry,
        "audio.buffer_target_ms",
        validation::MIN_AUDIO_BUFFER_TARGET_MS,
        validation::MAX_AUDIO_BUFFER_TARGET_MS,
    );
    assert_integer_range(
        &registry,
        "network.memory_cache_mb",
        0_u64,
        validation::MAX_NETWORK_MEMORY_CACHE_MB,
    );
    assert_integer_range(
        &registry,
        "network.read_ahead_mb",
        1_u64,
        validation::MAX_NETWORK_READ_AHEAD_MB,
    );
    assert_integer_range(
        &registry,
        "network.prefetch_initial_chunk_kb",
        1_u64,
        validation::MAX_NETWORK_PREFETCH_INITIAL_CHUNK_KB,
    );
    assert_integer_range(
        &registry,
        "network.prefetch_chunk_mb",
        1_u64,
        validation::MAX_NETWORK_READ_AHEAD_MB,
    );
    assert_integer_range(
        &registry,
        "network.connect_timeout_ms",
        validation::MIN_POSITIVE_U64_SETTING_VALUE,
        validation::MAX_POSITIVE_U64_SETTING_VALUE,
    );
    assert_integer_range(
        &registry,
        "network.read_timeout_ms",
        validation::MIN_POSITIVE_U64_SETTING_VALUE,
        validation::MAX_POSITIVE_U64_SETTING_VALUE,
    );
    assert_integer_range(
        &registry,
        "yt_dlp.resolve_timeout_ms",
        1_u64,
        validation::MAX_YT_DLP_RESOLVE_TIMEOUT_MS,
    );
    assert_text_len(
        &registry,
        "ui.language",
        validation::MIN_UI_LANGUAGE_LEN,
        validation::MAX_UI_LANGUAGE_LEN,
    );
    assert_integer_range(
        &registry,
        "ui.settings.live_preview_max_hz",
        validation::MIN_LIVE_PREVIEW_MAX_HZ,
        validation::MAX_LIVE_PREVIEW_MAX_HZ,
    );
    assert_integer_range(
        &registry,
        "ui.sidebar.width_points",
        crate::MIN_SIDEBAR_WIDTH_POINTS,
        crate::MAX_SIDEBAR_WIDTH_POINTS,
    );
    assert_integer_range(
        &registry,
        "ui.animations.sidebar_slide_duration_ms",
        validation::MIN_SIDEBAR_SLIDE_DURATION_MS,
        validation::MAX_SIDEBAR_SLIDE_DURATION_MS,
    );
    assert_integer_range(
        &registry,
        "ui.window.titlebar_height_px",
        validation::MIN_TITLEBAR_HEIGHT_PX,
        validation::MAX_TITLEBAR_HEIGHT_PX,
    );
}

#[test]
fn static_enum_and_string_options_use_stable_ids() {
    let registry = registry();

    assert_select_options(
        &registry,
        "player.seek.paused_commit_behavior",
        &["stay_paused"],
    );
    assert_select_list_options(
        &registry,
        "player.preferred_video_codec_order",
        &["vp9", "av1", "h264", "h265", "vp8"],
    );
    assert_select_options(
        &registry,
        "video.preferred_backend",
        &["auto", "hardware", "software"],
    );
    assert_select_options(
        &registry,
        "playlist.sibling_media_filter",
        &["video_only", "all_media", "audio_only", "same_as_opened"],
    );
    assert_select_options(
        &registry,
        "playlist.playback_behavior",
        &["stop_after_last", "repeat_queue", "repeat_one"],
    );
    assert_select_options(&registry, "playlist.error_behavior", &["stop", "skip"]);
    assert_select_options(&registry, "render.profile", &["auto", "vulkan", "opengles"]);
    assert_select_options(&registry, "render.hdr_to_sdr.operator", &["bt2446_c"]);
    assert_select_options(&registry, "render.tone_mapping", &["auto", "disabled"]);
    assert_select_options(
        &registry,
        "render.vulkan.present_mode",
        &["auto", "fifo", "mailbox", "immediate"],
    );
    assert_select_options(
        &registry,
        "frame_server.live_scrub_decode_mode",
        &["throttled_latest", "every_drag_event"],
    );
    assert_select_options(
        &registry,
        "yt_dlp.hdr_selection",
        &["sdr_only", "prefer_hdr"],
    );
    assert_select_options(&registry, "ui.skin", &[validation::DEFAULT_UI_SKIN]);
}

#[test]
fn frame_server_metadata_is_editable_and_metadata_ready() {
    let registry = registry();
    let frame_server_ids = EXPECTED_SETTING_IDS
        .iter()
        .copied()
        .filter(|id| id.starts_with("frame_server."))
        .collect::<Vec<_>>();

    assert_eq!(frame_server_ids.len(), 3);
    for id in frame_server_ids {
        let descriptor = descriptor(&registry, id);
        assert_eq!(descriptor.access, SettingAccess::ReadWrite);
        assert_eq!(
            descriptor.default_behavior,
            DefaultBehavior::FromDefaultDocument
        );
        assert_eq!(descriptor.placement.section.as_str(), "frame_server");
        assert!(!descriptor.placement.group_default_open);
        assert_eq!(descriptor.route.as_str(), "frame_server.apply");
        assert_eq!(descriptor.apply_mode, SettingApplyMode::CommittedApply);
        assert_russian_fallback(&descriptor.text.label.fallback_ru, id);
        let description = descriptor
            .text
            .description
            .as_ref()
            .unwrap_or_else(|| panic!("{id} description must explain frame_server behavior"));
        assert_russian_fallback(&description.fallback_ru, id);
        let help = descriptor
            .text
            .help
            .as_ref()
            .unwrap_or_else(|| panic!("{id} help must explain S30B limitations"));
        assert_russian_fallback(&help.fallback_ru, id);
    }

    let mut config = AppConfig::default();
    registry
        .set_value(
            &mut config,
            &SettingId::from("frame_server.live_scrub_enabled"),
            SettingValue::Bool(false),
        )
        .expect("frame_server bool setting must be writable in S30B");
    assert!(!config.frame_server.live_scrub_enabled);
}

#[test]
fn video_backend_options_do_not_expose_implementation_specific_public_id() {
    let registry = registry();
    let SettingEditor::Select(SelectDescriptor::Static { options }) =
        &descriptor(&registry, "video.preferred_backend").editor
    else {
        panic!("video.preferred_backend must use static select editor");
    };

    let ids = option_ids(options);
    let implementation_specific_underscore_id = ["ffmpeg", "sw"].join("_");
    let implementation_specific_dash_id = ["ffmpeg", "sw"].join("-");

    assert_eq!(ids, vec!["auto", "hardware", "software"]);
    assert!(!ids.contains(&implementation_specific_underscore_id.as_str()));
    assert!(!ids.contains(&implementation_specific_dash_id.as_str()));
}

#[test]
fn generated_accessors_read_and_reset_values_from_default_documents() {
    let registry = registry();
    let default_config = AppConfig::default();

    assert_eq!(
        registry
            .get_value(&default_config, &SettingId::from("audio.volume"))
            .expect("default audio.volume should be readable"),
        SettingValue::Float(default_config.audio.volume)
    );
    assert_eq!(
        registry
            .get_value(
                &default_config,
                &SettingId::from("player.preferred_video_codec_order"),
            )
            .expect("default codec order should be readable"),
        SettingValue::SelectList(vec![
            SettingOptionId::from("vp9"),
            SettingOptionId::from("av1"),
            SettingOptionId::from("h264"),
            SettingOptionId::from("h265"),
            SettingOptionId::from("vp8"),
        ])
    );

    let mut config = AppConfig::default();
    config.audio.volume = 0.25;
    let mut custom_default = AppConfig::default();
    custom_default.audio.volume = 0.5;

    registry
        .reset_value(
            &mut config,
            &custom_default,
            &SettingId::from("audio.volume"),
        )
        .expect("reset should read from provided default document");
    assert_eq!(config.audio.volume, custom_default.audio.volume);
}

fn registry() -> SettingsRegistry<AppConfig> {
    AppConfig::settings_registry().expect("AppConfig registry should be generated")
}

fn descriptor<'registry>(
    registry: &'registry SettingsRegistry<AppConfig>,
    id: &str,
) -> &'registry settings_core::SettingDescriptor {
    registry
        .descriptor(&SettingId::from(id))
        .unwrap_or_else(|| panic!("{id} descriptor should exist"))
}

fn assert_integer_range<Min, Max>(
    registry: &SettingsRegistry<AppConfig>,
    id: &str,
    expected_min: Min,
    expected_max: Max,
) where
    Min: TryInto<i64>,
    Max: TryInto<i64>,
    Min::Error: std::fmt::Debug,
    Max::Error: std::fmt::Debug,
{
    let SettingEditor::Numeric(numeric) = &descriptor(registry, id).editor else {
        panic!("{id} must use numeric editor");
    };
    let NumericRange::Integer { min, max } = &numeric.range else {
        panic!("{id} must use integer range");
    };
    assert_eq!(*min, expected_min.try_into().expect("min must fit i64"));
    assert_eq!(*max, expected_max.try_into().expect("max must fit i64"));
}

fn assert_float_range<Min, Max>(
    registry: &SettingsRegistry<AppConfig>,
    id: &str,
    expected_min: Min,
    expected_max: Max,
) where
    Min: Into<f64>,
    Max: Into<f64>,
{
    let SettingEditor::Numeric(numeric) = &descriptor(registry, id).editor else {
        panic!("{id} must use numeric editor");
    };
    let NumericRange::Float { min, max } = &numeric.range else {
        panic!("{id} must use float range");
    };
    assert_eq!(*min, expected_min.into());
    assert_eq!(*max, expected_max.into());
}

fn assert_vector_range<Min, Max>(
    registry: &SettingsRegistry<AppConfig>,
    id: &str,
    expected_min: Min,
    expected_max: Max,
    expected_len: usize,
) where
    Min: Into<f64>,
    Max: Into<f64>,
{
    let SettingEditor::Vector(vector) = &descriptor(registry, id).editor else {
        panic!("{id} must use vector editor");
    };
    let NumericRange::Float { min, max } = &vector.element.range else {
        panic!("{id} vector elements must use float range");
    };
    assert_eq!(*min, expected_min.into());
    assert_eq!(*max, expected_max.into());
    assert_eq!(vector.expected_len, expected_len);
}

fn assert_text_len(
    registry: &SettingsRegistry<AppConfig>,
    id: &str,
    expected_min_len: usize,
    expected_max_len: usize,
) {
    let SettingEditor::Text(text) = &descriptor(registry, id).editor else {
        panic!("{id} must use text editor");
    };
    assert_eq!(text.format, TextFormat::SingleLine);
    assert_eq!(text.min_len, Some(expected_min_len));
    assert_eq!(text.max_len, Some(expected_max_len));
}

fn assert_select_options(registry: &SettingsRegistry<AppConfig>, id: &str, expected: &[&str]) {
    let SettingEditor::Select(SelectDescriptor::Static { options }) =
        &descriptor(registry, id).editor
    else {
        panic!("{id} must use static select editor");
    };
    assert_eq!(option_ids(options), expected);
}

fn assert_select_list_options(registry: &SettingsRegistry<AppConfig>, id: &str, expected: &[&str]) {
    let SettingEditor::SelectList(SelectListDescriptor { options, .. }) =
        &descriptor(registry, id).editor
    else {
        panic!("{id} must use static select-list editor");
    };
    assert_eq!(option_ids(options), expected);
}

fn assert_russian_fallback(text: &str, id: &str) {
    assert!(
        text.chars().any(|character| {
            ('а'..='я').contains(&character)
                || ('А'..='Я').contains(&character)
                || character == 'ё'
                || character == 'Ё'
        }),
        "{id} fallback must contain Russian text",
    );
}

fn option_ids(options: &[settings_core::SettingOption]) -> Vec<&str> {
    options.iter().map(|option| option.id.as_str()).collect()
}
