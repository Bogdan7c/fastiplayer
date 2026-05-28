use std::time::Instant;

use anyhow::Result;
use player_core::{
    PlayerRenderError, PlayerSnapshot, PlayerTickResult, PlayerVideoDropReason, PlayerWorkerEvent,
};
use render_wgpu_shell::{RenderFrameDropReason, RenderFrameOutcome, Renderer};
use render_wgpu_video::{WgpuFrameTextureViewLookup, WgpuRenderableFrame};
use tracing::instrument;
use video_core::DecodedPixelFormat;
use winit::window::Window;

use crate::redraw_pacing::RedrawPacing;
use crate::state::{AppFrameContext, AppState, RenderablePresentFrame};
use crate::telemetry::{SeekDiscardReason, Telemetry, VideoDropReason, VideoFrameTelemetryEvent};

/// Результат UI stage до входа в renderer/surface critical path.
struct PreparedUiFrame {
    /// Уже tessellated egui primitives для `egui-wgpu`.
    paint_jobs: Vec<egui::epaint::ClippedPrimitive>,

    /// Изменения egui texture atlas-а, которые renderer должен применить до pass-а.
    textures_delta: egui::TexturesDelta,

    /// Размер surface target-а и UI scale без раскрытия `egui-wgpu` наружу.
    screen: render_wgpu_shell::RenderScreenDescriptor,

    /// Признак, что egui попросил следующий repaint.
    requested_repaint: bool,
}

/// Video stage, подготовленный до финального `render_wgpu_shell::RenderFrameInput`.
struct PreparedVideoFrame {
    /// Lease и texture views живут вместе, чтобы `WgpuRenderableFrame` не пережил owner-а.
    renderable_frame: Option<RenderablePresentFrame>,

    /// Диагностическое имя acquisition state для tracing.
    acquisition_state: &'static str,
}

impl PreparedVideoFrame {
    /// Создаёт video stage без renderable кадра.
    const fn empty(acquisition_state: &'static str) -> Self {
        Self {
            renderable_frame: None,
            acquisition_state,
        }
    }

    /// Создаёт video stage с уже удерживаемым render lease-ом.
    fn ready(renderable_frame: RenderablePresentFrame, acquisition_state: &'static str) -> Self {
        Self {
            renderable_frame: Some(renderable_frame),
            acquisition_state,
        }
    }

    /// Собирает video boundary frame на время одного `render-wgpu-shell` call-а.
    fn render_input_video_frame(
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
    fn mark_submitted_to_renderer(&self) {
        if let Some(renderable_frame) = &self.renderable_frame {
            renderable_frame.present_frame.mark_submitted_to_renderer();
        }
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

    /// Error — fatal lookup на render boundary, cache должен быть очищен.
    ReportRenderResourceLookupFailure,
}

impl VideoFrameTexturePreparationAction {
    /// Возвращает `true`, если action должен инвалидировать cached renderable frame.
    const fn clears_cached_renderable_frame(self) -> bool {
        matches!(
            self,
            Self::ReportMissingRenderResources | Self::ReportRenderResourceLookupFailure
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
        TextureViewLookupKind::Error => {
            VideoFrameTexturePreparationAction::ReportRenderResourceLookupFailure
        }
    }
}

/// Переносит результат playback worker tick в shell telemetry.
fn record_player_tick_result(telemetry: &Telemetry, tick_result: &PlayerTickResult) {
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

/// Переносит worker event stream в shell telemetry и app-level lifecycle boundaries.
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
            PlayerWorkerEvent::RenderError(_) => {
                app_state.clear_cached_present_frame_after_worker_render_error();
            }
            PlayerWorkerEvent::Player(player_event) => {
                app_state.handle_cached_present_frame_player_event(&player_event);
            }
        }
    }
}

/// Возвращает `true`, если surface drop означает разрыв lifecycle для cached texture.
fn render_drop_reason_invalidates_cached_present_frame(reason: RenderFrameDropReason) -> bool {
    matches!(
        reason,
        RenderFrameDropReason::SurfaceOccluded
            | RenderFrameDropReason::SurfaceLost
            | RenderFrameDropReason::SurfaceValidation
            | RenderFrameDropReason::SurfaceOutdatedRecoveryFailed
    )
}

/// Классифицирует core-причину удаления кадра для пользовательской telemetry.
fn map_video_frame_telemetry_event(reason: PlayerVideoDropReason) -> VideoFrameTelemetryEvent {
    match reason {
        PlayerVideoDropReason::Late => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::Late)
        }
        PlayerVideoDropReason::QueueOverflow => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::QueueOverflow)
        }
        PlayerVideoDropReason::Paused => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::Paused)
        }
        PlayerVideoDropReason::SeekPreroll => {
            VideoFrameTelemetryEvent::SeekDiscard(SeekDiscardReason::SeekPreroll)
        }
        PlayerVideoDropReason::StaleGeneration => {
            VideoFrameTelemetryEvent::SeekDiscard(SeekDiscardReason::StaleGeneration)
        }
        PlayerVideoDropReason::DecoderStarvation => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::DecoderStarvation)
        }
        PlayerVideoDropReason::RenderAcquisitionTimeout => {
            VideoFrameTelemetryEvent::PlaybackDrop(VideoDropReason::Other)
        }
    }
}

