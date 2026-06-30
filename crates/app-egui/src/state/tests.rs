use std::sync::Arc;
use std::time::{Duration, Instant};

use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata};
use media_core::MediaTime;
use player_core::{
    MediaOpenRequest, MediaSource, MediaSummary, PlaybackResumeIntent, PlaybackState,
    PlayerCommand, PlayerError, PlayerErrorKind, PlayerEvent, PlayerSnapshot, SeekCommitInfo,
    SeekRequest,
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
    control_action_cancels_timeline_hover_leave_grace,
    control_actions_include_timeline_pointer_target, raw_input_has_primary_pointer_press,
    timeline_command_from_action, timeline_hover_prepare_allows_preview_borrow,
    timeline_hover_prepare_playback_mode,
};
use super::{AppFrameContext, AppState};
use crate::telemetry::Telemetry;
use crate::timeline_hover_intent::{
    TimelineHoverFrameCoalescer, TimelineHoverIntentState, TimelineHoverPreviewSlot,
};
use crate::timeline_hover_prepare::TimelineHoverPreparePlaybackMode;
use crate::ui::player_controls::ControlAction;
use crate::ui::timeline::{
    TimelineAction, TimelineHoverIntent, TimelineHoverPreviewPlacement, TimelineHoverTarget,
    TimelineHoverVisualTarget, TimelineUiState,
};

fn hover_visual_target(seconds: u64) -> TimelineHoverVisualTarget {
    TimelineHoverVisualTarget::new(
        TimelineHoverTarget::new(MediaTime::from_secs(seconds)),
        TimelineHoverPreviewPlacement::new(
            egui::pos2(50.0, 20.0),
            egui::Rect::from_min_size(egui::pos2(0.0, 10.0), egui::vec2(100.0, 12.0)),
        ),
    )
}

/// Собирает source `state` и child-модулей для guard-тестов после split-а.
fn state_source_for_architecture_tests() -> String {
    [
        include_str!("../state.rs"),
        include_str!("media_jobs.rs"),
        include_str!("main_visual_override.rs"),
        include_str!("present_frame_cache.rs"),
        include_str!("telemetry_panel.rs"),
        include_str!("../timeline_hover_intent.rs"),
        include_str!("timeline_hover_leave_grace.rs"),
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
        TimelineAction::EndLiveScrub(target_position),
        TimelineAction::CancelLiveScrub,
    ] {
        assert_eq!(timeline_command_from_action(action), None);
    }
}

#[test]
fn timeline_hover_prepare_uses_player_owned_one_shot_resume_pending_mode() {
    let playback_mode = timeline_hover_prepare_playback_mode(
        PlaybackState::Scrubbing,
        player_core::TimelineHoverPrepareInteraction::OneShotSeekLandingResumePending {
            spare_capacity_available: true,
        },
    );

    assert_eq!(
        playback_mode,
        TimelineHoverPreparePlaybackMode::ResumePendingAfterSeek {
            spare_capacity_available: true,
        }
    );
}

#[test]
fn timeline_hover_prepare_uses_player_owned_live_scrub_mode() {
    let playback_mode = timeline_hover_prepare_playback_mode(
        PlaybackState::Playing,
        player_core::TimelineHoverPrepareInteraction::LiveScrubActive,
    );

    assert_eq!(
        playback_mode,
        TimelineHoverPreparePlaybackMode::LiveScrubActive
    );
    assert!(!timeline_hover_prepare_allows_preview_borrow(playback_mode));
}

#[test]
fn timeline_hover_preview_borrow_remains_allowed_during_one_shot_resume_pending() {
    let playback_mode = TimelineHoverPreparePlaybackMode::ResumePendingAfterSeek {
        spare_capacity_available: false,
    };

    assert!(timeline_hover_prepare_allows_preview_borrow(playback_mode));
}

