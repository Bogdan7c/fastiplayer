use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata};
use frame_server_core::{
    DeferredLiveScrubSettingsChange, LiveScrubDecodeMode, LiveScrubDiagnostics,
    LiveScrubSettingsSnapshot, ScrubDiagnosticsRecorder,
};
use media_core::MediaTime;
use player_core::{
    MediaOpenRequest, MediaSource, MediaSummary, PlaybackRate, PlaybackResumeIntent, PlaybackState,
    PlayerCommand, PlayerError, PlayerErrorKind, PlayerEvent, PlayerSnapshot, ScrubCommitPolicy,
    SeekCommitInfo, SeekRequest,
};
use render_core::{
    ActiveColorPath, ColorPipelineSettings, HdrMetadataDiagnosticMarker,
    HdrReferenceDefaultDiagnostics, RenderDiagnostics,
};
use video_core::{DecodedFrame, FrameResourceHandle};
use video_frame_contract::VideoFramePixelLayout;
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use super::present_frame_cache::{
    CachedPresentFrameDiscardReason, CachedPresentFrameValidationState, PresentFrameIdentity,
    TextureBusyFallbackRejectReason, TextureBusyFallbackReuseState,
    cached_present_frame_discard_reason_for_player_event, cached_present_frame_stale_reason,
    texture_busy_fallback_can_reuse_previous_frame, texture_busy_fallback_reject_reason,
};
use super::telemetry_panel::{
    TELEMETRY_PANEL_REFRESH_INTERVAL, TelemetryPanelCache, TelemetryPanelRow, TelemetryPanelState,
};
use super::ui_runtime::{
    live_scrub_release_policy_from_action, playback_rate_command_from_action,
    timeline_command_from_action,
};
use super::{AppFrameContext, AppState};
use crate::telemetry::Telemetry;
use crate::ui::player_controls::ControlAction;
use crate::ui::timeline::{TimelineAction, TimelineUiState};

/// Собирает source `state` и child-модулей для guard-тестов после split-а.
fn state_source_for_architecture_tests() -> String {
    [
        include_str!("../state.rs"),
        include_str!("media_jobs.rs"),
        include_str!("main_visual_override.rs"),
        include_str!("present_frame_cache.rs"),
        include_str!("telemetry_panel.rs"),
        include_str!("ui_runtime.rs"),
        include_str!("video_backend.rs"),
    ]
    .join("\n")
}

/// Возвращает участок source между двумя маркерами для architecture guard tests.
fn source_section_between<'source>(
    source_code: &'source str,
    start_marker: &str,
    end_marker: &str,
) -> &'source str {
    let section_start = source_code
        .find(start_marker)
        .unwrap_or_else(|| panic!("Не найден начальный source marker: {start_marker}"));
    let section_after_start = &source_code[section_start..];
    let section_end = section_after_start
        .find(end_marker)
        .unwrap_or_else(|| panic!("Не найден конечный source marker: {end_marker}"));

    &section_after_start[..section_end]
}

/// Создаёт decoded frame для pure identity tests без GPU lease-а.
fn decoded_frame_for_identity_tests(
    generation: u64,
    pts: Duration,
    resource_handle: u64,
) -> DecodedFrame {
    DecodedFrame {
        generation,
        pts,
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        width: 640,
        height: 360,
        render_width: 640,
        render_height: 360,
        display_orientation: codec_core::VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle: FrameResourceHandle(resource_handle),
        diagnostics: video_core::VideoFrameDiagnostics::default(),
    }
}

/// Создаёт минимальный input telemetry panel без запуска worker/renderer.
fn telemetry_panel_state_for_tests<'state>(
    player_snapshot: &'state PlayerSnapshot,
    telemetry: &'state Telemetry,
    render_diagnostics: &'state RenderDiagnostics,
    timeline_ui_state: &'state TimelineUiState,
    start_time: Instant,
) -> TelemetryPanelState<'state> {
    TelemetryPanelState {
        player_snapshot,
        telemetry,
        render_diagnostics,
        timeline_ui_state,
        backend_name: "test-backend",
        start_time,
        frame_duration_estimate_ms: 16.67,
    }
}

/// Ищет точный текст в cached строках telemetry panel.
fn telemetry_rows_contain(panel_rows: &[TelemetryPanelRow], expected_text: &str) -> bool {
    panel_rows
        .iter()
        .any(|panel_row| panel_row.text() == expected_text)
}

/// Проверяет, что click-to-seek уходит в exact seek route без scrub policy.
#[test]
fn timeline_click_seek_maps_to_exact_player_seek_route() {
    let target_position = MediaTime::from_secs(42);

    let (command, route) = timeline_command_from_action(TimelineAction::ClickSeek(target_position))
        .expect("click seek must map to a player command");

    assert_eq!(route.as_str(), "click-seek");
    assert_eq!(
        command,
        PlayerCommand::Seek(SeekRequest::absolute(target_position))
    );
}

/// Проверяет, что drag release уходит в exact seek route без scrub policy.
#[test]
fn timeline_drag_release_maps_to_exact_player_seek_route() {
    let target_position = MediaTime::from_secs(64);

    let (command, route) =
        timeline_command_from_action(TimelineAction::CommitDragSeek(target_position))
            .expect("drag seek must map to a player command");

    assert_eq!(route.as_str(), "drag-seek");
    assert_eq!(
        command,
        PlayerCommand::Seek(SeekRequest::absolute(target_position))
    );
}