/// Готовит egui output до входа в renderer/surface critical path.
fn prepare_ui_frame(
    window: &Window,
    app_state: &mut AppState,
    egui_input: egui::RawInput,
    frame_context: &AppFrameContext,
) -> PreparedUiFrame {
    let egui_full_output = app_state.render_ui(window, egui_input, frame_context);
    let requested_repaint = app_state.egui_ctx.has_requested_repaint();

    app_state
        .egui_winit_state
        .handle_platform_output(window, egui_full_output.platform_output);

    let pixels_per_point = app_state.egui_ctx.pixels_per_point();
    let paint_jobs = app_state
        .egui_ctx
        .tessellate(egui_full_output.shapes, pixels_per_point);
    let size = window.inner_size();
    let screen_size_in_pixels = [size.width.max(1), size.height.max(1)];

    PreparedUiFrame {
        paint_jobs,
        textures_delta: egui_full_output.textures_delta,
        screen: render_wgpu_shell::RenderScreenDescriptor {
            size_in_pixels: screen_size_in_pixels,
            pixels_per_point,
        },
        requested_repaint,
    }
}

/// Готовит video lease и texture views без входа в swapchain acquisition.
fn prepare_video_frame(
    telemetry: &Telemetry,
    app_state: &mut AppState,
    player_snapshot: &PlayerSnapshot,
) -> PreparedVideoFrame {
    let present_frame_acquisition = app_state.acquire_present_frame_for_render(player_snapshot);
    let acquisition_state = present_frame_acquisition.metric_name();
    let acquisition_reused_previous_frame = present_frame_acquisition.reused_previous_frame();

    if acquisition_reused_previous_frame {
        telemetry.record_video_frame_repeated();
    }

    let Some(present_frame) = present_frame_acquisition.into_present_frame() else {
        return PreparedVideoFrame::empty(acquisition_state);
    };

    let Some(texture_view_materializer) = app_state.wgpu_frame_materializer() else {
        report_video_render_boundary_error(
            app_state,
            PlayerRenderError::missing_render_resources(&present_frame),
        );
        return PreparedVideoFrame::empty(acquisition_state);
    };

    let texture_view_lookup =
        texture_view_materializer.try_texture_view_lookup(present_frame.texture_handle());
    present_frame.report_resource_lookup_sample(
        texture_view_lookup.texture_pool_lock_wait(),
        texture_view_lookup.lookup_was_busy(),
    );

    // Эта развилка остаётся до renderer/surface critical path: Busy не ждёт backend lock,
    // а Missing/Error не превращаются в silent fallback.
    match texture_view_lookup {
        WgpuFrameTextureViewLookup::Ready { views, .. } => {
            debug_assert_eq!(
                video_frame_texture_preparation_action(
                    TextureViewLookupKind::Ready,
                    false,
                    acquisition_reused_previous_frame,
                ),
                VideoFrameTexturePreparationAction::RenderCurrentFrame
            );
            let current_renderable_frame = RenderablePresentFrame::new(present_frame, views);
            app_state.remember_renderable_present_frame(
                current_renderable_frame.clone(),
                player_snapshot,
            );
            PreparedVideoFrame::ready(current_renderable_frame, acquisition_state)
        }
        WgpuFrameTextureViewLookup::Busy { .. } => {
            let reusable_renderable_frame =
                app_state.reusable_renderable_frame_for_texture_busy(player_snapshot);
            let preparation_action = video_frame_texture_preparation_action(
                TextureViewLookupKind::Busy,
                reusable_renderable_frame.is_some(),
                acquisition_reused_previous_frame,
            );

            match (preparation_action, reusable_renderable_frame) {
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
            }
        }
        WgpuFrameTextureViewLookup::Missing { .. } => {
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
            PreparedVideoFrame::empty(acquisition_state)
        }
        WgpuFrameTextureViewLookup::Error { .. } => {
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
            PreparedVideoFrame::empty(acquisition_state)
        }
    }
}

/// Передаёт render boundary error в player и сбрасывает cached frame lease.
fn report_video_render_boundary_error(app_state: &mut AppState, error: PlayerRenderError) {
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

    tracing::trace!(
        handle_id = present_frame.frame.texture_handle.0,
        pts_ms = present_frame.frame.pts.as_millis(),
        format = %present_frame.frame.format,
        memory_path = %present_frame.frame.memory_path,
        stale = present_frame.stale,
        acquisition = acquisition_state,
        "Present frame acquired from playback worker"
    );

    let boundary_frame = match present_frame.frame.format {
        DecodedPixelFormat::Nv12 => WgpuRenderableFrame::from_decoded_nv12(
            &present_frame.frame,
            &texture_views.y_view,
            &texture_views.uv_view,
        ),
        DecodedPixelFormat::P010 => WgpuRenderableFrame::from_decoded_p010(
            &present_frame.frame,
            &texture_views.y_view,
            &texture_views.uv_view,
        ),
        DecodedPixelFormat::Rgba8 => Err(anyhow::anyhow!(
            "RGBA8 decoded video surface is not a production zero-copy render path"
        )),
    };

    boundary_frame.map_err(|error| {
        let message = format!(
            "WGPU renderable frame rejected decoded {} frame: {}",
            present_frame.frame.format, error
        );
        tracing::error!(
            error = %error,
            format = %present_frame.frame.format,
            memory_path = %present_frame.frame.memory_path,
            "Failed to build WGPU renderable frame"
        );
        PlayerRenderError::unsupported_frame_format(present_frame, message)
    })
}