/// Hover intent не входит в command-oriented `TimelineAction -> PlayerCommand` route.
#[test]
fn timeline_hover_intent_has_no_player_command_surface() {
    let hover_source = include_str!("../timeline_hover_intent.rs");

    assert!(!hover_source.contains("PlayerCommand"));
    assert!(!hover_source.contains("player_worker"));
}

#[test]
fn non_timeline_controls_cancel_pending_hover_leave_grace() {
    let target_position = MediaTime::from_secs(42);

    for action in [
        ControlAction::TogglePlayback,
        ControlAction::OpenFile,
        ControlAction::SetVolume(0.5),
        ControlAction::ToggleMute,
        ControlAction::ToggleFullscreen,
    ] {
        assert!(control_action_cancels_timeline_hover_leave_grace(&action));
    }
    assert!(!control_action_cancels_timeline_hover_leave_grace(
        &ControlAction::Timeline(TimelineAction::ClickSeek(target_position))
    ));
    assert!(!control_action_cancels_timeline_hover_leave_grace(
        &ControlAction::TimelineHover(TimelineHoverIntent::Clear)
    ));
}

#[test]
fn timeline_hover_passive_primary_click_without_timeline_interaction_cancels_leave_grace() {
    let target_position = MediaTime::from_secs(42);
    let mut egui_input = egui::RawInput::default();
    egui_input.events.push(egui::Event::PointerButton {
        pos: egui::pos2(12.0, 34.0),
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });

    assert!(raw_input_has_primary_pointer_press(&egui_input));
    assert!(!control_actions_include_timeline_pointer_target(&[]));
    assert!(control_actions_include_timeline_pointer_target(&[
        ControlAction::Timeline(TimelineAction::ClickSeek(target_position))
    ]));
    assert!(control_actions_include_timeline_pointer_target(&[
        ControlAction::TimelineHover(TimelineHoverIntent::Target(hover_visual_target(42)))
    ]));
    assert!(!control_actions_include_timeline_pointer_target(&[
        ControlAction::TimelineHover(TimelineHoverIntent::Clear)
    ]));
}

/// App-owned coalescer применяет только latest hover target одного UI frame-а.
#[test]
fn state_timeline_hover_updates_coalesce_to_latest_target() {
    let mut hover_state = TimelineHoverIntentState::default();
    let mut coalescer = TimelineHoverFrameCoalescer::default();
    let latest_target = TimelineHoverTarget::new(MediaTime::from_secs(90));

    coalescer.record(TimelineHoverIntent::Target(hover_visual_target(10)));
    coalescer.record(TimelineHoverIntent::Target(hover_visual_target(90)));
    let outcome = coalescer.finish(&mut hover_state, true);

    assert_eq!(hover_state.active_target(), Some(latest_target));
    assert_eq!(hover_state.invisible_prepare_target_count(), 1);
    assert_eq!(outcome.invisible_prepare_target, Some(latest_target));
    assert_eq!(
        outcome.visual_presentation_target,
        Some(hover_visual_target(90))
    );
}

/// `hover_preview_enabled=false` глушит только visual slot, но не invisible prepare intent.
#[test]
fn state_timeline_hover_preview_disabled_still_emits_invisible_prepare() {
    let mut hover_state = TimelineHoverIntentState::default();
    let mut coalescer = TimelineHoverFrameCoalescer::default();
    let target = TimelineHoverTarget::new(MediaTime::from_secs(55));

    coalescer.record(TimelineHoverIntent::Target(hover_visual_target(55)));
    let outcome = coalescer.finish(&mut hover_state, false);

    assert_eq!(hover_state.active_target(), Some(target));
    assert_eq!(
        hover_state.preview_slot(),
        TimelineHoverPreviewSlot::DisabledByConfig
    );
    assert_eq!(outcome.invisible_prepare_target, Some(target));
    assert_eq!(outcome.visual_presentation_target, None);
}

