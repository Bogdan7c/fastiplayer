//! Renderer submit, mark-submitted и surface telemetry одного кадра.

use std::time::Instant;

use player_core::{PlayerRenderError, PlayerSnapshot};
use render_wgpu_shell::{RenderFrameOutcome, RenderFrameTiming, Renderer};
use winit::window::Window;

use super::sequence::{FrameSequenceObserver, FrameSequenceStage};
use super::timing::VideoPrepareTimings;
use super::ui_prepare::PreparedUiFrame;
use super::{
    PreparedVideoFrame, prepare_video_frame, render_drop_reason_invalidates_cached_present_frame,
    render_outcome_marks_video_submitted, report_video_render_boundary_error,
};
use crate::state::AppState;
use crate::telemetry::Telemetry;

/// Отдельный tracing target позволяет acceptance включить покадровое доказательство точечно.
const VIDEO_RENDER_ACCEPTANCE_TARGET: &str = "fastiplayer::video_render_acceptance";

/// Диагностика реально выполненной video preparation и surface submission.
pub(super) struct SubmittedFrame {
    pub(super) renderer_timing: Option<RenderFrameTiming>,
    pub(super) video_timings: VideoPrepareTimings,
    pub(super) acquisition_state: &'static str,
    pub(super) texture_lookup_state: &'static str,
}

/// Получает surface, затем готовит свежий app-owned lease и учитывает outcome.
pub(super) fn submit_render_frame(
    telemetry: &Telemetry,
    window: &Window,
    renderer: &mut Renderer,
    app_state: &mut AppState,
    prepared_ui_frame: PreparedUiFrame,
    player_snapshot: &PlayerSnapshot,
    frame_sequence: &mut impl FrameSequenceObserver,
) -> SubmittedFrame {
    frame_sequence.reached(FrameSequenceStage::SurfaceAcquire);
    let acquired_frame = renderer.acquire_frame(render_wgpu_shell::RenderFrameInput {
        window,
        egui_paint_jobs: prepared_ui_frame.paint_jobs,
        egui_textures_delta: prepared_ui_frame.textures_delta,
        screen: prepared_ui_frame.screen,
        video_viewport: prepared_ui_frame.video_viewport,
        video_exclusion_rects: prepared_ui_frame.video_exclusion_rects,
        window_corner_mask: prepared_ui_frame.window_corner_mask,
    });
    // Failed acquisition не запрашивает lease и не меняет video accounting.
    // Snapshot сохраняет app lifecycle fences; допустимый PTS выбирает живой worker handoff.
    let stage_started_at = Instant::now();
    let prepared_video_frame = match &acquired_frame {
        Ok(surface_frame) => {
            let prepared = prepare_video_frame(telemetry, app_state, player_snapshot);
            let renderer = surface_frame.renderer();
            if let Err(fallback_failure) = app_state.apply_pending_dma_buf_runtime_fallback(
                renderer.instance(),
                renderer.adapter(),
                renderer.device(),
                renderer.queue(),
            ) {
                tracing::warn!(error = %fallback_failure.error, "Runtime DMA-BUF layout recovery rejected");
                report_video_render_boundary_error(app_state, fallback_failure.player_error);
            }
            prepared
        }
        Err(_) => PreparedVideoFrame::empty("surface_not_acquired"),
    };
    let mut video_timings = prepared_video_frame.timings;
    video_timings.total = stage_started_at.elapsed();
    let acquisition_state = prepared_video_frame.acquisition_state;
    let texture_lookup_state = prepared_video_frame.texture_view_lookup_state;
    frame_sequence.reached(FrameSequenceStage::MaterializerLookup);
    let video_frame = match prepared_video_frame.render_input_video_frame() {
        Ok(video_frame) => video_frame,
        Err(error) => {
            report_video_render_boundary_error(app_state, error);
            None
        }
    };
    let submitted_video_frame = video_frame.is_some();
    let startup_frame_identity = prepared_video_frame.current_frame_identity();

    // Этот lease выбран после acquisition; trace связывает его с фактическим
    // handoff и отдельно сохраняет Busy fallback, не подменяя frame identity.
    tracing::trace!(
        target: "fastiplayer::frame_cadence",
        frame_pts_ns = ?startup_frame_identity.map(|identity| identity.pts().as_nanos()),
        render_generation = ?startup_frame_identity.map(|identity| identity.render_generation()),
        decoded_generation = ?startup_frame_identity.map(|identity| identity.decoded_generation()),
        acquisition = prepared_video_frame.acquisition_state,
        texture_lookup = prepared_video_frame.texture_view_lookup_state,
        has_video_input = submitted_video_frame,
        "video frame prepared for surface"
    );

    frame_sequence.reached(FrameSequenceStage::RendererSubmit);
    let render_frame_outcome = match acquired_frame {
        Ok(surface_frame) => surface_frame.render(video_frame.as_ref()),
        Err(reason) => RenderFrameOutcome::Dropped(reason),
    };
    let video_was_submitted =
        render_outcome_marks_video_submitted(&render_frame_outcome, submitted_video_frame);
    if video_was_submitted {
        prepared_video_frame.mark_submitted_to_renderer();
        tracing::trace!(
            target: VIDEO_RENDER_ACCEPTANCE_TARGET,
            "video frame submitted to renderer"
        );
        if let Some(identity) = startup_frame_identity {
            // Acceptance различает новые PTS и повторный submit того же кадра.
            // Событие находится за Presented gate; UI redraw сам по себе его
            // не создаёт. Это surface handoff, а не доказательство scanout.
            tracing::trace!(
                target: VIDEO_RENDER_ACCEPTANCE_TARGET,
                frame_pts_ns = ?identity.pts().as_nanos(),
                render_generation = identity.render_generation(),
                decoded_generation = identity.decoded_generation(),
                "current video frame submitted to surface"
            );
        }
    }

    let renderer_timing = match render_frame_outcome {
        RenderFrameOutcome::Presented(timing) => {
            telemetry.record_frame_presented_to_surface();
            app_state.report_gpu_submit_present_latency(timing.submit_present_elapsed);
            if video_was_submitted && let Some(frame_identity) = startup_frame_identity {
                app_state.note_startup_surface_frame_presented(frame_identity);
            }
            Some(timing)
        }
        RenderFrameOutcome::Dropped(reason) => {
            telemetry.record_surface_dropped_frame();
            if render_drop_reason_invalidates_cached_present_frame(reason) {
                app_state.clear_cached_present_frame_after_surface_lifecycle_break();
            }
            None
        }
        RenderFrameOutcome::Failed(failure) => {
            telemetry.record_surface_dropped_frame();
            app_state.clear_cached_present_frame_after_render_failure();
            app_state.report_render_error(PlayerRenderError::render_device_lost(format!(
                "Video render failed: {}",
                failure.message
            )));
            None
        }
    };
    SubmittedFrame {
        renderer_timing,
        video_timings,
        acquisition_state,
        texture_lookup_state,
    }
}