/// Live scrub actions не должны случайно пройти legacy helper как ordinary Seek.
#[test]
fn timeline_live_scrub_actions_do_not_map_to_exact_seek_route() {
    let target_position = MediaTime::from_secs(12);

    for action in [
        TimelineAction::BeginLiveScrub(target_position),
        TimelineAction::PreviewLiveScrub(target_position),
        TimelineAction::EndLiveScrubAtLatestTarget(target_position),
        TimelineAction::EndLiveScrubAtVisiblePreview(target_position),
        TimelineAction::CancelLiveScrub,
    ] {
        assert_eq!(timeline_command_from_action(action), None);
    }
}

/// Короткий click и настоящий drag сохраняют разные release semantics на app boundary.
#[test]
fn timeline_live_scrub_release_actions_map_to_intended_commit_policies() {
    let target_position = MediaTime::from_secs(12);

    assert_eq!(
        live_scrub_release_policy_from_action(TimelineAction::EndLiveScrubAtLatestTarget(
            target_position,
        )),
        Some(ScrubCommitPolicy::CommitLatestTarget)
    );
    assert_eq!(
        live_scrub_release_policy_from_action(TimelineAction::EndLiveScrubAtVisiblePreview(
            target_position,
        )),
        Some(ScrubCommitPolicy::CommitVisiblePreview)
    );
}

/// Playback-rate UI разрешён в Paused: скорость применится при следующем Play без движения времени.
#[test]
fn playback_rate_ui_allows_paused_state() {
    let mut player_snapshot = PlayerSnapshot::empty();
    player_snapshot.playback_state = PlaybackState::Paused;

    let command = playback_rate_command_from_action(
        &player_snapshot,
        &ControlAction::AdjustPlaybackRateSteps(1),
    )
    .expect("paused playback-rate action must map to command");

    assert_eq!(
        command,
        PlayerCommand::SetPlaybackRate(PlaybackRate::new(1.10).expect("valid test rate"))
    );
}

/// Buffering намеренно пропускается, даже если он пришёл из Playing.
#[test]
fn playback_rate_ui_skips_buffering_state() {
    let mut player_snapshot = PlayerSnapshot::empty();
    player_snapshot.playback_state = PlaybackState::Buffering;

    assert_eq!(
        playback_rate_command_from_action(
            &player_snapshot,
            &ControlAction::AdjustPlaybackRateSteps(1),
        ),
        None
    );
}

/// Reset на `1.0x` не должен слать повторный no-op command.
#[test]
fn playback_rate_ui_skips_normal_reset_noop() {
    let mut player_snapshot = PlayerSnapshot::empty();
    player_snapshot.playback_state = PlaybackState::Playing;
    player_snapshot.playback_rate = PlaybackRate::NORMAL;

    assert_eq!(
        playback_rate_command_from_action(&player_snapshot, &ControlAction::ResetPlaybackRate),
        None
    );
}

/// Явный reset из non-1x остаётся единственным UI-путём к NORMAL rate.
#[test]
fn playback_rate_ui_explicit_reset_selects_normal_rate() {
    let mut player_snapshot = PlayerSnapshot::empty();
    player_snapshot.playback_state = PlaybackState::Playing;
    player_snapshot.playback_rate = PlaybackRate::new(1.50).expect("valid test rate");

    let command =
        playback_rate_command_from_action(&player_snapshot, &ControlAction::ResetPlaybackRate)
            .expect("explicit reset from non-normal rate must send one command");

    assert_eq!(
        command,
        PlayerCommand::SetPlaybackRate(PlaybackRate::NORMAL)
    );
}

/// Incremental decrease перепрыгивает 1x, не запуская закрытие reset-кнопки.
#[test]
fn playback_rate_ui_decrease_skips_normal_rate() {
    let mut player_snapshot = PlayerSnapshot::empty();
    player_snapshot.playback_state = PlaybackState::Playing;
    player_snapshot.playback_rate = PlaybackRate::new(1.10).expect("valid test rate");

    let command = playback_rate_command_from_action(
        &player_snapshot,
        &ControlAction::AdjustPlaybackRateSteps(-1),
    )
    .expect("decrease across normal rate must remain an applied adjustment");

    assert_eq!(
        command,
        PlayerCommand::SetPlaybackRate(PlaybackRate::new(0.90).expect("valid test rate"))
    );
}

/// Incremental increase симметрично перепрыгивает 1x снизу вверх.
#[test]
fn playback_rate_ui_increase_skips_normal_rate() {
    let mut player_snapshot = PlayerSnapshot::empty();
    player_snapshot.playback_state = PlaybackState::Playing;
    player_snapshot.playback_rate = PlaybackRate::new(0.90).expect("valid test rate");

    let command = playback_rate_command_from_action(
        &player_snapshot,
        &ControlAction::AdjustPlaybackRateSteps(1),
    )
    .expect("increase across normal rate must remain an applied adjustment");

    assert_eq!(
        command,
        PlayerCommand::SetPlaybackRate(PlaybackRate::new(1.10).expect("valid test rate"))
    );
}