/// Hover state update не трогает public playback state snapshot.
#[test]
fn state_timeline_hover_does_not_change_paused_or_stopped_snapshot_state() {
    for playback_state in [PlaybackState::Paused, PlaybackState::Stopped] {
        let mut player_snapshot = PlayerSnapshot::empty();
        player_snapshot.playback_state = playback_state;

        let mut hover_state = TimelineHoverIntentState::default();
        let mut coalescer = TimelineHoverFrameCoalescer::default();
        coalescer.record(TimelineHoverIntent::Target(hover_visual_target(12)));
        coalescer.finish(&mut hover_state, true);

        assert_eq!(player_snapshot.playback_state, playback_state);
    }
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

/// Проверяет empty snapshot path без player/render side effects.
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

    assert!(telemetry_rows_contain(&panel_rows, "[Media Info]"));
    assert!(telemetry_rows_contain(&panel_rows, "No file loaded"));
    assert!(!telemetry_rows_contain(&panel_rows, "[Video]"));
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
        .find(".map_err(|error| format!(\"video pipeline selection failed: {error}\"))?;")
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
        render_ui_section.contains("video_viewport_rect.min.x = sidebar_rect"),
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
    let removed_getter_signature = concat!("fn ", "player_snapshot", "(&mut self)");
    let refresh_signature = concat!(
        "pub(crate) fn ",
        "refresh_player_snapshot",
        "(&mut self) -> PlayerSnapshot"
    );
    let publish_signature = concat!(
        "pub(crate) fn ",
        "publish_desktop_snapshot",
        "(&self, player_snapshot: &PlayerSnapshot)"
    );

    assert!(
        !state_source.contains(removed_getter_signature),
        "AppState не должен возвращать player snapshot через mutable getter-like API"
    );
    assert!(
        state_source.contains(refresh_signature),
        "AppState должен явно читать worker snapshot через refresh_player_snapshot()"
    );
    assert!(
        state_source.contains(publish_signature),
        "AppState должен явно публиковать desktop snapshot через publish_desktop_snapshot()"
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

    let publish_section = source_section_between(
        &state_source,
        publish_signature,
        concat!("fn ", "log_desktop_integration_events"),
    );
    assert!(
        !publish_section.contains("latest_snapshot"),
        "publish_desktop_snapshot() не должен читать worker snapshot"
    );
    assert!(
        !publish_section.contains("player_worker"),
        "publish_desktop_snapshot() не должен зависеть от worker storage"
    );

    let begin_frame_position = frame_prepare_source
        .find("let frame_context = app_state.begin_frame_context(renderer.diagnostics());")
        .expect("render_frame должен создавать AppFrameContext перед публикацией");
    let publish_position = frame_prepare_source
        .find("app_state.publish_desktop_snapshot(frame_context.player_snapshot());")
        .expect("render_frame должен явно публиковать snapshot текущего frame-а");
    let ui_prepare_position = frame_prepare_source
        .find("let mut prepared_ui_frame = prepare_ui_frame(")
        .expect("render_frame должен готовить UI через тот же AppFrameContext");

    assert!(
        begin_frame_position < publish_position && publish_position < ui_prepare_position,
        "render_frame должен публиковать snapshot из AppFrameContext до UI/render подготовки"
    );
}

/// Фиксирует, что live-смена настроек пересобирает video pipeline с учётом
/// requirement активного стрима, а не вслепую (`None`). Иначе `auto` для
/// software-only кодека (AV1) выбрал бы hardware backend, который железо не
/// тянет, и decoder thread сразу падал бы с "Decoder thread disconnected".
#[test]
fn live_settings_rebuild_passes_active_stream_requirement() {
    let state_source = state_source_for_architecture_tests();
    let frame_prepare_source = include_str!("../frame_prepare.rs");

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
        "self.app_state.apply_player_runtime_settings(update)",
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

/// Проверяет, что reuse identity различает новый decoded frame на той же texture.
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
