use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use fastiplayer_settings::{
    AppRouteApplyResult, MediaServiceRuntimeSettingsUpdate, PlayerCommittedSettingsUpdate,
    RenderCommittedSettingsUpdate, SettingsBoundaryActivity,
};
use player_core::{
    PlayerError, PlayerEvent, PlayerRenderError, PlayerRuntimeApplyError, PlayerRuntimeApplyGroup,
    PlayerRuntimeApplyGroupReport, PlayerRuntimeApplyReport, PlayerRuntimeApplyResult,
    PlayerRuntimeSettingsUpdate, PlayerSnapshot, PlayerTickResult, PlayerWorkerEvent,
};
use render_core::{
    RenderLiveApplyReport, RenderLiveSettings, RenderLiveSettingsAdapter, RenderLiveSettingsError,
    RenderLiveSettingsUpdate,
};
use render_shell_api::{
    FrameImportUnsupportedReason, RenderFrameDropReason, RenderFrameOutcome, RenderShell,
    RenderableFrameKey, VideoFrameImport,
};
use settings_core::SettingId;
use tracing::{error, instrument, warn};
use video_present_core::VideoPresentFrameIdentity;
use winit::window::{ResizeDirection, Window};

use crate::redraw_pacing::RedrawPacing;
use crate::renderer_recreation::{LiveRendererRecreation, RendererLifecycleCoordinator};
use crate::settings_runtime::{
    CommittedConfigSnapshot, SettingsRouteTargetPolicy, SettingsRuntime,
    SettingsRuntimeReconfigureHost,
};
use crate::state::{
    ActiveMediaSource, AppState, BackendSwapVideoPhase, MainVisualOverrideAcquisition,
    RenderablePresentFrame, VideoPipelineRebuildError, VideoPipelineRebuildRequest,
};
use crate::system_capabilities::probe_system_capabilities;
use crate::telemetry::{Telemetry, VideoFrameTelemetryEvent};
use crate::ui::window_chrome::{WindowChromeAction, WindowChromeResizeDirection};
use crate::user_audio_level::PlayerAudioObservation;
use media_source_open::video_codec_mapping::runtime_video_codec;

#[path = "frame_prepare/frame_decision.rs"]
mod frame_decision;
#[path = "frame_prepare/frame_work_plan.rs"]
mod frame_work_plan;
#[path = "frame_prepare/present.rs"]
mod present;
use frame_decision::{FrameDecisionInputs, decide_frame_work};
pub(crate) use frame_work_plan::{FrameWorkPlan, PresentReasons};
use present::{PresentedFrameTimings, present_frame_for_plan};
#[path = "frame_prepare/geometry.rs"]
mod geometry;
#[path = "frame_prepare/input_snapshot.rs"]
mod input_snapshot;
#[path = "frame_prepare/runtime_settings_persistence.rs"]
mod runtime_settings_persistence;
#[path = "frame_prepare/sequence.rs"]
mod sequence;
#[path = "frame_prepare/settings_runtime_adapter.rs"]
mod settings_runtime_adapter;
#[path = "frame_prepare/shared_frame_materialization.rs"]
mod shared_frame_materialization;
#[path = "frame_prepare/submit.rs"]
mod submit;
#[path = "frame_prepare/telemetry_mapping.rs"]
mod telemetry_mapping;
#[path = "frame_prepare/tick_telemetry.rs"]
mod tick_telemetry;
#[path = "frame_prepare/timing.rs"]
mod timing;
#[path = "frame_prepare/ui_prepare.rs"]
mod ui_prepare;
#[path = "frame_prepare/web_media_runtime.rs"]
mod web_media_runtime;
use input_snapshot::prepare_frame_input;
use runtime_settings_persistence::persist_due_runtime_originated_settings;
use sequence::{FrameSequenceContract, FrameSequenceObserver, FrameSequenceStage};
pub(crate) use settings_runtime_adapter::FrameSettingsRuntimeAdapter;
use shared_frame_materialization::{
    SharedMaterializationUnsupportedReason, SharedVideoFrameLeaseRole,
    SharedVideoFrameMaterializationOutcome, SharedVideoFrameMaterializationRequest,
    materialize_shared_video_frame,
};
use telemetry_mapping::map_video_frame_telemetry_event;
use timing::{FrameWorkTrace, VideoPrepareTimings, log_skipped_frame};
use ui_prepare::{advance_ui_timers, prepare_ui_frame_for_plan};

/// Результат полного render frame-а с shell-level запросами от window chrome.
pub(crate) struct AppRenderFrameResult {
    /// Redraw pacing после рендера кадра.
    pub(crate) pacing: RedrawPacing,

    /// Пользователь запросил закрытие через кастомный titlebar.
    pub(crate) close_requested: bool,

    /// Ближайшая event-driven UI смена без continuous redraw.
    pub(crate) next_ui_wake_deadline: Option<Instant>,
}

/// Выбирает ближайший deadline независимых UI owners для одного `WaitUntil`.
fn earliest_ui_wake_deadline<const N: usize>(deadlines: [Option<Instant>; N]) -> Option<Instant> {
    deadlines.into_iter().flatten().min()
}

/// Video stage, подготовленный до финального `render_shell_api::RenderFrameInput`.
pub(super) struct PreparedVideoFrame {
    /// Ключ рендера с арендой внутри: кадр не вернётся декодеру, пока ключ жив.
    renderable_frame: Option<RenderablePresentFrame>,

    /// Диагностическое имя acquisition state для tracing.
    acquisition_state: &'static str,

    /// Диагностическое имя texture-view lookup state.
    texture_view_lookup_state: &'static str,

    /// Подробная CPU-разбивка video prepare stage.
    timings: VideoPrepareTimings,
}

impl PreparedVideoFrame {
    /// Создаёт video stage без renderable кадра.
    fn empty(acquisition_state: &'static str) -> Self {
        Self {
            renderable_frame: None,
            acquisition_state,
            texture_view_lookup_state: "not_requested",
            timings: VideoPrepareTimings::default(),
        }
    }

    /// Создаёт video stage с уже удерживаемым render lease-ом.
    fn ready(renderable_frame: RenderablePresentFrame, acquisition_state: &'static str) -> Self {
        Self {
            renderable_frame: Some(renderable_frame),
            acquisition_state,
            texture_view_lookup_state: "ready",
            timings: VideoPrepareTimings::default(),
        }
    }

    /// Добавляет диагностическую информацию, собранную в `prepare_video_frame`.
    fn with_diagnostics(
        mut self,
        texture_view_lookup_state: &'static str,
        timings: VideoPrepareTimings,
    ) -> Self {
        self.texture_view_lookup_state = texture_view_lookup_state;
        self.timings = timings;
        self
    }

