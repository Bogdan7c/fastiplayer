use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use player_core::{
    PlayerError, PlayerEvent, PlayerRenderError, PlayerRuntimeApplyError, PlayerRuntimeApplyGroup,
    PlayerRuntimeApplyGroupReport, PlayerRuntimeApplyReport, PlayerRuntimeApplyResult,
    PlayerRuntimeSettingsUpdate, PlayerSnapshot, PlayerTickResult, PlayerWorkerEvent,
    PreparedMedia,
};
use render_core::{
    RenderLiveApplyReport, RenderLiveSettings, RenderLiveSettingsAdapter, RenderLiveSettingsError,
    RenderLiveSettingsUpdate,
};
use render_wgpu_shell::{RenderFrameDropReason, RenderFrameOutcome, Renderer};
use render_wgpu_video::WgpuRenderableFrame;
use rustiplayer_settings::{
    AppRouteApplyResult, MediaServiceRuntimeSettingsUpdate, PlayerCommittedSettingsUpdate,
    RenderCommittedSettingsUpdate, SettingsBoundaryActivity,
};
use settings_core::{SettingId, SettingsResult};
use tracing::{error, instrument, warn};
use video_core::DecodedPixelFormat;
use video_present_core::VideoPresentFrameIdentity;
use winit::window::{ResizeDirection, Window};

use crate::redraw_pacing::RedrawPacing;
use crate::renderer_recreation::{LiveRendererRecreation, RendererLifecycleCoordinator};
use crate::settings_runtime::{
    CommittedConfigSnapshot, SettingsRouteTargetPolicy, SettingsRuntime,
    SettingsRuntimeReconfigureHost,
};
use crate::startup_media::{resolve_direct_media_startup_media, runtime_video_codec};
use crate::state::{
    ActiveMediaSource, AppState, BackendSwapVideoPhase, MainVisualOverrideAcquisition,
    RenderablePresentFrame, VideoPipelineRebuildError, VideoPipelineRebuildRequest,
};
use crate::system_capabilities::probe_system_capabilities;
use crate::telemetry::{Telemetry, VideoFrameTelemetryEvent};
use crate::ui::window_chrome::{WindowChromeAction, WindowChromeResizeDirection};

#[path = "frame_prepare/geometry.rs"]
mod geometry;
#[path = "frame_prepare/input_snapshot.rs"]
mod input_snapshot;
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
#[path = "frame_prepare/timing.rs"]
mod timing;
#[path = "frame_prepare/ui_prepare.rs"]
mod ui_prepare;
#[path = "frame_prepare/web_media_runtime.rs"]
mod web_media_runtime;
use input_snapshot::prepare_frame_input;
use sequence::{FrameSequenceContract, FrameSequenceObserver, FrameSequenceStage};
use settings_runtime_adapter::FrameSettingsRuntimeAdapter;
use shared_frame_materialization::{
    SharedMaterializationUnsupportedReason, SharedVideoFrameLeaseRole,
    SharedVideoFrameMaterializationOutcome, SharedVideoFrameMaterializationRequest,
    materialize_shared_video_frame,
};
use submit::submit_render_frame;
use telemetry_mapping::map_video_frame_telemetry_event;
use timing::{
    AppRenderFrameTimings, SurfaceFrameCounters, VideoPrepareTimings, log_render_frame_timings,
};
use ui_prepare::prepare_ui_frame;

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
fn earliest_ui_wake_deadline(first: Option<Instant>, second: Option<Instant>) -> Option<Instant> {
    first.into_iter().chain(second).min()
}

/// Video stage, подготовленный до финального `render_wgpu_shell::RenderFrameInput`.
pub(super) struct PreparedVideoFrame {
    /// Lease и texture views живут вместе, чтобы `WgpuRenderableFrame` не пережил owner-а.
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