/// Multi-step wheel batch также не может закончиться ровно на 1x.
#[test]
fn playback_rate_ui_multi_step_landing_skips_normal_rate() {
    let mut player_snapshot = PlayerSnapshot::empty();
    player_snapshot.playback_state = PlaybackState::Playing;
    player_snapshot.playback_rate = PlaybackRate::new(1.20).expect("valid test rate");

    let command = playback_rate_command_from_action(
        &player_snapshot,
        &ControlAction::AdjustPlaybackRateSteps(-2),
    )
    .expect("multi-step landing on normal rate must skip it");

    assert_eq!(
        command,
        PlayerCommand::SetPlaybackRate(PlaybackRate::new(0.90).expect("valid test rate"))
    );
}

/// Clamp на верхней границе не должен повторно отправлять no-op command.
#[test]
fn playback_rate_ui_skips_clamped_edge_noop() {
    let mut player_snapshot = PlayerSnapshot::empty();
    player_snapshot.playback_state = PlaybackState::Playing;
    player_snapshot.playback_rate = PlaybackRate::MAX;

    assert_eq!(
        playback_rate_command_from_action(
            &player_snapshot,
            &ControlAction::AdjustPlaybackRateSteps(1),
        ),
        None
    );
}

/// UI-шаги квантуются по 0.10x и clamp-ятся в public `PlaybackRate` range.
#[test]
fn playback_rate_ui_clamps_adjustment_to_public_range() {
    let mut player_snapshot = PlayerSnapshot::empty();
    player_snapshot.playback_state = PlaybackState::Playing;
    player_snapshot.playback_rate = PlaybackRate::new(3.95).expect("valid test rate");

    let command = playback_rate_command_from_action(
        &player_snapshot,
        &ControlAction::AdjustPlaybackRateSteps(2),
    )
    .expect("clamped adjustment must still send one command");

    assert_eq!(command, PlayerCommand::SetPlaybackRate(PlaybackRate::MAX));
}

/// Проверяет, что UI diagnostics получает active path как renderer-neutral данные.
#[test]
fn ui_diagnostics_reads_active_color_path_without_gpu_handles() {
    let settings = ColorPipelineSettings::default();
    let active_path = ActiveColorPath::from_parts(
        VideoFramePixelLayout::Nv12,
        BitDepth::Eight,
        ChromaSubsampling::Yuv420,
        VideoColorMetadata::sdr_bt709_limited(),
        &settings,
    );
    let render_diagnostics = RenderDiagnostics {
        active_color_path: Some(active_path),
        hdr_reference_defaults: Some(HdrReferenceDefaultDiagnostics {
            mastering_max_luminance: HdrMetadataDiagnosticMarker::Confirmed,
            mastering_min_luminance: HdrMetadataDiagnosticMarker::Confirmed,
            max_content_light_level: HdrMetadataDiagnosticMarker::ReferenceDefault,
            max_frame_average_light_level: HdrMetadataDiagnosticMarker::ReferenceDefault,
        }),
        ..RenderDiagnostics::default()
    };

    assert_eq!(
        AppState::active_color_path_text_for_ui(&render_diagnostics).as_deref(),
        Some("NV12 8-bit BT.709 limited -> SDR BT.709 preserve-current-unorm")
    );
    assert_eq!(
        AppState::hdr_reference_defaults_text_for_ui(&render_diagnostics).as_deref(),
        Some(
            "mastering-max=confirmed, mastering-min=confirmed, maxcll=reference-default, maxfall=reference-default"
        )
    );
}

/// Проверяет, что telemetry panel не форматирует heavy diagnostics чаще refresh interval.
#[test]
fn telemetry_panel_cache_reuses_rows_until_refresh_deadline() {
    let mut telemetry_panel_cache = TelemetryPanelCache::default();
    let player_snapshot = PlayerSnapshot::empty();
    let telemetry = Telemetry::new();
    let render_diagnostics = RenderDiagnostics::default();
    let timeline_ui_state = TimelineUiState::default();
    let started_at = Instant::now();

    let initial_rows = telemetry_panel_cache.rows_for_frame(
        started_at,
        telemetry_panel_state_for_tests(
            &player_snapshot,
            &telemetry,
            &render_diagnostics,
            &timeline_ui_state,
            started_at,
        ),
    );
    assert!(telemetry_rows_contain(
        &initial_rows,
        "frames_presented_to_surface: 0"
    ));

    telemetry.record_frame_presented_to_surface();
    let rows_before_deadline = telemetry_panel_cache.rows_for_frame(
        started_at + TELEMETRY_PANEL_REFRESH_INTERVAL / 2,
        telemetry_panel_state_for_tests(
            &player_snapshot,
            &telemetry,
            &render_diagnostics,
            &timeline_ui_state,
            started_at,
        ),
    );
    assert!(Arc::ptr_eq(&initial_rows, &rows_before_deadline));
    assert!(telemetry_rows_contain(
        &rows_before_deadline,
        "frames_presented_to_surface: 0"
    ));

    let rows_after_deadline = telemetry_panel_cache.rows_for_frame(
        started_at + TELEMETRY_PANEL_REFRESH_INTERVAL,
        telemetry_panel_state_for_tests(
            &player_snapshot,
            &telemetry,
            &render_diagnostics,
            &timeline_ui_state,
            started_at,
        ),
    );
    assert!(!Arc::ptr_eq(&initial_rows, &rows_after_deadline));
    assert!(telemetry_rows_contain(
        &rows_after_deadline,
        "frames_presented_to_surface: 1"
    ));
}