    /// Ключ видеокадра для входа `render_frame` (`None` — очистить target в чёрный).
    pub(super) fn render_input_video_frame(&self) -> Option<&RenderableFrameKey> {
        let renderable_frame = self.renderable_frame.as_ref()?;
        let present_frame = renderable_frame.present_frame();
        let decoded_frame = present_frame.decoded_frame();
        tracing::trace!(
            handle_id = decoded_frame.resource_handle.0,
            pts_ms = decoded_frame.pts.as_millis(),
            format = %decoded_frame.format(),
            memory_path = %decoded_frame.memory_path(),
            stale = present_frame.is_stale(),
            acquisition = self.acquisition_state,
            "Present frame acquired from playback worker"
        );
        Some(renderable_frame.key())
    }

    /// Отмечает, что подготовленный video frame реально попал в renderer submit.
    pub(super) fn mark_submitted_to_renderer(&self) {
        if let Some(renderable_frame) = &self.renderable_frame {
            renderable_frame
                .present_frame()
                .mark_submitted_to_renderer();
        }
    }

    /// Возвращает identity только текущего, не помеченного stale video lease-а.
    fn current_frame_identity(&self) -> Option<VideoPresentFrameIdentity> {
        let present_frame = &self.renderable_frame.as_ref()?.present_frame();
        if present_frame.is_stale() {
            return None;
        }
        Some(VideoPresentFrameIdentity::from_decoded_frame(
            present_frame.render_generation(),
            present_frame.decoded_frame(),
        ))
    }
}

/// Typed форма texture-view lookup-а без GPU handles для проверки app-level контракта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextureViewLookupKind {
    /// Texture views готовы для текущего present frame-а.
    Ready,

    /// Backend pool занят, render thread не должен ждать lock.
    Busy,

    /// Backend доступен, но resource для frame handle отсутствует.
    Missing,

    /// Resource descriptor существует, но текущий materializer его не поддерживает.
    Unsupported,

    /// Backend сообщил fatal/poisoned lookup state.
    Error,
}

/// App-level действие после texture-view lookup-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoFrameTexturePreparationAction {
    /// Рендерим текущий frame и обновляем cached renderable frame.
    RenderCurrentFrame,

    /// Рендерим cached previous frame из-за Busy, не делая blocking lookup.
    ReusePreviousFrameForTextureBusy {
        /// Нужно ли отдельно учесть repeated frame в shell telemetry.
        record_repeated_frame: bool,
    },

    /// Busy не дал безопасного cached frame-а; кадр не рендерится и ошибки нет.
    SkipVideoFrameForTextureBusy,

    /// Missing — это absent resource на render boundary, cache должен быть очищен.
    ReportMissingRenderResources,

    /// Unsupported — materializer не умеет текущий resource descriptor.
    ReportUnsupportedRenderResource,

    /// Error — fatal lookup на render boundary, cache должен быть очищен.
    ReportRenderResourceLookupFailure,
}

impl VideoFrameTexturePreparationAction {
    /// Возвращает `true`, если action должен инвалидировать cached renderable frame.
    const fn clears_cached_renderable_frame(self) -> bool {
        matches!(
            self,
            Self::ReportMissingRenderResources
                | Self::ReportUnsupportedRenderResource
                | Self::ReportRenderResourceLookupFailure
        )
    }
}

/// Кодирует fallback contract без GPU handles, чтобы tests проверяли именно boundary.
fn video_frame_texture_preparation_action(
    lookup_kind: TextureViewLookupKind,
    has_reusable_previous_frame: bool,
    acquisition_reused_previous_frame: bool,
) -> VideoFrameTexturePreparationAction {
    match lookup_kind {
        TextureViewLookupKind::Ready => VideoFrameTexturePreparationAction::RenderCurrentFrame,
        TextureViewLookupKind::Busy if has_reusable_previous_frame => {
            VideoFrameTexturePreparationAction::ReusePreviousFrameForTextureBusy {
                record_repeated_frame: !acquisition_reused_previous_frame,
            }
        }
        TextureViewLookupKind::Busy => {
            VideoFrameTexturePreparationAction::SkipVideoFrameForTextureBusy
        }
        TextureViewLookupKind::Missing => {
            VideoFrameTexturePreparationAction::ReportMissingRenderResources
        }
        TextureViewLookupKind::Unsupported => {
            VideoFrameTexturePreparationAction::ReportUnsupportedRenderResource
        }
        TextureViewLookupKind::Error => {
            VideoFrameTexturePreparationAction::ReportRenderResourceLookupFailure
        }
    }
}

/// Переносит результат playback worker tick в shell telemetry.
fn record_player_tick_result(telemetry: &Telemetry, tick_result: &PlayerTickResult) {
    telemetry.record_packets(
        media_core::TrackKind::Audio,
        tick_result.dropped_seek_audio_preroll_packets,
    );
    telemetry.record_packets(
        media_core::TrackKind::Video,
        tick_result.staged_video_backlog_recovery_packets,
    );

    for packet in &tick_result.demuxed_packets {
        telemetry.record_packet(packet.kind);

        if telemetry.packets_read() <= 50 {
            tracing::debug!(
                track_id = %packet.track_id,
                kind = ?packet.kind,
                pts_ms = packet.pts.as_millis(),
                raw_pts_units = ?packet.track_pts.map(|timestamp| timestamp.units.get()),
                raw_dts_units = ?packet.track_dts.map(|timestamp| timestamp.units.get()),
                size = packet.size,
                keyframe = ?packet.keyframe,
                "Packet"
            );
        }
    }

    for _ in 0..tick_result.decoded_video_frames {
        telemetry.record_video_frame_decoded();
    }

    for _ in 0..tick_result.video_frames_presented {
        telemetry.record_video_frame_presented();
    }

    for _ in 0..tick_result.video_frames_repeated {
        telemetry.record_video_frame_repeated();
    }

    for dropped_frame in &tick_result.dropped_video_frames {
        match map_video_frame_telemetry_event(dropped_frame.reason) {
            VideoFrameTelemetryEvent::PlaybackDrop(reason) => {
                telemetry.record_video_frame_dropped(reason);
            }
            VideoFrameTelemetryEvent::SeekDiscard(reason) => {
                telemetry.record_seek_discarded_frame(reason);
            }
        }
    }
}