    /// Собирает video boundary frame на время одного `render-wgpu-shell` call-а.
    pub(super) fn render_input_video_frame(
        &self,
    ) -> Result<Option<WgpuRenderableFrame<'_>>, PlayerRenderError> {
        self.renderable_frame
            .as_ref()
            .map(|renderable_frame| {
                build_render_input_video_frame(renderable_frame, self.acquisition_state)
            })
            .transpose()
    }

    /// Отмечает, что подготовленный video frame реально попал в renderer submit.
    pub(super) fn mark_submitted_to_renderer(&self) {
        if let Some(renderable_frame) = &self.renderable_frame {
            renderable_frame.present_frame.mark_submitted_to_renderer();
        }
    }

    /// Возвращает identity только текущего, не помеченного stale video lease-а.
    fn current_frame_identity(&self) -> Option<VideoPresentFrameIdentity> {
        let present_frame = &self.renderable_frame.as_ref()?.present_frame;
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

fn record_worker_events(
    telemetry: &Telemetry,
    app_state: &mut AppState,
    events: Vec<PlayerWorkerEvent>,
) {
    for event in events {
        match event {
            PlayerWorkerEvent::Tick(tick_result) => {
                record_player_tick_result(telemetry, &tick_result);
            }
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
                match player_event {
                    PlayerEvent::MediaOpenRequested(_) => {
                        app_state.reset_dma_buf_runtime_fallback_for_new_media();
                    }
                    PlayerEvent::FatalError(fatal_error) => {
                        log_player_fatal_error(&fatal_error);
                    }
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
                    warn!(error = %error, "Не удалось начать перетаскивание окна");
                }
            }
            WindowChromeAction::BeginResize(direction) => {
                if let Err(error) = window.drag_resize_window(winit_resize_direction(direction)) {
                    warn!(
                        error = %error,
                        ?direction,
                        "Не удалось начать resize окна"
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
        prepare_main_visual_override_frame(app_state, player_snapshot)
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
    let texture_view_materializer = app_state.wgpu_frame_materializer();
    timings.materializer_access = stage_started_at.elapsed();

    let Some(texture_view_materializer) = texture_view_materializer else {
        let stage_started_at = Instant::now();
        report_video_render_boundary_error(
            app_state,
            PlayerRenderError::missing_render_resources(&present_frame),
        );
        timings.lookup_action = stage_started_at.elapsed();
        timings.total = video_prepare_started_at.elapsed();
        return PreparedVideoFrame::empty(acquisition_state)
            .with_diagnostics("missing_materializer", timings);
    };

    let materialization = materialize_shared_video_frame(
        SharedVideoFrameMaterializationRequest::new(
            SharedVideoFrameLeaseRole::Playback,
            present_frame,
        ),
        texture_view_materializer.as_ref(),
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
                SharedMaterializationUnsupportedReason::Wgpu(
                    render_wgpu_video::WgpuFrameMaterializationUnsupportedReason::DmaBufDescriptorRejected(
                        layout_rejection,
                    ),
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
            let texture_view_materializer = app_state.wgpu_frame_materializer();
            timings.materializer_access = stage_started_at.elapsed();

            let texture_view_materializer = texture_view_materializer?;

            let materialization = materialize_shared_video_frame(
                SharedVideoFrameMaterializationRequest::new(
                    SharedVideoFrameLeaseRole::ScrubOverride,
                    lease,
                ),
                texture_view_materializer.as_ref(),
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

/// Строит renderer-facing video frame из prepared lease-а и texture views.
fn build_render_input_video_frame<'frame>(
    renderable_frame: &'frame RenderablePresentFrame,
    acquisition_state: &'static str,
) -> Result<WgpuRenderableFrame<'frame>, PlayerRenderError> {
    let present_frame = &renderable_frame.present_frame;
    let texture_views = &renderable_frame.texture_views;
    let decoded_frame = present_frame.decoded_frame();
    let frame_format = decoded_frame.format();
    let frame_memory_path = decoded_frame.memory_path();

    tracing::trace!(
        handle_id = decoded_frame.resource_handle.0,
        pts_ms = decoded_frame.pts.as_millis(),
        format = %frame_format,
        memory_path = %frame_memory_path,
        stale = present_frame.is_stale(),
        acquisition = acquisition_state,
        "Present frame acquired from playback worker"
    );

    let boundary_frame = match frame_format {
        DecodedPixelFormat::Nv12 => match texture_views.dma_buf_views() {
            Some((y_view, uv_view)) => {
                WgpuRenderableFrame::from_decoded_nv12(decoded_frame, y_view, uv_view)
            }
            None => Err(anyhow::anyhow!(
                "NV12 decoded video surface requires DMA-BUF Y/UV texture views"
            )),
        },
        DecodedPixelFormat::P010 => match texture_views.dma_buf_views() {
            Some((y_view, uv_view)) => {
                WgpuRenderableFrame::from_decoded_p010(decoded_frame, y_view, uv_view)
            }
            None => Err(anyhow::anyhow!(
                "P010 decoded video surface requires DMA-BUF Y/UV texture views"
            )),
        },
        DecodedPixelFormat::Rgba8 => Err(anyhow::anyhow!(
            "RGBA8 decoded video surface is not a production zero-copy render path"
        )),
        host_planar_layout if host_planar_layout.is_host_planar() => {
            match texture_views.host_planar_views() {
                Some((y_view, u_view, v_view)) => WgpuRenderableFrame::from_decoded_host_yuv(
                    decoded_frame,
                    y_view,
                    u_view,
                    v_view,
                ),
                None => Err(anyhow::anyhow!(
                    "{} decoded video surface requires HostPlanar Y/U/V texture views",
                    host_planar_layout
                )),
            }
        }
        unsupported_layout => Err(anyhow::anyhow!(
            "{} decoded video surface is not a production zero-copy render path",
            unsupported_layout
        )),
    };

    boundary_frame.map_err(|error| {
        let message = format!(
            "WGPU renderable frame rejected decoded {} frame: {}",
            frame_format, error
        );
        tracing::error!(
            error = %error,
            format = %frame_format,
            memory_path = %frame_memory_path,
            "Failed to build WGPU renderable frame"
        );
        PlayerRenderError::unsupported_frame_format(present_frame, message)
    })
}

/// Возвращает `true`, только если renderer реально принял video input в presented frame.
pub(super) fn render_outcome_marks_video_submitted(
    render_frame_outcome: &RenderFrameOutcome,
    submitted_video_frame: bool,
) -> bool {
    submitted_video_frame && matches!(render_frame_outcome, RenderFrameOutcome::Presented(_))
}

/// Принудительно flush-ит sidebar resize, пока renderer-bound AppState ещё доступен.
pub(crate) fn flush_sidebar_resize_before_lifecycle_boundary(
    window: &Arc<Window>,
    renderer: &mut Renderer,
    app_state: &mut AppState,
    playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
    settings_runtime: &mut SettingsRuntime,
    renderer_lifecycle: &mut RendererLifecycleCoordinator,
) -> SettingsResult<crate::settings_runtime::SidebarResizeFlushOutcome> {
    let mut runtime_adapter = FrameSettingsRuntimeAdapter::new(
        window.clone(),
        app_state,
        renderer,
        playlist_runtime,
        renderer_lifecycle,
    );
    settings_runtime.flush_pending_sidebar_resize(&mut runtime_adapter)
}

/// Рендерит один полный кадр: видео + egui overlay.
///
/// Измеряет время кадра, обновляет телеметрию,
/// и вызывает renderer.render_frame().
#[instrument(skip(
    telemetry,
    window,
    renderer,
    app_state,
    playlist_runtime,
    settings_runtime
))]
pub(crate) fn render_frame(
    telemetry: &Telemetry,
    window: &Arc<Window>,
    renderer: &mut Renderer,
    app_state: &mut AppState,
    playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
    settings_runtime: &mut SettingsRuntime,
    renderer_lifecycle: &mut RendererLifecycleCoordinator,
) -> AppRenderFrameResult {
    let frame_start = Instant::now();
    let mut frame_sequence = FrameSequenceContract::default();

    let prepared_frame_input =
        prepare_frame_input(telemetry, window, renderer, app_state, &mut frame_sequence);
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
    let mut prepared_ui_frame = prepare_ui_frame(
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
    frame_sequence.reached(FrameSequenceStage::EguiOutput);
    let egui_requested_repaint = prepared_ui_frame.requested_repaint;
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
        if let Some(width_change) = sidebar_width_change {
            let _pending_changed =
                settings_runtime.record_sidebar_width_change(width_change, Instant::now());
        }
        let resize_requested_repaint = match settings_runtime
            .flush_due_sidebar_resize(Instant::now(), &mut runtime_adapter)
        {
            Ok(outcome) => outcome.needs_redraw(),
            Err(error) => {
                settings_runtime.report_runtime_error("Не удалось сохранить ширину sidebar", error);
                true
            }
        };
        let actions_requested_repaint = match settings_runtime
            .handle_ui_actions_with_runtime_adapter(settings_actions, &mut runtime_adapter)
        {
            Ok(requested_repaint) => requested_repaint,
            Err(error) => {
                settings_runtime
                    .report_runtime_error("Не удалось обработать действие settings UI", error);
                true
            }
        };
        resize_requested_repaint || actions_requested_repaint
    };
    let chrome_close_requested = apply_window_chrome_actions(window, window_chrome_actions);
    renderer_lifecycle.set_surface_event_pending(false);

    if let Some(action) = playlist_confirmation_action {
        app_state.apply_playlist_confirmation_action(action, playlist_runtime);
    }
    let playlist_action_requested_repaint = crate::playlist_action_runtime::apply_playlist_actions(
        window,
        app_state,
        playlist_runtime,
        renderer,
        playlist_actions,
    );
    let url_action_requested_repaint = match url_sidebar_action {
        Some(action) => {
            if let Err(error) =
                app_state.apply_url_sidebar_action(action, playlist_runtime, renderer)
            {
                tracing::warn!(error = %error, "URL same-item switch intent отклонён");
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
        renderer,
        frame_context.player_snapshot(),
        transport_actions,
    );
    crate::transport_runtime::apply_initial_queue_playback_action(
        app_state,
        playlist_runtime,
        renderer,
    );
    crate::transport_runtime::apply_playlist_automatic_snapshot(
        app_state,
        playlist_runtime,
        renderer,
        frame_context.player_snapshot(),
    );
    crate::transport_runtime::apply_discovery_navigation_action(
        app_state,
        playlist_runtime,
        renderer,
    );
    web_media_runtime::advance_after_actions(app_state, playlist_runtime, renderer);
    app_state.poll_playlist_transport(playlist_runtime, renderer);

    let settings_preview_tick = match settings_runtime.apply_due_preview(renderer, Instant::now()) {
        Ok(tick) => tick,
        Err(error) => {
            settings_runtime.report_runtime_error("Не удалось применить live preview", error);
            crate::settings_runtime::SettingsPreviewTick::default()
        }
    };
    if let Some(repaint_after) = settings_preview_tick.repaint_after {
        app_state.egui_ctx.request_repaint_after(repaint_after);
    }

    let stage_started_at = Instant::now();
    let prepared_video_frame =
        prepare_video_frame(telemetry, app_state, frame_context.player_snapshot());
    frame_sequence.reached(FrameSequenceStage::MaterializerLookup);
    if let Err(fallback_failure) = app_state.apply_pending_dma_buf_runtime_fallback(
        renderer.instance(),
        renderer.adapter(),
        renderer.device(),
        renderer.queue(),
    ) {
        warn!(error = %fallback_failure.error, "Runtime DMA-BUF layout recovery rejected");
        report_video_render_boundary_error(app_state, fallback_failure.player_error);
    }
    let mut video_prepare_timings = prepared_video_frame.timings;
    video_prepare_timings.total = stage_started_at.elapsed();
    let video_acquisition_state = prepared_video_frame.acquisition_state;
    let texture_view_lookup_state = prepared_video_frame.texture_view_lookup_state;

    let stage_started_at = Instant::now();
    frame_sequence.reached(FrameSequenceStage::RendererSubmit);
    let renderer_timing = submit_render_frame(
        telemetry,
        window,
        renderer,
        app_state,
        prepared_ui_frame,
        prepared_video_frame,
    );
    let renderer_submit_elapsed = stage_started_at.elapsed();

    let frame_duration = frame_start.elapsed();
    let frame_time_ms = frame_duration.as_secs_f64() * 1000.0;
    telemetry.update_fps(frame_time_ms);
    let surface_frame_counters = SurfaceFrameCounters {
        presented: telemetry.frames_presented_to_surface(),
        dropped: telemetry.surface_dropped_frames(),
    };
    log_render_frame_timings(
        frame_context.player_snapshot(),
        AppRenderFrameTimings {
            total: frame_duration,
            input_snapshot: input_snapshot_timings,
            ui_prepare: ui_prepare_timings,
            video_prepare: video_prepare_timings,
            renderer_submit: renderer_submit_elapsed,
        },
        video_acquisition_state,
        texture_view_lookup_state,
        surface_frame_counters,
        renderer_timing,
    );

    AppRenderFrameResult {
        pacing: RedrawPacing::new(
            app_state.wants_continuous_redraw(),
            app_state.take_pending_worker_redraw()
                || egui_requested_repaint
                || settings_action_requested_repaint
                || playlist_action_requested_repaint
                || url_action_requested_repaint,
        ),
        close_requested: chrome_close_requested,
        next_ui_wake_deadline: earliest_ui_wake_deadline(
            undo_model.next_wake_deadline,
            settings_runtime.next_sidebar_resize_deadline(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use player_core::{PlayerRenderErrorKind, PlayerVideoDropReason, PlayerVideoFrameDrop};
    use render_wgpu_shell::{
        RenderFrameDropReason, RenderFrameFailure, RenderFrameStageTimings, RenderFrameTiming,
    };

    #[test]
    fn sidebar_debounce_deadline_participates_in_nearest_ui_wake_selection() {
        let now = Instant::now();
        let transport_deadline = now + Duration::from_secs(2);
        let sidebar_deadline = now + Duration::from_millis(500);

        assert_eq!(
            earliest_ui_wake_deadline(Some(transport_deadline), Some(sidebar_deadline)),
            Some(sidebar_deadline)
        );
        assert_eq!(
            earliest_ui_wake_deadline(None, Some(sidebar_deadline)),
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

        let render_input_frame = prepared_video_frame
            .render_input_video_frame()
            .expect("empty prepared video frame must not fail");

        assert!(render_input_frame.is_none());
    }

    /// Проверяет, что renderer input заимствует lease/views у prepared stage.
    #[test]
    fn prepared_video_frame_render_input_is_borrowed_from_prepared_stage() {
        fn render_input_with_prepared_lifetime<'prepared>(
            prepared_video_frame: &'prepared PreparedVideoFrame,
        ) -> Result<Option<WgpuRenderableFrame<'prepared>>, PlayerRenderError> {
            prepared_video_frame.render_input_video_frame()
        }

        let prepared_video_frame = PreparedVideoFrame::empty("no_frame_yet");
        let render_input_frame = render_input_with_prepared_lifetime(&prepared_video_frame)
            .expect("empty prepared frame must not fail lifetime check");

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
