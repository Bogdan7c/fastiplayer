//! Renderer submit, mark-submitted и surface telemetry одного кадра.

use player_core::PlayerRenderError;
use render_wgpu_shell::{RenderFrameOutcome, RenderFrameTiming, Renderer};
use winit::window::Window;

use super::ui_prepare::PreparedUiFrame;
use super::{
    PreparedVideoFrame, render_drop_reason_invalidates_cached_present_frame,
    render_outcome_marks_video_submitted, report_video_render_boundary_error,
};
use crate::state::AppState;
use crate::telemetry::Telemetry;

/// Отдельный tracing target позволяет acceptance включить покадровое доказательство точечно.
const VIDEO_RENDER_ACCEPTANCE_TARGET: &str = "rustiplayer::video_render_acceptance";

/// Передаёт краткоживущие UI/video resources renderer-у и учитывает outcome.
pub(super) fn submit_render_frame(
    telemetry: &Telemetry,
    window: &Window,
    renderer: &mut Renderer,
    app_state: &mut AppState,
    prepared_ui_frame: PreparedUiFrame,
    prepared_video_frame: PreparedVideoFrame,
) -> Option<RenderFrameTiming> {
    let video_frame = match prepared_video_frame.render_input_video_frame() {
        Ok(video_frame) => video_frame,
        Err(error) => {
            report_video_render_boundary_error(app_state, error);
            None
        }
    };
    let submitted_video_frame = video_frame.is_some();
    let startup_frame_identity = prepared_video_frame.current_frame_identity();

    let render_frame_outcome = renderer.render_frame(render_wgpu_shell::RenderFrameInput {
        window,
        video_frame: video_frame.as_ref(),
        egui_paint_jobs: prepared_ui_frame.paint_jobs,
        egui_textures_delta: prepared_ui_frame.textures_delta,
        screen: prepared_ui_frame.screen,
        video_viewport: prepared_ui_frame.video_viewport,
        video_exclusion_rects: prepared_ui_frame.video_exclusion_rects,
        window_corner_mask: prepared_ui_frame.window_corner_mask,
    });
    let video_was_submitted =
        render_outcome_marks_video_submitted(&render_frame_outcome, submitted_video_frame);
    if video_was_submitted {
        prepared_video_frame.mark_submitted_to_renderer();
        tracing::trace!(
            target: VIDEO_RENDER_ACCEPTANCE_TARGET,
            "video frame submitted to renderer"
        );
    }

    match render_frame_outcome {
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
    }
}