/// Проверяет, что отсутствие файла не скрывает runtime diagnostics.
#[test]
fn telemetry_panel_rows_keep_empty_media_state_explicit() {
    let player_snapshot = PlayerSnapshot::empty();
    let telemetry = Telemetry::new();
    let render_diagnostics = RenderDiagnostics::default();
    let timeline_ui_state = TimelineUiState::default();
    let started_at = Instant::now();

    let panel_rows = AppState::build_telemetry_panel_rows(telemetry_panel_state_for_tests(
        &player_snapshot,
        &telemetry,
        &render_diagnostics,
        &timeline_ui_state,
        started_at,
    ));

    assert!(!telemetry_rows_contain(&panel_rows, "[Media Info]"));
    assert!(!telemetry_rows_contain(&panel_rows, "No file loaded"));
    assert!(telemetry_rows_contain(&panel_rows, "[Swapchain]"));
    assert!(telemetry_rows_contain(&panel_rows, "[Video]"));
}

/// Проверяет explicit telemetry mapping нового public Scrubbing state.
#[test]
fn telemetry_panel_rows_label_scrubbing_playback_state() {
    let mut player_snapshot = PlayerSnapshot::empty();
    player_snapshot.playback_state = PlaybackState::Scrubbing;
    let telemetry = Telemetry::new();
    let render_diagnostics = RenderDiagnostics::default();
    let timeline_ui_state = TimelineUiState::default();
    let started_at = Instant::now();

    let panel_rows = AppState::build_telemetry_panel_rows(telemetry_panel_state_for_tests(
        &player_snapshot,
        &telemetry,
        &render_diagnostics,
        &timeline_ui_state,
        started_at,
    ));

    assert!(telemetry_rows_contain(
        &panel_rows,
        "Playback state: Scrubbing"
    ));
}

/// Проверяет, что telemetry читает player-owned frame-server diagnostics boundary.
#[test]
fn telemetry_panel_rows_map_frame_server_diagnostics() {
    let mut player_scrub_recorder = ScrubDiagnosticsRecorder::new();
    let old_live_scrub_settings = LiveScrubSettingsSnapshot {
        decode_mode: LiveScrubDecodeMode::ThrottledLatest,
        max_hz: 60,
    };
    let new_live_scrub_settings = LiveScrubSettingsSnapshot {
        decode_mode: LiveScrubDecodeMode::EveryDragEvent,
        max_hz: 120,
    };
    let mut live_scrub = LiveScrubDiagnostics::from_settings_snapshot(old_live_scrub_settings);

    live_scrub.record_deferred_settings_change(DeferredLiveScrubSettingsChange {
        old_snapshot: old_live_scrub_settings,
        new_snapshot: new_live_scrub_settings,
    });
    live_scrub.record_throttled_latest_skip();
    player_scrub_recorder.record_event_diagnostics(
        frame_server_core::ScrubEventDiagnostics::new(
            frame_server_core::ScrubDriverOutcomeKind::Progressed,
        )
        .with_live_scrub(live_scrub),
    );

    let mut player_snapshot = PlayerSnapshot::empty();
    player_snapshot.diagnostics.frame_server_scrub = player_scrub_recorder.snapshot();

    let telemetry = Telemetry::new();
    let render_diagnostics = RenderDiagnostics::default();
    let timeline_ui_state = TimelineUiState::default();
    let started_at = Instant::now();

    let panel_rows = AppState::build_telemetry_panel_rows(telemetry_panel_state_for_tests(
        &player_snapshot,
        &telemetry,
        &render_diagnostics,
        &timeline_ui_state,
        started_at,
    ));

    assert!(telemetry_rows_contain(&panel_rows, "[Frame Server]"));
    assert!(telemetry_rows_contain(
        &panel_rows,
        "FS player requests: seek=0 live=0"
    ));
    assert!(telemetry_rows_contain(
        &panel_rows,
        "FS player outcomes: cold_progress=0 exact_ready=0 resume_pending=0 audio_timeout=0 audio_error=0 cancelled=0 stale=0 timeout=0 fatal=0"
    ));
    assert!(telemetry_rows_contain(
        &panel_rows,
        "FS player backpressure: demux_unsupported=0 demux_unavailable=0 decoder=0 host_upload=0 resource_busy=0"
    ));
    assert!(telemetry_rows_contain(
        &panel_rows,
        "FS live scrub: mode=throttled_latest max_hz=60 deferred_changes=1 latest_change=throttled_latest / 60hz -> every_drag_event / 120hz throttle_skips=1"
    ));
}

/// Проверяет, что app shell не читает внутренний present frame из player pipeline.
#[test]
fn app_egui_does_not_access_pipeline_present_video_frame_directly() {
    let forbidden_member = concat!("pipeline", ".", "present_video_frame");

    assert!(!state_source_for_architecture_tests().contains(forbidden_member));
    assert!(!include_str!("../main.rs").contains(forbidden_member));
    assert!(!include_str!("../app_shell/mod.rs").contains(forbidden_member));
}

