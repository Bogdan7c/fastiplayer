//! Per-frame input I/O и единый immutable snapshot для UI/desktop/render стадий.

use render_shell_api::RenderShell;
use std::time::{Duration, Instant};
use winit::window::Window;

use super::record_worker_events;
use super::sequence::{FrameSequenceObserver, FrameSequenceStage};
use super::tick_telemetry::record_tick_telemetry;
use crate::state::{AppFrameContext, AppState};
use crate::telemetry::Telemetry;

pub(super) struct PreparedFrameInput {
    pub(super) egui_input: egui::RawInput,
    pub(super) frame_context: AppFrameContext,
    pub(super) timings: AppFrameInputTimings,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct AppFrameInputTimings {
    pub(super) total: Duration,
    pub(super) egui_input: Duration,
    pub(super) worker_event_drain: Duration,
    pub(super) worker_event_record: Duration,
    pub(super) worker_event_count: usize,
    pub(super) frame_context: Duration,
    pub(super) desktop_publish: Duration,
}

pub(super) fn prepare_frame_input(
    telemetry: &Telemetry,
    window: &Window,
    renderer: &mut dyn RenderShell,
    app_state: &mut AppState,
    sequence: &mut impl FrameSequenceObserver,
) -> PreparedFrameInput {
    let input_snapshot_started_at = Instant::now();
    let stage_started_at = Instant::now();
    let egui_input = app_state.egui_winit_state.take_egui_input(window);
    let egui_input_elapsed = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    let worker_events = app_state.drain_worker_events();
    sequence.reached(FrameSequenceStage::WorkerEventDrain);
    let worker_event_count = worker_events.len();
    let worker_event_drain_elapsed = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    record_worker_events(app_state, worker_events);
    // Tick-телеметрия едет отдельно от lifecycle-событий (независимые состояния:
    // телеметрия пишет только в `Telemetry`, lifecycle — только в `AppState`),
    // поэтому порядок между ними не важен.
    record_tick_telemetry(telemetry, app_state.take_worker_tick_telemetry());
    sequence.reached(FrameSequenceStage::WorkerEventRecord);
    if let Some(request) = app_state.take_pending_video_backend_reselection() {
        app_state.apply_video_backend_reselection(
            &request,
            crate::state::RendererVideoImportTarget::for_renderer(renderer),
        );
    }
    let worker_event_record_elapsed = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    let frame_context = app_state.begin_frame_context(renderer.diagnostics());
    let frame_context_elapsed = stage_started_at.elapsed();

    let stage_started_at = Instant::now();
    sequence.reached(FrameSequenceStage::DesktopPublish);
    let desktop_publish_elapsed = stage_started_at.elapsed();

    PreparedFrameInput {
        egui_input,
        frame_context,
        timings: AppFrameInputTimings {
            total: input_snapshot_started_at.elapsed(),
            egui_input: egui_input_elapsed,
            worker_event_drain: worker_event_drain_elapsed,
            worker_event_record: worker_event_record_elapsed,
            worker_event_count,
            frame_context: frame_context_elapsed,
            desktop_publish: desktop_publish_elapsed,
        },
    }
}