fn record_worker_events(app_state: &mut AppState, events: Vec<PlayerWorkerEvent>) {
    for event in events {
        match event {
            PlayerWorkerEvent::Scrub(scrub_event) => {
                app_state.handle_main_visual_override_scrub_event(&scrub_event);
                app_state.handle_timeline_inline_status_scrub_event(&scrub_event);
            }
            PlayerWorkerEvent::RenderError(_) => {
                app_state.clear_cached_present_frame_after_worker_render_error();
            }
            PlayerWorkerEvent::Player(correlated_event) => {
                let media_instance_id = correlated_event.media_instance_id;
                let player_event = correlated_event.event;
                app_state.note_startup_player_event(media_instance_id, &player_event);
                app_state.handle_cached_present_frame_player_event(&player_event);
                app_state.handle_main_visual_override_player_event(&player_event);
                app_state.handle_notification_player_event(&player_event);
                match player_event {
                    PlayerEvent::MediaOpenRequested(_) => {
                        app_state.reset_dma_buf_runtime_fallback_for_new_media();
                    }
                    PlayerEvent::FatalError(fatal_error) => log_player_fatal_error(&fatal_error),
                    PlayerEvent::VideoBackendSelectionRequested(request) => {
                        app_state.note_video_backend_reselection_request(request);
                    }
                    PlayerEvent::SeekTargetFramePresented(presentation) => {
                        app_state
                            .note_live_scrub_landing_for_dispatch(presentation.target_position);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Пишет stable app-level marker для fatal-событий, чтобы local smoke мог
/// отличить typed rejection от panic, silent fallback или decoder disconnect.
fn log_player_fatal_error(fatal_error: &PlayerError) {
    error!(
        kind = ?fatal_error.kind,
        message = %fatal_error.message,
        "PlayerEvent::FatalError"
    );
}

/// Возвращает `true`, если surface drop означает разрыв lifecycle для cached texture.
pub(super) fn render_drop_reason_invalidates_cached_present_frame(
    reason: RenderFrameDropReason,
) -> bool {
    matches!(
        reason,
        RenderFrameDropReason::SurfaceOccluded
            | RenderFrameDropReason::SurfaceLost
            | RenderFrameDropReason::SurfaceValidation
            | RenderFrameDropReason::SurfaceOutdatedRecoveryFailed
    )
}

/// Применяет window chrome actions, которые не владеют lifecycle закрытия приложения.
fn apply_window_chrome_actions(window: &Window, actions: Vec<WindowChromeAction>) -> bool {
    let mut close_requested = false;

    for action in actions {
        match action {
            WindowChromeAction::Minimize => window.set_minimized(true),
            WindowChromeAction::ToggleMaximize => window.set_maximized(!window.is_maximized()),
            WindowChromeAction::Close => close_requested = true,
            WindowChromeAction::StartDrag => {
                if let Err(error) = window.drag_window() {
                    warn!(error = %error, "Could not start dragging the window");
                }
            }
            WindowChromeAction::BeginResize(direction) => {
                if let Err(error) = window.drag_resize_window(winit_resize_direction(direction)) {
                    warn!(
                        error = %error,
                        ?direction,
                        "Could not start resizing the window"
                    );
                }
            }
        }
    }

    close_requested
}

/// Маппит visual resize direction в winit API на shell boundary.
fn winit_resize_direction(direction: WindowChromeResizeDirection) -> ResizeDirection {
    match direction {
        WindowChromeResizeDirection::North => ResizeDirection::North,
        WindowChromeResizeDirection::NorthEast => ResizeDirection::NorthEast,
        WindowChromeResizeDirection::East => ResizeDirection::East,
        WindowChromeResizeDirection::SouthEast => ResizeDirection::SouthEast,
        WindowChromeResizeDirection::South => ResizeDirection::South,
        WindowChromeResizeDirection::SouthWest => ResizeDirection::SouthWest,
        WindowChromeResizeDirection::West => ResizeDirection::West,
        WindowChromeResizeDirection::NorthWest => ResizeDirection::NorthWest,
    }
}

/// Готовит video lease и texture views без входа в swapchain acquisition.
fn prepare_video_frame(
    telemetry: &Telemetry,
    app_state: &mut AppState,
    video_frames: &mut dyn VideoFrameImport,
    player_snapshot: &PlayerSnapshot,
) -> PreparedVideoFrame {
    let video_prepare_started_at = Instant::now();
    let mut timings = VideoPrepareTimings::default();

    // Живая смена backend-а: пока worker не переключился и не выдал первый кадр нового
    // backend-а, не материализуем кадры старого backend-а новым materializer-ом — держим
    // замороженный кадр (его texture views уже готовы), либо пусто, если кэша не было.
    if let BackendSwapVideoPhase::HoldFrozenFrame(frozen_frame) =
        app_state.backend_swap_video_phase(player_snapshot)
    {
        let state = "backend_swap_hold_frozen";
        timings.total = video_prepare_started_at.elapsed();
        return match frozen_frame {
            Some(renderable_frame) => PreparedVideoFrame::ready(renderable_frame.clone(), state)
                .with_diagnostics(state, timings),
            None => PreparedVideoFrame::empty(state).with_diagnostics(state, timings),
        };
    }

    if let Some(prepared_override_frame) =
        prepare_main_visual_override_frame(app_state, video_frames, player_snapshot)
    {
        return prepared_override_frame;
    }

    let stage_started_at = Instant::now();
    let present_frame_acquisition = app_state.acquire_present_frame_for_render(player_snapshot);
    timings.present_frame_acquire = stage_started_at.elapsed();
    let acquisition_state = present_frame_acquisition.metric_name();
    let acquisition_reused_previous_frame = present_frame_acquisition.reused_previous_frame();

    let stage_started_at = Instant::now();
    if acquisition_reused_previous_frame {
        telemetry.record_video_frame_repeated();
    }
    timings.repeated_frame_accounting = stage_started_at.elapsed();

    let Some(present_frame) = present_frame_acquisition.into_present_frame() else {
        timings.total = video_prepare_started_at.elapsed();
        return PreparedVideoFrame::empty(acquisition_state)
            .with_diagnostics("not_requested", timings);
    };

    let stage_started_at = Instant::now();
    let video_import_bound = video_frames.active_video_import_path().is_some();
    timings.materializer_access = stage_started_at.elapsed();

    if !video_import_bound {
        let stage_started_at = Instant::now();
        report_video_render_boundary_error(
            app_state,
            PlayerRenderError::missing_render_resources(&present_frame),
        );
        timings.lookup_action = stage_started_at.elapsed();
        timings.total = video_prepare_started_at.elapsed();
        return PreparedVideoFrame::empty(acquisition_state)
            .with_diagnostics("missing_materializer", timings);
    }

    let materialization = materialize_shared_video_frame(
        SharedVideoFrameMaterializationRequest::new(
            SharedVideoFrameLeaseRole::Playback,
            present_frame,
        ),
        video_frames,
    );
    timings.texture_view_lookup = materialization.timings.texture_view_lookup;
    timings.resource_lookup_report = materialization.timings.resource_lookup_report;

    // Эта развилка остаётся до renderer/surface critical path: Busy не ждёт backend lock,
    // а Missing/Error не превращаются в silent fallback.
    match materialization.outcome {
        SharedVideoFrameMaterializationOutcome::Ready { materialized_frame } => {
            let stage_started_at = Instant::now();
            debug_assert_eq!(
                video_frame_texture_preparation_action(
                    TextureViewLookupKind::Ready,
                    false,
                    acquisition_reused_previous_frame,
                ),
                VideoFrameTexturePreparationAction::RenderCurrentFrame
            );
            let current_renderable_frame = materialized_frame.into_renderable_present_frame();
            app_state.remember_renderable_present_frame(
                current_renderable_frame.clone(),
                player_snapshot,
            );
            timings.lookup_action = stage_started_at.elapsed();
            timings.total = video_prepare_started_at.elapsed();
            PreparedVideoFrame::ready(current_renderable_frame, acquisition_state)
                .with_diagnostics("ready", timings)
        }
        SharedVideoFrameMaterializationOutcome::Busy {
            present_frame: _present_frame,
        } => {
            let stage_started_at = Instant::now();
            let reusable_renderable_frame =
                app_state.reusable_renderable_frame_for_texture_busy(player_snapshot);
            let preparation_action = video_frame_texture_preparation_action(
                TextureViewLookupKind::Busy,
                reusable_renderable_frame.is_some(),
                acquisition_reused_previous_frame,
            );

            let prepared_video_frame = match (preparation_action, reusable_renderable_frame) {
                (
                    VideoFrameTexturePreparationAction::ReusePreviousFrameForTextureBusy {
                        record_repeated_frame,
                    },
                    Some(reusable_renderable_frame),
                ) => {
                    if record_repeated_frame {
                        telemetry.record_video_frame_repeated();
                    }
                    app_state.report_render_resource_previous_frame_reuse();
                    PreparedVideoFrame::ready(
                        reusable_renderable_frame,
                        "texture_view_busy_previous_frame_reuse",
                    )
                }
                (VideoFrameTexturePreparationAction::SkipVideoFrameForTextureBusy, None) => {
                    PreparedVideoFrame::empty("texture_view_busy_no_reusable_frame")
                }
                (
                    VideoFrameTexturePreparationAction::ReusePreviousFrameForTextureBusy {
                        record_repeated_frame,
                    },
                    None,
                ) => unreachable!(
                    "Busy reuse action requires a cached renderable frame; record_repeated_frame={record_repeated_frame}"
                ),
                (VideoFrameTexturePreparationAction::SkipVideoFrameForTextureBusy, Some(_)) => {
                    unreachable!("Busy skip action must not keep a reusable cached frame")
                }
                _ => unreachable!("Busy lookup must only produce Busy preparation actions"),
            };
            timings.lookup_action = stage_started_at.elapsed();
            timings.total = video_prepare_started_at.elapsed();
            prepared_video_frame.with_diagnostics("busy", timings)
        }
        SharedVideoFrameMaterializationOutcome::Missing { present_frame } => {
            let stage_started_at = Instant::now();
            let preparation_action = video_frame_texture_preparation_action(
                TextureViewLookupKind::Missing,
                false,
                acquisition_reused_previous_frame,
            );
            debug_assert!(preparation_action.clears_cached_renderable_frame());
            report_video_render_boundary_error(
                app_state,
                PlayerRenderError::missing_render_resources(&present_frame),
            );
            timings.lookup_action = stage_started_at.elapsed();
            timings.total = video_prepare_started_at.elapsed();
            PreparedVideoFrame::empty(acquisition_state).with_diagnostics("missing", timings)
        }
        SharedVideoFrameMaterializationOutcome::Unsupported {
            present_frame,
            reason,
        } => {
            let stage_started_at = Instant::now();
            let preparation_action = video_frame_texture_preparation_action(
                TextureViewLookupKind::Unsupported,
                false,
                acquisition_reused_previous_frame,
            );
            debug_assert!(preparation_action.clears_cached_renderable_frame());
            match reason {
                SharedMaterializationUnsupportedReason::Renderer(
                    FrameImportUnsupportedReason::DmaBufDescriptorRejected(layout_rejection),
                ) => {
                    let render_generation = present_frame.render_generation();
                    let player_error = PlayerRenderError::unsupported_frame_format(
                        &present_frame,
                        layout_rejection.to_string(),
                    );
                    app_state.note_dma_buf_layout_rejection(
                        layout_rejection,
                        render_generation,
                        player_error,
                    );
                }
                _ => report_video_render_boundary_error(
                    app_state,
                    PlayerRenderError::render_resource_lookup_failed(&present_frame),
                ),
            }
            timings.lookup_action = stage_started_at.elapsed();
            timings.total = video_prepare_started_at.elapsed();
            PreparedVideoFrame::empty(acquisition_state).with_diagnostics("unsupported", timings)
        }
        SharedVideoFrameMaterializationOutcome::Error { present_frame } => {
            let stage_started_at = Instant::now();
            let preparation_action = video_frame_texture_preparation_action(
                TextureViewLookupKind::Error,
                false,
                acquisition_reused_previous_frame,
            );
            debug_assert!(preparation_action.clears_cached_renderable_frame());
            report_video_render_boundary_error(
                app_state,
                PlayerRenderError::render_resource_lookup_failed(&present_frame),
            );
            timings.lookup_action = stage_started_at.elapsed();
            timings.total = video_prepare_started_at.elapsed();
            PreparedVideoFrame::empty(acquisition_state).with_diagnostics("error", timings)
        }
    }
}

fn prepare_main_visual_override_frame(
    app_state: &mut AppState,
    video_frames: &mut dyn VideoFrameImport,
    player_snapshot: &PlayerSnapshot,
) -> Option<PreparedVideoFrame> {
    let video_prepare_started_at = Instant::now();
    let mut timings = VideoPrepareTimings::default();
    let override_acquisition = app_state.acquire_main_visual_override_for_render(player_snapshot);
    let acquisition_state = override_acquisition.metric_name();

    match override_acquisition {
        MainVisualOverrideAcquisition::NoOverride
        | MainVisualOverrideAcquisition::WaitingForExactFrame => None,
        MainVisualOverrideAcquisition::Ready(renderable_frame) => {
            timings.total = video_prepare_started_at.elapsed();
            Some(
                PreparedVideoFrame::ready(renderable_frame, acquisition_state)
                    .with_diagnostics("scrub_override_ready", timings),
            )
        }
        MainVisualOverrideAcquisition::Lease { metadata, lease } => {
            let stage_started_at = Instant::now();
            let video_import_bound = video_frames.active_video_import_path().is_some();
            timings.materializer_access = stage_started_at.elapsed();

            if !video_import_bound {
                return None;
            }

            let materialization = materialize_shared_video_frame(
                SharedVideoFrameMaterializationRequest::new(
                    SharedVideoFrameLeaseRole::ScrubOverride,
                    lease,
                ),
                video_frames,
            );
            timings.texture_view_lookup = materialization.timings.texture_view_lookup;
            timings.resource_lookup_report = materialization.timings.resource_lookup_report;

            match materialization.outcome {
                SharedVideoFrameMaterializationOutcome::Ready { materialized_frame } => {
                    let stage_started_at = Instant::now();
                    let renderable_frame = materialized_frame.into_renderable_present_frame();
                    app_state.remember_main_visual_override_renderable(
                        metadata,
                        renderable_frame.clone(),
                    );
                    timings.lookup_action = stage_started_at.elapsed();
                    timings.total = video_prepare_started_at.elapsed();
                    Some(
                        PreparedVideoFrame::ready(renderable_frame, acquisition_state)
                            .with_diagnostics("scrub_override_ready", timings),
                    )
                }
                SharedVideoFrameMaterializationOutcome::Busy { .. } => None,
                SharedVideoFrameMaterializationOutcome::Missing { present_frame } => {
                    app_state.report_render_error(PlayerRenderError::missing_render_resources(
                        &present_frame,
                    ));
                    app_state.clear_main_visual_override();
                    None
                }
                SharedVideoFrameMaterializationOutcome::Unsupported { present_frame, .. }
                | SharedVideoFrameMaterializationOutcome::Error { present_frame } => {
                    app_state.report_render_error(
                        PlayerRenderError::render_resource_lookup_failed(&present_frame),
                    );
                    app_state.clear_main_visual_override();
                    None
                }
            }
        }
    }
}

/// Передаёт render boundary error в player и сбрасывает cached frame lease.
pub(super) fn report_video_render_boundary_error(
    app_state: &mut AppState,
    error: PlayerRenderError,
) {
    app_state.clear_cached_present_frame_after_render_failure();
    app_state.report_render_error(error);
    tracing::debug!(
        acquisition = "render_error_reported",
        "Present frame acquisition ended with render boundary error"
    );
}

/// Возвращает `true`, только если renderer реально принял video input в presented frame.
pub(super) fn render_outcome_marks_video_submitted(
    render_frame_outcome: &RenderFrameOutcome,
    submitted_video_frame: bool,
) -> bool {
    submitted_video_frame && matches!(render_frame_outcome, RenderFrameOutcome::Presented(_))
}

/// Даёт lifecycle flush-у (sidebar, громкость) settings adapter, пока renderer-bound
/// AppState ещё доступен: перед suspend и terminal exit.
pub(crate) fn with_lifecycle_settings_adapter<R>(
    window: &Arc<Window>,
    renderer: &mut Box<dyn RenderShell>,
    app_state: &mut AppState,
    playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
    renderer_lifecycle: &mut RendererLifecycleCoordinator,
    flush: impl FnOnce(&mut FrameSettingsRuntimeAdapter<'_>) -> R,
) -> R {
    let mut runtime_adapter = FrameSettingsRuntimeAdapter::new(
        window.clone(),
        app_state,
        renderer,
        playlist_runtime,
        renderer_lifecycle,
    );
    flush(&mut runtime_adapter)
}

/// Обрабатывает одно пробуждение UI-потока: решает, нужен ли egui-проход и показ кадра,
/// и выполняет ровно столько, сколько решено (cpu-parity, сессия 04).
///
/// Порядок стадий: ввод и snapshot → публикация desktop → таймеры интерфейса → решение о
/// плане → (egui-проход) → действия панелей и runtime-опросы → (видео → submit → present) →
/// трасса и pacing. Housekeeping (persist по таймерам, транспорт плейлиста, preview настроек)
/// выполняется на каждом пробуждении независимо от плана, чтобы поведение не зависело от того,
/// рисовался ли кадр.
// Span кадра — уровня TRACE и без полей: на уровне INFO он создавался на каждом
// пробуждении окна и форматировал `Debug` координатора рендерера в строку
// (~2 % главного потока по профилю cpu-parity сессии 05). Стадии кадра пишет
// отдельная трасса `fastiplayer::render_frame_timing`.
#[instrument(level = "trace", skip_all)]
pub(crate) fn render_frame(
    telemetry: &Telemetry,
    window: &Arc<Window>,
    renderer: &mut Box<dyn RenderShell>,
    app_state: &mut AppState,
    playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
    settings_runtime: &mut SettingsRuntime,
    renderer_lifecycle: &mut RendererLifecycleCoordinator,
) -> AppRenderFrameResult {
    let frame_start = Instant::now();
    let mut frame_sequence = FrameSequenceContract::default();

    let prepared_frame_input = prepare_frame_input(
        telemetry,
        window,
        renderer.as_mut(),
        app_state,
        &mut frame_sequence,
    );
    let input_snapshot_timings = prepared_frame_input.timings;
    let egui_input = prepared_frame_input.egui_input;
    let frame_context = prepared_frame_input.frame_context;
    playlist_runtime.publish_desktop_snapshot(frame_context.player_snapshot());

    let stage_started_at = Instant::now();
    // Playlist projection и catalog snapshot фиксируются до построения UI model.
    web_media_runtime::sync_before_ui(app_state, playlist_runtime);
    let playlist_import_preview = playlist_runtime.pending_playlist_import_preview();
    let playlist_confirmation = playlist_runtime.pending_playlist_confirmation();
    let playlist_interaction = playlist_runtime.playlist_interaction_model();
    // Один monotonic timestamp согласует countdown snapshot и его wake deadline.
    let playlist_ui_now = Instant::now();
    let transport_model = playlist_runtime
        .playlist_transport_ui_model(frame_context.player_snapshot().current_position);
    let undo_model = playlist_runtime.playlist_undo_ui_snapshot(playlist_ui_now);

    // Таймеры интерфейса идут на каждом пробуждении, затем — решение, что делать с кадром.
    advance_ui_timers(
        window,
        app_state,
        settings_runtime,
        &egui_input,
        playlist_ui_now,
    );
    let decision = decide_frame_work(
        app_state,
        settings_runtime,
        FrameDecisionInputs {
            window,
            renderer: renderer.as_ref(),
            frame_context: &frame_context,
            worker_event_count: input_snapshot_timings.worker_event_count,
            playlist: crate::state::PlaylistVisualKey {
                view_revision: app_state
                    .playlist_view_model()
                    .map(|model| model.revision()),
                interaction: playlist_interaction.clone(),
                transport: transport_model,
                undo: undo_model,
                import_preview: playlist_import_preview.clone(),
                confirmation: playlist_confirmation.clone(),
            },
            now: playlist_ui_now,
        },
    );
    let (mut prepared_ui_frame, plan) = prepare_ui_frame_for_plan(
        decision.plan,
        window,
        app_state,
        settings_runtime,
        egui_input,
        &frame_context,
        crate::state::PlaylistUiFrameModels {
            import_preview: playlist_import_preview.as_ref(),
            confirmation: playlist_confirmation.as_ref(),
            interaction: &playlist_interaction,
            transport: &transport_model,
            undo: &undo_model,
        },
    );
    telemetry.record_frame_work_plan(plan);
    if decision.fallback_probe && plan.presents() {
        telemetry.record_redraw_fallback_found_work();
    }
    // Видимая позиция кадра (за экраном — без дедлайна) нужна дедлайну пикселя/секунды ниже.
    let visible_position = decision.ui_frame_model.position;
    if plan.runs_ui_pass() {
        frame_sequence.reached(FrameSequenceStage::EguiOutput);
        app_state.remember_full_ui_pass(
            decision.ui_frame_model,
            prepared_ui_frame.retained_geometry(),
            playlist_ui_now,
        );
    }
    let settings_actions = std::mem::take(&mut prepared_ui_frame.settings_actions);
    let sidebar_width_change = prepared_ui_frame.sidebar_width_change.take();
    let transport_actions = std::mem::take(&mut prepared_ui_frame.transport_actions);
    let window_chrome_actions = std::mem::take(&mut prepared_ui_frame.window_chrome_actions);
    let playlist_confirmation_action = prepared_ui_frame.playlist_confirmation_action.take();
    let playlist_actions = std::mem::take(&mut prepared_ui_frame.playlist_actions);
    let url_sidebar_action = prepared_ui_frame.url_sidebar_action.take();
    let playlist_visible_items_hint = prepared_ui_frame.playlist_visible_items_hint.take();
    let mut ui_prepare_timings = prepared_ui_frame.timings;
    ui_prepare_timings.total = stage_started_at.elapsed();

    // Решение пользователя по субтитрам этого кадра (C, CC, меню); отдаётся один раз.
    let subtitle_enabled_change = app_state.take_subtitle_enabled_preference_change();
    // Память выбора дорожек: пауза покоя прошла - снимок уходит в фоновую запись.
    app_state.tick_track_memory();
    // Стиль, сохранённый кнопкой «Готово» (или неявным выходом), пишется сразу, одной транзакцией.
    let subtitle_style_commit = app_state.take_subtitle_style_commit();
    let mut subtitle_style_outcome = None;
    let settings_action_requested_repaint = {
        let surface_event_pending = window_chrome_actions.iter().any(|action| {
            matches!(
                action,
                WindowChromeAction::Minimize
                    | WindowChromeAction::ToggleMaximize
                    | WindowChromeAction::BeginResize(_)
            )
        });
        renderer_lifecycle.set_surface_event_pending(surface_event_pending);
        let mut runtime_adapter = FrameSettingsRuntimeAdapter::new(
            window.clone(),
            app_state,
            renderer,
            playlist_runtime,
            renderer_lifecycle,
        );
        let resize_requested_repaint = persist_due_runtime_originated_settings(
            settings_runtime,
            sidebar_width_change,
            PlayerAudioObservation::from_player_snapshot(frame_context.player_snapshot()),
            subtitle_enabled_change,
            &mut runtime_adapter,
        );
        if let Some(requests) = subtitle_style_commit {
            subtitle_style_outcome =
                Some(settings_runtime.commit_subtitle_style(requests, &mut runtime_adapter));
        }
        let actions_requested_repaint = match settings_runtime
            .handle_ui_actions_with_runtime_adapter(settings_actions, &mut runtime_adapter)
        {
            Ok(requested_repaint) => requested_repaint,
            Err(error) => {
                settings_runtime.report_runtime_error("Could not handle Settings UI action", error);
                true
            }
        };
        resize_requested_repaint || actions_requested_repaint
    };
    if subtitle_style_outcome == Some(crate::settings_runtime::SubtitleStyleCommitOutcome::Failed) {
        app_state.report_subtitle_style_save_failed();
    }
    let chrome_close_requested = apply_window_chrome_actions(window, window_chrome_actions);
    renderer_lifecycle.set_surface_event_pending(false);

    if let Some(action) = playlist_confirmation_action {
        app_state.apply_playlist_confirmation_action(action, playlist_runtime, renderer.as_mut());
    }
    let playlist_action_requested_repaint = crate::playlist_action_runtime::apply_playlist_actions(
        window,
        app_state,
        playlist_runtime,
        renderer.as_mut(),
        playlist_actions,
    );
    // «Повторить» на ошибке открытия: исполняется тем же путём, что исходное действие.
    app_state.apply_requested_media_retry(playlist_runtime, renderer.as_mut());
    // Web-версия звука из меню «♪»/`B` идёт тем же same-item switch-ем, что строка sidebar.
    let audio_version_requested_repaint =
        app_state.apply_requested_web_audio_version(playlist_runtime, renderer.as_mut());
    let url_action_requested_repaint = match url_sidebar_action {
        Some(action) => {
            if let Err(error) =
                app_state.apply_url_sidebar_action(action, playlist_runtime, renderer.as_mut())
            {
                tracing::warn!(error = %error, "URL same-item switch intent rejected");
            }
            true
        }
        None => false,
    };
    if let Some(hint) = playlist_visible_items_hint
        && playlist_runtime.validate_binding(hint.binding()).is_ok()
    {
        let yt_dlp_config = app_state.yt_dlp_metadata_config();
        let _refresh_outcome =
            playlist_runtime.request_visible_metadata_refresh(hint.item_ids(), &yt_dlp_config);
    }
    crate::transport_runtime::apply_transport_actions(
        app_state,
        playlist_runtime,
        renderer.as_mut(),
        frame_context.player_snapshot(),
        transport_actions,
    );
    crate::transport_runtime::apply_initial_queue_playback_action(
        app_state,
        playlist_runtime,
        renderer.as_mut(),
    );
    crate::transport_runtime::apply_playlist_automatic_snapshot(
        app_state,
        playlist_runtime,
        renderer.as_mut(),
        frame_context.player_snapshot(),
    );
    crate::transport_runtime::apply_discovery_navigation_action(
        app_state,
        playlist_runtime,
        renderer.as_mut(),
    );
    web_media_runtime::advance_after_actions(app_state, playlist_runtime, renderer.as_mut());
    crate::transport_runtime::poll_playlist_transport(
        app_state,
        playlist_runtime,
        renderer.as_mut(),
    );
    // Итоги пропусков битых файлов очереди — после всех transport-шагов этого кадра.
    app_state.show_playlist_queue_notices(playlist_runtime.drain_automatic_queue_notices());
    // Как прочитан импортированный плейлист (например, запасная кодировка Windows-1251).
    app_state.show_playlist_import_notices(playlist_runtime.drain_playlist_import_notices());

    let settings_preview_tick = match settings_runtime
        .apply_due_preview(renderer.live_settings_adapter(), Instant::now())
    {
        Ok(tick) => tick,
        Err(error) => {
            settings_runtime.report_runtime_error("Could not apply live preview", error);
            crate::settings_runtime::SettingsPreviewTick::default()
        }
    };
    // Следующий preview-тик — дедлайн планировщика, а не запрос egui: запрос после прохода
    // egui сбросил бы в начале следующего, и тик терялся (дефект до сессии 04).
    let settings_preview_deadline = settings_preview_tick
        .repaint_after
        .map(|repaint_after| Instant::now() + repaint_after);

    let frame_work_trace = FrameWorkTrace {
        plan: plan.metric_name(),
        ui_pass_reason: decision.ui_reasons.primary_name(),
        present_reason: decision.present_reasons.primary_name(),
        counters: telemetry.frame_work_counters(),
    };
    if plan.presents() {
        present_frame_for_plan(
            telemetry,
            renderer.as_mut(),
            app_state,
            &frame_context,
            prepared_ui_frame,
            &mut frame_sequence,
            PresentedFrameTimings {
                frame_start,
                input_snapshot: input_snapshot_timings,
                ui_prepare: ui_prepare_timings,
                frame_work: frame_work_trace,
            },
        );
    } else {
        log_skipped_frame(frame_work_trace, frame_start.elapsed());
    }

    // Запрос egui после прохода и действий: «сейчас» → немедленный redraw, иначе дедлайн.
    // Без прохода запрос не сбрасывается: его дедлайн по-прежнему ждёт планировщик.
    let egui_request = if plan.runs_ui_pass() {
        app_state.take_egui_repaint_request()
    } else {
        app_state.peek_egui_repaint_request()
    };
    AppRenderFrameResult {
        pacing: RedrawPacing::new(
            app_state.take_pending_worker_redraw()
                || egui_request.immediate
                || settings_action_requested_repaint
                || playlist_action_requested_repaint
                || url_action_requested_repaint
                || audio_version_requested_repaint,
            app_state.wants_continuous_redraw(),
        ),
        close_requested: chrome_close_requested,
        next_ui_wake_deadline: earliest_ui_wake_deadline([
            undo_model.next_wake_deadline,
            settings_runtime.next_sidebar_resize_deadline(),
            settings_runtime.next_user_audio_level_persist_deadline(),
            settings_runtime.next_subtitle_enabled_persist_deadline(),
            app_state.track_memory_wake_deadline(),
            app_state.next_notification_wake_deadline(),
            app_state.fullscreen_chrome_wake_deadline(),
            app_state.timeline_inline_status_wake_deadline(),
            app_state.next_telemetry_refresh_deadline(),
            egui_request.deadline,
            settings_preview_deadline,
            app_state.next_position_wake_deadline(
                frame_context.player_snapshot(),
                visible_position,
                Instant::now(),
            ),
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use player_core::{PlayerRenderErrorKind, PlayerVideoDropReason, PlayerVideoFrameDrop};
    use render_shell_api::{
        RenderFrameDropReason, RenderFrameFailure, RenderFrameStageTimings, RenderFrameTiming,
    };

    #[test]
    fn sidebar_debounce_deadline_participates_in_nearest_ui_wake_selection() {
        let now = Instant::now();
        let transport_deadline = now + Duration::from_secs(2);
        let sidebar_deadline = now + Duration::from_millis(500);

        assert_eq!(
            earliest_ui_wake_deadline([Some(transport_deadline), Some(sidebar_deadline), None]),
            Some(sidebar_deadline)
        );
        assert_eq!(
            earliest_ui_wake_deadline([None, Some(sidebar_deadline), Some(transport_deadline)]),
            Some(sidebar_deadline)
        );
    }

    /// Проверяет, что renderer error становится fatal media error, а не silent fallback.
    #[test]
    fn render_failure_maps_to_fatal_render_device_error() {
        let failure = RenderFrameFailure::new("P010 HDR renderer rejected strict metadata");

        let error = PlayerRenderError::render_device_lost(format!(
            "Video render failed: {}",
            failure.message
        ))
        .to_player_error();

        assert_eq!(error.kind, player_core::PlayerErrorKind::RenderDeviceLost);
        assert!(
            error
                .message
                .contains("P010 HDR renderer rejected strict metadata")
        );
    }

    /// Проверяет, что ошибка renderer boundary не превращается в silent empty frame.
    #[test]
    fn render_boundary_failure_maps_to_fatal_render_format_error() {
        let error = PlayerRenderError {
            kind: player_core::PlayerRenderErrorKind::UnsupportedFrameFormat,
            render_generation: Some(4),
            frame_handle: Some(9),
            message: "WGPU renderable frame rejected decoded P010 frame".into(),
        }
        .to_player_error();

        assert_eq!(
            error.kind,
            player_core::PlayerErrorKind::UnsupportedRenderFormat
        );
        assert!(
            error
                .message
                .contains("WGPU renderable frame rejected decoded P010 frame")
        );
    }

    /// Проверяет, что submitted lease отмечается только после успешного renderer present.
    #[test]
    fn render_outcome_marks_video_submitted_only_after_presented_submit() {
        let presented_frame = RenderFrameOutcome::Presented(RenderFrameTiming::new(
            RenderFrameStageTimings::default(),
            Duration::ZERO,
        ));
        let dropped_frame = RenderFrameOutcome::Dropped(RenderFrameDropReason::SurfaceTimeout);
        let failed_frame = RenderFrameOutcome::Failed(RenderFrameFailure::new(
            "renderer rejected frame after validation",
        ));

        assert!(render_outcome_marks_video_submitted(&presented_frame, true));
        assert!(!render_outcome_marks_video_submitted(
            &presented_frame,
            false
        ));
        assert!(!render_outcome_marks_video_submitted(&dropped_frame, true));
        assert!(!render_outcome_marks_video_submitted(&failed_frame, true));
    }

    /// Проверяет, что Busy + valid cached frame рендерит previous frame без cache clear.
    #[test]
    fn texture_view_busy_with_valid_cached_frame_reuses_previous_frame() {
        let preparation_action =
            video_frame_texture_preparation_action(TextureViewLookupKind::Busy, true, false);

        assert_eq!(
            preparation_action,
            VideoFrameTexturePreparationAction::ReusePreviousFrameForTextureBusy {
                record_repeated_frame: true
            }
        );
        assert!(!preparation_action.clears_cached_renderable_frame());
    }

    /// Проверяет, что Busy reuse не удваивает repeated telemetry для already-reused lease-а.
    #[test]
    fn texture_view_busy_reuse_does_not_double_count_already_repeated_frame() {
        let preparation_action =
            video_frame_texture_preparation_action(TextureViewLookupKind::Busy, true, true);

        assert_eq!(
            preparation_action,
            VideoFrameTexturePreparationAction::ReusePreviousFrameForTextureBusy {
                record_repeated_frame: false
            }
        );
        assert!(!preparation_action.clears_cached_renderable_frame());
    }

    /// Проверяет, что Busy без safe previous frame-а пропускает video input без fatal error.
    #[test]
    fn texture_view_busy_without_cached_frame_skips_video_input_without_error() {
        let preparation_action =
            video_frame_texture_preparation_action(TextureViewLookupKind::Busy, false, false);

        assert_eq!(
            preparation_action,
            VideoFrameTexturePreparationAction::SkipVideoFrameForTextureBusy
        );
        assert!(!preparation_action.clears_cached_renderable_frame());
    }

    /// Проверяет, что Missing — absent resource boundary error, а не silent fallback.
    #[test]
    fn texture_view_missing_reports_boundary_error_and_clears_cache() {
        let preparation_action =
            video_frame_texture_preparation_action(TextureViewLookupKind::Missing, true, false);

        assert_eq!(
            preparation_action,
            VideoFrameTexturePreparationAction::ReportMissingRenderResources
        );
        assert!(preparation_action.clears_cached_renderable_frame());
        let render_error = PlayerRenderError {
            kind: PlayerRenderErrorKind::MissingRenderResources,
            render_generation: Some(7),
            frame_handle: Some(42),
            message: "missing texture views".to_string(),
        };
        assert_eq!(
            render_error.to_player_error().kind,
            player_core::PlayerErrorKind::UnsupportedRenderFormat
        );
    }

    /// Проверяет, что Unsupported — отдельный descriptor boundary outcome.
    #[test]
    fn texture_view_unsupported_reports_boundary_error_and_clears_cache() {
        let preparation_action =
            video_frame_texture_preparation_action(TextureViewLookupKind::Unsupported, true, false);

        assert_eq!(
            preparation_action,
            VideoFrameTexturePreparationAction::ReportUnsupportedRenderResource
        );
        assert!(preparation_action.clears_cached_renderable_frame());
    }

    /// Проверяет, что Error — fatal lookup boundary error, а не Busy fallback.
    #[test]
    fn texture_view_error_reports_boundary_error_and_clears_cache() {
        let preparation_action =
            video_frame_texture_preparation_action(TextureViewLookupKind::Error, true, false);

        assert_eq!(
            preparation_action,
            VideoFrameTexturePreparationAction::ReportRenderResourceLookupFailure
        );
        assert!(preparation_action.clears_cached_renderable_frame());
        let render_error = PlayerRenderError {
            kind: PlayerRenderErrorKind::RenderResourceLookupFailed,
            render_generation: Some(7),
            frame_handle: Some(42),
            message: "texture view lookup failed".to_string(),
        };
        assert_eq!(
            render_error.to_player_error().kind,
            player_core::PlayerErrorKind::UnsupportedRenderFormat
        );
    }

    /// Проверяет absent-resource ветку нового video preparation boundary.
    #[test]
    fn prepared_video_frame_without_lease_has_no_render_input() {
        let prepared_video_frame = PreparedVideoFrame::empty("no_frame_yet");

        let render_input_frame = prepared_video_frame.render_input_video_frame();

        assert!(render_input_frame.is_none());
    }

    /// Проверяет, что cache чистится на lifecycle break, но не на transient timeout.
    #[test]
    fn surface_drop_reason_invalidates_cached_present_frame_only_on_lifecycle_break() {
        assert!(render_drop_reason_invalidates_cached_present_frame(
            RenderFrameDropReason::SurfaceLost
        ));
        assert!(render_drop_reason_invalidates_cached_present_frame(
            RenderFrameDropReason::SurfaceOutdatedRecoveryFailed
        ));
        assert!(render_drop_reason_invalidates_cached_present_frame(
            RenderFrameDropReason::SurfaceValidation
        ));
        assert!(render_drop_reason_invalidates_cached_present_frame(
            RenderFrameDropReason::SurfaceOccluded
        ));
        assert!(!render_drop_reason_invalidates_cached_present_frame(
            RenderFrameDropReason::SurfaceTimeout
        ));
    }

    /// Проверяет, что seek-discard причины не попадают в пользовательский счётчик drops.
    #[test]
    fn seek_reasons_map_to_seek_discarded_frames() {
        let telemetry = Telemetry::new();
        let tick_result = PlayerTickResult {
            dropped_video_frames: vec![
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(120),
                    reason: PlayerVideoDropReason::SeekPreroll,
                },
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(140),
                    reason: PlayerVideoDropReason::StaleGeneration,
                },
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(160),
                    reason: PlayerVideoDropReason::Late,
                },
            ],
            ..PlayerTickResult::default()
        };

        record_player_tick_result(&telemetry, &tick_result);

        assert_eq!(telemetry.video_frames_dropped(), 1);
        assert_eq!(telemetry.video_late_drops(), 1);
        assert_eq!(telemetry.video_other_drops(), 0);
        assert_eq!(telemetry.seek_discarded_frames(), 2);
        assert_eq!(telemetry.seek_preroll_discarded(), 1);
        assert_eq!(telemetry.stale_generation_discarded(), 1);
    }

    /// Проверяет bounded scalar telemetry для dense video recovery scan-а.
    #[test]
    fn staged_video_recovery_packets_count_as_read_without_packet_vec_entries() {
        let telemetry = Telemetry::new();
        let tick_result = PlayerTickResult {
            staged_video_backlog_recovery_packets: 420,
            ..PlayerTickResult::default()
        };

        record_player_tick_result(&telemetry, &tick_result);

        assert_eq!(telemetry.packets_read(), 420);
        assert!(tick_result.demuxed_packets.is_empty());
    }

    /// Проверяет, что playback причины не смешиваются с seek или surface taxonomy.
    #[test]
    fn playback_reasons_map_to_dedicated_playback_categories() {
        let telemetry = Telemetry::new();
        let tick_result = PlayerTickResult {
            dropped_video_frames: vec![
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(120),
                    reason: PlayerVideoDropReason::Late,
                },
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(140),
                    reason: PlayerVideoDropReason::QueueOverflow,
                },
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(160),
                    reason: PlayerVideoDropReason::Paused,
                },
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(180),
                    reason: PlayerVideoDropReason::DecoderStarvation,
                },
            ],
            ..PlayerTickResult::default()
        };

        record_player_tick_result(&telemetry, &tick_result);

        assert_eq!(telemetry.video_frames_dropped(), 4);
        assert_eq!(telemetry.playback_visible_drops(), 3);
        assert_eq!(telemetry.video_late_drops(), 1);
        assert_eq!(telemetry.video_queue_drops(), 1);
        assert_eq!(telemetry.video_pause_drops(), 1);
        assert_eq!(telemetry.video_decoder_starvation(), 1);
        assert_eq!(telemetry.video_other_drops(), 0);
        assert_eq!(telemetry.seek_discarded_frames(), 0);
        assert_eq!(telemetry.surface_dropped_frames(), 0);
    }

    /// Проверяет, что чистые seek-discard причины не становятся playback/render drop-ами.
    #[test]
    fn pure_seek_discard_reasons_do_not_count_as_playback_or_render_drops() {
        let telemetry = Telemetry::new();
        let tick_result = PlayerTickResult {
            dropped_video_frames: vec![
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(120),
                    reason: PlayerVideoDropReason::SeekPreroll,
                },
                PlayerVideoFrameDrop {
                    pts: Duration::from_millis(140),
                    reason: PlayerVideoDropReason::StaleGeneration,
                },
            ],
            ..PlayerTickResult::default()
        };

        record_player_tick_result(&telemetry, &tick_result);

        assert_eq!(telemetry.video_frames_dropped(), 0);
        assert_eq!(telemetry.video_late_drops(), 0);
        assert_eq!(telemetry.video_queue_drops(), 0);
        assert_eq!(telemetry.video_pause_drops(), 0);
        assert_eq!(telemetry.video_other_drops(), 0);
        assert_eq!(telemetry.surface_dropped_frames(), 0);
        assert_eq!(telemetry.seek_discarded_frames(), 2);
        assert_eq!(telemetry.seek_preroll_discarded(), 1);
        assert_eq!(telemetry.stale_generation_discarded(), 1);
    }
}