/// Фиксирует, что startup не обходит selector прямым VAAPI fallback-ом.
#[test]
fn video_pipeline_rebuild_stops_before_vaapi_startup_when_selector_rejects() {
    let state_source = state_source_for_architecture_tests();
    let rebuild_section = source_section_between(
        &state_source,
        "pub(crate) fn rebuild_video_pipeline_with_decoder_config",
        "/// Возвращает WGPU materializer текущего concrete video backend-а.",
    );
    let selector_error_return = rebuild_section
        .find("video pipeline selection failed: {error}")
        .expect("rebuild должен возвращать selection error через ? до backend startup");
    let vaapi_factory_start = rebuild_section
        .find("VaapiVideoBackendFactory::new_with_decoder_config")
        .expect("rebuild должен сохранять текущий VAAPI startup plan после selector");

    assert!(
        selector_error_return < vaapi_factory_start,
        "VAAPI startup должен быть недостижим, когда selector вернул ошибку"
    );
    assert!(
        !rebuild_section.contains("or_else"),
        "rebuild не должен добавлять fallback startup после ошибки selector"
    );
}

#[test]
fn app_layout_uses_sidebar_instead_of_floating_settings_window() {
    let state_source = state_source_for_architecture_tests();
    let render_ui_section = source_section_between(
        &state_source,
        "pub fn render_ui(",
        "let egui_run_elapsed = egui_run_started_at.elapsed();",
    );

    assert!(
        render_ui_section.contains("sidebar::show"),
        "AppState::render_ui должен рисовать настройки через app sidebar"
    );
    assert!(
        !render_ui_section.contains("settings_window::show"),
        "AppState::render_ui не должен возвращать floating settings window"
    );
}

#[test]
fn app_layout_shrinks_video_viewport_by_sidebar_without_exclusion_rects() {
    let state_source = state_source_for_architecture_tests();
    let render_ui_section = source_section_between(
        &state_source,
        "pub fn render_ui(",
        "let egui_run_elapsed = egui_run_started_at.elapsed();",
    );

    assert!(
        render_ui_section.contains("video_viewport_rect = ui.max_rect();"),
        "video underlay должен начинаться с полного egui root rect"
    );
    assert!(
        render_ui_section.contains("video_viewport_rect.min.x = sidebar_output"),
        "sidebar должен вытеснять видео: viewport начинается от правого края панели"
    );
    assert!(
        !render_ui_section.contains("video_exclusion_rects.push("),
        "sidebar больше не должен становиться exclusion rect: он сжимает viewport"
    );
}

/// Фиксирует явную границу refresh/publish вместо getter-like API с side effects.
#[test]
fn app_state_player_snapshot_boundary_stays_explicit() {
    let state_source = state_source_for_architecture_tests();
    let frame_prepare_source = include_str!("../frame_prepare.rs");
    let desktop_owner_source = include_str!("../playlist_runtime/desktop_transport.rs");
    let removed_getter_signature = concat!("fn ", "player_snapshot", "(&mut self)");
    let refresh_signature = concat!(
        "pub(crate) fn ",
        "refresh_player_snapshot",
        "(&mut self) -> PlayerSnapshot"
    );
    let publish_signature =
        "pub(crate) fn publish_desktop_snapshot(&mut self, snapshot: &PlayerSnapshot)";

    assert!(
        !state_source.contains(removed_getter_signature),
        "AppState не должен возвращать player snapshot через mutable getter-like API"
    );
    assert!(
        state_source.contains(refresh_signature),
        "AppState должен явно читать worker snapshot через refresh_player_snapshot()"
    );
    assert!(
        !state_source.contains("desktop_integration: Option<DesktopIntegration>")
            && !state_source.contains("DesktopIntegration::spawn"),
        "renderer-lifetime AppState не должен владеть desktop integration"
    );

    let refresh_section = source_section_between(
        &state_source,
        refresh_signature,
        concat!("pub fn ", "wants_continuous_redraw", "(&self) -> bool"),
    );
    assert!(
        !refresh_section.contains("publish_desktop_snapshot"),
        "refresh_player_snapshot() не должен публиковать desktop state"
    );
    assert!(
        !refresh_section.contains("desktop_integration"),
        "refresh_player_snapshot() не должен напрямую трогать desktop integration"
    );

    assert!(
        desktop_owner_source.contains(publish_signature),
        "process-lifetime PlaylistRuntime должен явно публиковать desktop snapshot"
    );
    assert!(
        !desktop_owner_source.contains("PlayerCommand"),
        "desktop owner не должен создавать direct player command bypass"
    );

    let input_prepare_position = frame_prepare_source
        .find("prepare_frame_input(telemetry, window, renderer, app_state, &mut frame_sequence)")
        .expect("render_frame должен делегировать input snapshot отдельному adapter-у");
    let publish_position = frame_prepare_source
        .find("playlist_runtime.publish_desktop_snapshot(frame_context.player_snapshot());")
        .expect("frame adapter должен передать уже зафиксированный snapshot process owner-у");
    let ui_prepare_position = frame_prepare_source
        .find("let mut prepared_ui_frame = prepare_ui_frame(")
        .expect("render_frame должен готовить UI через тот же AppFrameContext");

    assert!(
        input_prepare_position < publish_position && publish_position < ui_prepare_position,
        "process owner должен публиковать тот же AppFrameContext до UI/render подготовки"
    );
}

