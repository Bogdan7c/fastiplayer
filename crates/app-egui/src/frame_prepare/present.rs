//! Видео-стадия, submit, present и трасса показанного кадра (планы `FullPass` и
//! `PresentReusingUi` сессии 04). Пропущенный кадр сюда не попадает.

use std::sync::Arc;
use std::time::Instant;

use render_wgpu_shell::Renderer;
use tracing::warn;
use winit::window::Window;

use super::input_snapshot::AppFrameInputTimings;
use super::sequence::{FrameSequenceContract, FrameSequenceObserver, FrameSequenceStage};
use super::submit::submit_render_frame;
use super::timing::{
    AppRenderFrameTimings, FrameWorkTrace, SurfaceFrameCounters, log_render_frame_timings,
};
use super::ui_prepare::{PreparedUiFrame, UiPrepareTimings};
use super::{prepare_video_frame, report_video_render_boundary_error};
use crate::state::{AppFrameContext, AppState};
use crate::telemetry::Telemetry;

/// Тайминги стадий до показа, которые нужны строке трассы показанного кадра.
pub(super) struct PresentedFrameTimings {
    pub(super) frame_start: Instant,
    pub(super) input_snapshot: AppFrameInputTimings,
    pub(super) ui_prepare: UiPrepareTimings,
    pub(super) frame_work: FrameWorkTrace,
}

/// Видео-стадия, submit, present и трасса — для планов `FullPass` и `PresentReusingUi`.
#[expect(
    clippy::too_many_arguments,
    reason = "координатор кадра передаёт владельцев стадий явно, без скрытого контекста"
)]
pub(super) fn present_frame_for_plan(
    telemetry: &Telemetry,
    window: &Arc<Window>,
    renderer: &mut Renderer,
    app_state: &mut AppState,
    frame_context: &AppFrameContext,
    prepared_ui_frame: PreparedUiFrame,
    frame_sequence: &mut FrameSequenceContract,
    timings: PresentedFrameTimings,
) {
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

    let frame_duration = timings.frame_start.elapsed();
    let frame_time_ms = frame_duration.as_secs_f64() * 1000.0;
    telemetry.update_fps(frame_time_ms);
    let surface_frame_counters = SurfaceFrameCounters {
        presented: telemetry.frames_presented_to_surface(),
        dropped: telemetry.surface_dropped_frames(),
    };
    log_render_frame_timings(
        frame_context,
        AppRenderFrameTimings {
            total: frame_duration,
            input_snapshot: timings.input_snapshot,
            ui_prepare: timings.ui_prepare,
            video_prepare: video_prepare_timings,
            renderer_submit: renderer_submit_elapsed,
            dma_buf_import_stats: app_state
                .wgpu_frame_materializer()
                .and_then(|materializer| materializer.dma_buf_import_stats()),
            video_hot_path: renderer.video_hot_path_stats(),
        },
        timings.frame_work,
        video_acquisition_state,
        texture_view_lookup_state,
        surface_frame_counters,
        renderer_timing,
    );
}