/// Собирает финальный GPU input и передаёт кадр в renderer-owned present path.
fn submit_render_frame(
    telemetry: &Telemetry,
    window: &Window,
    renderer: &mut Renderer,
    app_state: &mut AppState,
    prepared_ui_frame: PreparedUiFrame,
    prepared_video_frame: PreparedVideoFrame,
) {
    let video_frame = match prepared_video_frame.render_input_video_frame() {
        Ok(video_frame) => video_frame,
        Err(error) => {
            report_video_render_boundary_error(app_state, error);
            None
        }
    };
    let submitted_video_frame = video_frame.is_some();

    match renderer.render_frame(render_wgpu_shell::RenderFrameInput {
        window,
        video_frame: video_frame.as_ref(),
        egui_paint_jobs: prepared_ui_frame.paint_jobs,
        egui_textures_delta: prepared_ui_frame.textures_delta,
        screen: prepared_ui_frame.screen,
    }) {
        RenderFrameOutcome::Presented(timing) => {
            if submitted_video_frame {
                prepared_video_frame.mark_submitted_to_renderer();
            }
            telemetry.record_frame_presented_to_surface();
            app_state.report_gpu_submit_present_latency(timing.submit_present_elapsed);
        }
        RenderFrameOutcome::Dropped(reason) => {
            telemetry.record_surface_dropped_frame();
            if render_drop_reason_invalidates_cached_present_frame(reason) {
                app_state.clear_cached_present_frame_after_surface_lifecycle_break();
            }
        }
        RenderFrameOutcome::Failed(failure) => {
            telemetry.record_surface_dropped_frame();
            app_state.clear_cached_present_frame_after_render_failure();
            app_state.report_render_error(PlayerRenderError::render_device_lost(format!(
                "Video render failed: {}",
                failure.message
            )));
        }
    }
}

/// Рендерит один полный кадр: видео + egui overlay.
///
/// Измеряет время кадра, обновляет телеметрию,
/// и вызывает renderer.render_frame().
#[instrument(skip(telemetry, window, renderer, app_state))]
pub(crate) fn render_frame(
    telemetry: &Telemetry,
    window: &Window,
    renderer: &mut Renderer,
    app_state: &mut AppState,
) -> RedrawPacing {
    let frame_start = Instant::now();

    let egui_input = app_state.egui_winit_state.take_egui_input(window);
    let worker_events = app_state.drain_worker_events();
    record_worker_events(telemetry, app_state, worker_events);
    let frame_context = app_state.begin_frame_context(renderer.diagnostics());
    app_state.publish_desktop_snapshot(frame_context.player_snapshot());
    let prepared_ui_frame = prepare_ui_frame(window, app_state, egui_input, &frame_context);
    let egui_requested_repaint = prepared_ui_frame.requested_repaint;
    let prepared_video_frame =
        prepare_video_frame(telemetry, app_state, frame_context.player_snapshot());

    submit_render_frame(
        telemetry,
        window,
        renderer,
        app_state,
        prepared_ui_frame,
        prepared_video_frame,
    );

    let frame_duration = frame_start.elapsed();
    let frame_time_ms = frame_duration.as_secs_f64() * 1000.0;
    telemetry.update_fps(frame_time_ms);

    RedrawPacing::new(
        app_state.wants_continuous_redraw(),
        app_state.take_pending_worker_redraw() || egui_requested_repaint,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use player_core::{PlayerRenderErrorKind, PlayerVideoFrameDrop};
    use render_wgpu_shell::{RenderFrameDropReason, RenderFrameFailure};

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
        assert_eq!(telemetry.video_frames_late_dropped(), 1);
        assert_eq!(telemetry.video_frames_other_dropped(), 0);
        assert_eq!(telemetry.seek_discarded_frames(), 2);
        assert_eq!(telemetry.seek_preroll_discarded(), 1);
        assert_eq!(telemetry.stale_generation_discarded(), 1);
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
        assert_eq!(telemetry.video_frames_late_dropped(), 0);
        assert_eq!(telemetry.video_frames_queue_dropped(), 0);
        assert_eq!(telemetry.video_frames_pause_dropped(), 0);
        assert_eq!(telemetry.video_frames_other_dropped(), 0);
        assert_eq!(telemetry.dropped_frames(), 0);
        assert_eq!(telemetry.seek_discarded_frames(), 2);
        assert_eq!(telemetry.seek_preroll_discarded(), 1);
        assert_eq!(telemetry.stale_generation_discarded(), 1);
    }
}