/// Фиксирует production wiring EOF snapshot после explicit UI intents и до deferred actions.
#[test]
fn render_frame_routes_player_snapshot_into_playlist_automatic_lifecycle() {
    let frame_prepare_source = include_str!("../frame_prepare.rs");
    let render_frame_section = source_section_between(
        frame_prepare_source,
        "pub(crate) fn render_frame(",
        "app_state.poll_playlist_transport(playlist_runtime, renderer);",
    );

    let explicit_transport_position = render_frame_section
        .find("crate::transport_runtime::apply_transport_actions(")
        .expect("render frame должен сначала применить explicit transport intents");
    let automatic_snapshot_position = render_frame_section
        .find("crate::transport_runtime::apply_playlist_automatic_snapshot(")
        .expect("render frame должен передать player snapshot automatic lifecycle owner-у");
    let deferred_action_position = render_frame_section
        .find("crate::transport_runtime::apply_discovery_navigation_action(")
        .expect("render frame должен затем исполнить deferred discovery action");

    assert!(
        explicit_transport_position < automatic_snapshot_position
            && automatic_snapshot_position < deferred_action_position,
        "explicit intent должен иметь приоритет, а EOF observation — предшествовать deferred drain"
    );
    assert!(
        render_frame_section[automatic_snapshot_position..deferred_action_position]
            .contains("frame_context.player_snapshot()"),
        "automatic lifecycle должен получить тот же immutable snapshot текущего frame-а"
    );
}

/// Фиксирует, что live-смена настроек пересобирает video pipeline с учётом
/// requirement активного стрима, а не вслепую (`None`). Иначе `auto` для
/// software-only кодека (AV1) выбрал бы hardware backend, который железо не
/// тянет, и decoder thread сразу падал бы с "Decoder thread disconnected".
#[test]
fn live_settings_rebuild_passes_active_stream_requirement() {
    let state_source = state_source_for_architecture_tests();
    let frame_prepare_source = include_str!("../frame_prepare/settings_runtime_adapter.rs");

    // AppState кэширует requirement активного стрима и отдаёт его по boundary-методу.
    assert!(
        state_source.contains("active_video_stream_requirement: Option<VideoDecodeRequirement>"),
        "AppState должен хранить requirement активного video-стрима для live-rebuild"
    );
    assert!(
        state_source.contains(concat!(
            "pub(crate) fn ",
            "active_video_stream_requirement",
            "(&self) -> Option<&VideoDecodeRequirement>"
        )),
        "AppState должен отдавать requirement активного стрима через boundary-метод"
    );

    // Live-путь применения runtime-настроек обязан прокинуть этот requirement в rebuild.
    let apply_section = source_section_between(
        frame_prepare_source,
        "fn apply_player_runtime_settings(",
        "fn apply_media_service_runtime_settings(",
    );
    assert!(
        apply_section.contains("self.app_state.active_video_stream_requirement()"),
        "live-rebuild должен брать requirement активного стрима, а не выбирать backend вслепую"
    );
    assert!(
            !apply_section.contains("rebuild_video_pipeline_with_decoder_config(\n                decoder_thread_config,\n                None,"),
            "live-rebuild не должен передавать None как stream_requirement"
        );
}

/// App-owned media lifecycle обязан закрывать settings boundary ещё до player staging.
///
/// Полный `AppState` требует real window/audio worker и не имеет headless constructor-а;
/// поэтому app-layer guard фиксируется на production boundary source, а worker-side
/// Pending/Ready поведение отдельно проходит functional session tests.
#[test]
fn app_runtime_reconfigure_boundary_checks_pre_player_media_lifecycle_first() {
    let state_source = include_str!("../state.rs");
    let boundary_section = source_section_between(
        state_source,
        "pub(crate) fn runtime_reconfigure_boundary_activity(",
        "pub(crate) fn current_decoder_thread_config(",
    );

    let strong_open_check = boundary_section
        .find("self.has_pending_prepared_media_strong()")
        .expect("settings boundary должен проверять app-owned strong media open");
    let suspended_resume_check = boundary_section
        .find("self.has_pending_suspended_media_resume()")
        .expect("settings boundary должен проверять app-owned suspended resume");
    let worker_query = boundary_section
        .find("self.player_worker.runtime_reconfigure_boundary_activity()")
        .expect("после app-owned gate boundary должен спросить worker owner-а");

    assert!(
        strong_open_check < worker_query && suspended_resume_check < worker_query,
        "pre-player app owners должны закрывать boundary до worker query"
    );
    assert!(
        boundary_section.contains("PlayerRuntimeBoundaryActivity::PipelineLifecycle"),
        "app-owned media lifecycle должен возвращать typed PipelineLifecycle activity"
    );
}

#[test]
fn playlist_transport_routes_only_pre_barrier_failure_through_navigation_owner() {
    let transport_source = include_str!("playlist_transport.rs");
    let failed_branch = source_section_between(
        transport_source,
        "StrongMediaOpenPoll::Failed(error) => {",
        "/// Неблокирующе отправляет latest Clear reset",
    );

    assert!(
        failed_branch.contains(".allows_navigation_failure_recovery()"),
        "automatic continuation разрешён только до barrier-а либо после exact compensation"
    );
    assert!(
        failed_branch.contains("report_playlist_navigation_failure("),
        "terminal playlist failure должен вернуться controller-owned navigation owner-у"
    );
    assert!(
        failed_branch.contains("if let Some(install) = automatic_continuation"),
        "controller-owned automatic continuation должен запускать следующий strong install"
    );
}

/// Проверяет, что AppFrameContext отдаёт уже зафиксированный snapshot по ссылке.
#[test]
fn app_frame_context_returns_fixed_player_snapshot_reference() {
    let frame_context = AppFrameContext {
        player_snapshot: PlayerSnapshot::empty(),
        render_diagnostics: RenderDiagnostics::default(),
    };

    assert!(std::ptr::eq(
        frame_context.player_snapshot(),
        &frame_context.player_snapshot
    ));
}

/// Проверяет pure-classifier stale cache без создания GPU lease-а.
#[test]
fn cached_present_frame_validation_rejects_stale_lifecycle_identity() {
    let valid_state = CachedPresentFrameValidationState {
        current_video_frame_present: true,
        source_matches: true,
        cached_generation: 7,
        current_generation: 7,
    };

    assert_eq!(cached_present_frame_stale_reason(valid_state), None);
    assert_eq!(
        cached_present_frame_stale_reason(CachedPresentFrameValidationState {
            current_video_frame_present: false,
            ..valid_state
        }),
        Some(CachedPresentFrameDiscardReason::CurrentVideoFrameMissing)
    );
    assert_eq!(
        cached_present_frame_stale_reason(CachedPresentFrameValidationState {
            source_matches: false,
            ..valid_state
        }),
        Some(CachedPresentFrameDiscardReason::SourceLabelChanged)
    );
    assert_eq!(
        cached_present_frame_stale_reason(CachedPresentFrameValidationState {
            current_generation: 8,
            ..valid_state
        }),
        Some(CachedPresentFrameDiscardReason::RenderGenerationChanged)
    );
}

/// Проверяет, что player lifecycle events инвалидируют cache только на boundary-событиях.
#[test]
fn player_lifecycle_events_invalidate_cached_present_frame_at_boundaries() {
    let media_open_request =
        MediaOpenRequest::new(MediaSource::ExternalLabel("next-source".to_string()), true);
    let media_summary = MediaSummary {
        title: None,
        source_label: "next-source".to_string(),
        duration: None,
    };
    let fatal_error = PlayerError::new(PlayerErrorKind::RenderDeviceLost, "device lost");

    assert_eq!(
        cached_present_frame_discard_reason_for_player_event(&PlayerEvent::MediaOpenRequested(
            media_open_request
        )),
        Some(CachedPresentFrameDiscardReason::PlayerMediaOpenRequested)
    );
    assert_eq!(
        cached_present_frame_discard_reason_for_player_event(&PlayerEvent::MediaOpened(
            media_summary
        )),
        Some(CachedPresentFrameDiscardReason::PlayerMediaOpened)
    );
    assert_eq!(
        cached_present_frame_discard_reason_for_player_event(&PlayerEvent::PlaybackStateChanged(
            PlaybackState::Stopped
        )),
        Some(CachedPresentFrameDiscardReason::PlayerStopped)
    );
    assert_eq!(
        cached_present_frame_discard_reason_for_player_event(&PlayerEvent::PlaybackStateChanged(
            PlaybackState::Failed
        )),
        Some(CachedPresentFrameDiscardReason::PlayerFailed)
    );
    assert_eq!(
        cached_present_frame_discard_reason_for_player_event(&PlayerEvent::FatalError(fatal_error)),
        Some(CachedPresentFrameDiscardReason::PlayerFatalError)
    );
    assert_eq!(
        cached_present_frame_discard_reason_for_player_event(&PlayerEvent::PlaybackStateChanged(
            PlaybackState::Paused
        )),
        None
    );
    assert_eq!(
        cached_present_frame_discard_reason_for_player_event(&PlayerEvent::SeekCommitted(
            SeekCommitInfo {
                target_position: Duration::from_secs(12),
                actual_position: Duration::from_secs(12),
                resume_intent: PlaybackResumeIntent::Pause,
            }
        )),
        None
    );
}

#[test]
fn exact_clear_cleanup_rejects_newer_snapshot_and_accepts_reset_or_absent_instance() {
    let reset_instance = player_core::MediaInstanceId::from_non_zero(
        NonZeroU64::new(701).expect("test instance is non-zero"),
    );
    let newer_instance = player_core::MediaInstanceId::from_non_zero(
        NonZeroU64::new(702).expect("test instance is non-zero"),
    );

    assert_eq!(
        super::media_jobs::classify_exact_media_reset_cleanup(Some(newer_instance), reset_instance,),
        super::media_jobs::ExactMediaResetCleanup::SupersededByNewSnapshot
    );
    assert_eq!(
        super::media_jobs::classify_exact_media_reset_cleanup(Some(reset_instance), reset_instance,),
        super::media_jobs::ExactMediaResetCleanup::Cleared
    );
    assert_eq!(
        super::media_jobs::classify_exact_media_reset_cleanup(None, reset_instance),
        super::media_jobs::ExactMediaResetCleanup::Cleared
    );
}

/// Player media-open events пересоздают playback session, но не должны стирать
/// source identity, которую `AppState::remember_active_media_source` уже отдал
#[test]
fn present_frame_identity_distinguishes_decoded_generation_and_pts() {
    let previous_frame = decoded_frame_for_identity_tests(10, Duration::from_millis(1_000), 42);
    let next_generation_frame =
        decoded_frame_for_identity_tests(11, Duration::from_millis(1_000), 42);
    let next_pts_frame = decoded_frame_for_identity_tests(10, Duration::from_millis(1_033), 42);

    let previous_identity = PresentFrameIdentity::from_decoded_frame(7, &previous_frame);

    assert_ne!(
        previous_identity,
        PresentFrameIdentity::from_decoded_frame(7, &next_generation_frame)
    );
    assert_ne!(
        previous_identity,
        PresentFrameIdentity::from_decoded_frame(7, &next_pts_frame)
    );
}

/// Проверяет pure decision для texture-view Busy fallback-а.
#[test]
fn texture_busy_fallback_reuses_valid_previous_frame() {
    let valid_previous_frame = TextureBusyFallbackReuseState {
        cached_generation: 5,
        current_generation: 5,
        source_matches: true,
        has_current_video_frame: true,
        cached_frame_is_stale: false,
        timeline_marks_frame_stale: false,
    };

    assert!(texture_busy_fallback_can_reuse_previous_frame(
        valid_previous_frame
    ));
    assert_eq!(
        texture_busy_fallback_reject_reason(valid_previous_frame),
        None
    );
}

/// Проверяет, что active seek stale markers не запрещают визуальный Busy placeholder.
#[test]
fn texture_busy_fallback_reuses_previous_frame_while_seek_target_is_pending_and_stale() {
    let stale_seek_visual_placeholder = TextureBusyFallbackReuseState {
        cached_generation: 5,
        current_generation: 5,
        source_matches: true,
        has_current_video_frame: true,
        cached_frame_is_stale: true,
        timeline_marks_frame_stale: true,
    };

    assert!(texture_busy_fallback_can_reuse_previous_frame(
        stale_seek_visual_placeholder
    ));
    assert_eq!(
        texture_busy_fallback_reject_reason(stale_seek_visual_placeholder),
        None
    );
}

/// Проверяет, что timeline stale marker сам по себе не приводит к black clear на Busy.
#[test]
fn texture_busy_fallback_reuses_previous_frame_when_timeline_marks_frame_stale() {
    let timeline_stale_visual_placeholder = TextureBusyFallbackReuseState {
        cached_generation: 5,
        current_generation: 5,
        source_matches: true,
        has_current_video_frame: true,
        cached_frame_is_stale: false,
        timeline_marks_frame_stale: true,
    };

    assert!(texture_busy_fallback_can_reuse_previous_frame(
        timeline_stale_visual_placeholder
    ));
    assert_eq!(
        texture_busy_fallback_reject_reason(timeline_stale_visual_placeholder),
        None
    );
}

/// Проверяет, что Busy fallback различает lifecycle причины отказа.
#[test]
fn texture_busy_fallback_rejects_stale_lifecycle_identity() {
    let valid_previous_frame = TextureBusyFallbackReuseState {
        cached_generation: 5,
        current_generation: 5,
        source_matches: true,
        has_current_video_frame: true,
        cached_frame_is_stale: false,
        timeline_marks_frame_stale: false,
    };
    let stale_visual_placeholder = TextureBusyFallbackReuseState {
        cached_frame_is_stale: true,
        timeline_marks_frame_stale: true,
        ..valid_previous_frame
    };

    assert!(!texture_busy_fallback_can_reuse_previous_frame(
        TextureBusyFallbackReuseState {
            current_generation: 6,
            ..stale_visual_placeholder
        }
    ));
    assert_eq!(
        texture_busy_fallback_reject_reason(TextureBusyFallbackReuseState {
            current_generation: 6,
            ..stale_visual_placeholder
        }),
        Some(TextureBusyFallbackRejectReason::RenderGenerationChanged)
    );
    assert!(!texture_busy_fallback_can_reuse_previous_frame(
        TextureBusyFallbackReuseState {
            source_matches: false,
            ..stale_visual_placeholder
        }
    ));
    assert_eq!(
        texture_busy_fallback_reject_reason(TextureBusyFallbackReuseState {
            source_matches: false,
            ..stale_visual_placeholder
        }),
        Some(TextureBusyFallbackRejectReason::SourceLabelChanged)
    );
    assert!(!texture_busy_fallback_can_reuse_previous_frame(
        TextureBusyFallbackReuseState {
            has_current_video_frame: false,
            ..stale_visual_placeholder
        }
    ));
    assert_eq!(
        texture_busy_fallback_reject_reason(TextureBusyFallbackReuseState {
            has_current_video_frame: false,
            ..stale_visual_placeholder
        }),
        Some(TextureBusyFallbackRejectReason::CurrentVideoFrameMissing)
    );
}
