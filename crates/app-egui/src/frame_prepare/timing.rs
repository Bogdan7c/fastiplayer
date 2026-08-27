//! App-owned CPU timing и slow-frame diagnostics одного render frame-а.
//!
//! Модуль получает только immutable snapshots и уже измеренные длительности.
//! Он не владеет renderer submit, video lease, texture cache или telemetry lifecycle.

use std::time::Duration;

use player_core::{LatencyCounterSnapshot, PlayerSnapshot};
use render_wgpu_shell::RenderFrameTiming;
use tracing::{debug, trace};

use super::input_snapshot::AppFrameInputTimings;
use super::ui_prepare::UiPrepareTimings;

/// Tracing target для чистого включения render frame timings без packet debug шума.
const RENDER_FRAME_TIMING_TARGET: &str = "rustiplayer::render_frame_timing";

/// Fallback budget для startup/opening кадров, пока player snapshot ещё не измерил frame duration.
const DEFAULT_RENDER_FRAME_BUDGET: Duration = Duration::from_nanos(16_666_667);

/// Небольшой допуск, чтобы debug log показывал именно подозрительные кадры, а не шум таймера.
const SLOW_RENDER_FRAME_SLACK: Duration = Duration::from_millis(2);

/// Timings app-owned video подготовки до renderer boundary.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct VideoPrepareTimings {
    /// Полная длительность video prepare stage.
    pub(super) total: Duration,

    /// Получение `VideoFrameLease` или reuse cached present frame.
    pub(super) present_frame_acquire: Duration,

    /// Учёт repeated frame после acquisition.
    pub(super) repeated_frame_accounting: Duration,

    /// Получение active WGPU materializer из app state.
    pub(super) materializer_access: Duration,

    /// Lookup texture views по opaque frame handle.
    pub(super) texture_view_lookup: Duration,

    /// Передача lock-wait/busy sample обратно в player diagnostics.
    pub(super) resource_lookup_report: Duration,

    /// App-level действие после lookup: cache update, reuse, skip или error report.
    pub(super) lookup_action: Duration,
}

/// Surface-level frame counters после renderer outcome handling текущего кадра.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct SurfaceFrameCounters {
    /// Кадры, успешно отправленные в surface present path.
    pub(super) presented: u64,

    /// Кадры, отброшенные на surface/render boundary.
    pub(super) dropped: u64,
}

/// App-owned timing стадий до и вокруг вызова `render-wgpu-shell`.
#[derive(Debug, Clone, Copy)]
pub(super) struct AppRenderFrameTimings {
    /// Полная длительность кадра на UI/render thread.
    pub(super) total: Duration,

    /// Забор egui input, worker events, snapshot/context и desktop publication.
    pub(super) input_snapshot: AppFrameInputTimings,

    /// Выполнение egui UI, platform output и tessellation.
    pub(super) ui_prepare: UiPrepareTimings,

    /// Acquire present lease и materialization/lookup texture views.
    pub(super) video_prepare: VideoPrepareTimings,

    /// Вызов renderer-owned submit/present path вместе с app-level outcome handling.
    pub(super) renderer_submit: Duration,
}

/// Превращает длительность в миллисекунды для числовых tracing fields.
fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Возвращает worst latency одной player diagnostics stage в миллисекундах.
fn latency_worst_ms(counter: LatencyCounterSnapshot) -> f64 {
    counter
        .worst
        .map(|sample| duration_ms(sample.duration))
        .unwrap_or_default()
}

/// Возвращает frame budget из player snapshot-а без привязки scheduler-а к FPS магии.
fn render_frame_budget(player_snapshot: &PlayerSnapshot) -> Duration {
    let frame_budget = player_snapshot.video_frame_duration_estimate;
    if frame_budget.is_zero() {
        DEFAULT_RENDER_FRAME_BUDGET
    } else {
        frame_budget
    }
}

/// Логирует подробную разбивку кадра: trace для всех кадров, debug только для slow frame.
pub(super) fn log_render_frame_timings(
    player_snapshot: &PlayerSnapshot,
    app_timings: AppRenderFrameTimings,
    video_acquisition_state: &'static str,
    texture_view_lookup_state: &'static str,
    surface_frame_counters: SurfaceFrameCounters,
    renderer_timing: Option<RenderFrameTiming>,
) {
    let frame_budget = render_frame_budget(player_snapshot);
    let slow_frame_threshold = frame_budget + SLOW_RENDER_FRAME_SLACK;
    let slowest_renderer_stage = renderer_timing.map(|timing| timing.stages.slowest_stage());
    let input_timings = app_timings.input_snapshot;
    let ui_timings = app_timings.ui_prepare;
    let app_ui_timings = ui_timings.app_ui;
    let video_timings = app_timings.video_prepare;
    let diagnostics = &player_snapshot.diagnostics;
    let queues = diagnostics.queues;
    let texture_slots = queues.texture_slots;
    let decoder_control_channel = queues.decoder_control_channel;
    let publish_pressure = diagnostics.decoder_frame_publish_pressure;
    let latencies = diagnostics.worst_latencies;
    let render_resource_lock_wait = latencies.render_resource_lock_wait;
    let gpu_submit_present = latencies.gpu_submit_present;
    let release_acknowledgement = latencies.release_acknowledgement;
    let worker_scheduler = latencies.worker_scheduler;
    let demux_read = latencies.demux_read;
    let decoder_submit = latencies.decoder_submit;
    let decoder_event_drain = latencies.decoder_event_drain;
    let dma_buf_import = latencies.dma_buf_import;
    let frame_counters = player_snapshot.frame_counters;

    trace!(
        target: RENDER_FRAME_TIMING_TARGET,
        frame_total_ms = duration_ms(app_timings.total),
        frame_budget_ms = duration_ms(frame_budget),
        input_snapshot_ms = duration_ms(input_timings.total),
        ui_prepare_ms = duration_ms(ui_timings.total),
        video_prepare_ms = duration_ms(video_timings.total),
        renderer_submit_ms = duration_ms(app_timings.renderer_submit),
        renderer_elapsed_ms = renderer_timing
            .map(|timing| duration_ms(timing.renderer_elapsed))
            .unwrap_or_default(),
        surface_acquire_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.surface_acquire))
            .unwrap_or_default(),
        queue_submit_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.queue_submit))
            .unwrap_or_default(),
        device_poll_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.device_poll))
            .unwrap_or_default(),
        surface_present_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.surface_present))
            .unwrap_or_default(),
        "render frame stage timings"
    );

    if app_timings.total <= slow_frame_threshold {
        return;
    }

    debug!(
        target: RENDER_FRAME_TIMING_TARGET,
        frame_total_ms = duration_ms(app_timings.total),
        slow_threshold_ms = duration_ms(slow_frame_threshold),
        input_snapshot_ms = duration_ms(input_timings.total),
        input_egui_ms = duration_ms(input_timings.egui_input),
        input_worker_event_drain_ms = duration_ms(input_timings.worker_event_drain),
        input_worker_event_record_ms = duration_ms(input_timings.worker_event_record),
        input_worker_event_count = input_timings.worker_event_count,
        input_frame_context_ms = duration_ms(input_timings.frame_context),
        input_desktop_publish_ms = duration_ms(input_timings.desktop_publish),
        ui_prepare_ms = duration_ms(ui_timings.total),
        ui_app_total_ms = duration_ms(app_ui_timings.total),
        ui_pre_setup_ms = duration_ms(app_ui_timings.pre_ui_setup),
        ui_egui_run_ms = duration_ms(app_ui_timings.egui_run),
        ui_top_bar_ms = duration_ms(app_ui_timings.top_bar),
        ui_bottom_controls_ms = duration_ms(app_ui_timings.bottom_controls),
        ui_telemetry_panel_ms = duration_ms(app_ui_timings.telemetry_panel),
        ui_center_overlay_ms = duration_ms(app_ui_timings.center_overlay),
        ui_post_actions_ms = duration_ms(app_ui_timings.post_ui_actions),
        ui_telemetry_panel_visible = app_ui_timings.telemetry_panel_visible,
        ui_repaint_query_ms = duration_ms(ui_timings.repaint_query),
        ui_platform_output_ms = duration_ms(ui_timings.platform_output),
        ui_tessellate_ms = duration_ms(ui_timings.tessellate),
        ui_screen_descriptor_ms = duration_ms(ui_timings.screen_descriptor),
        video_prepare_ms = duration_ms(video_timings.total),
        video_present_acquire_ms = duration_ms(video_timings.present_frame_acquire),
        video_repeated_accounting_ms = duration_ms(video_timings.repeated_frame_accounting),
        video_materializer_access_ms = duration_ms(video_timings.materializer_access),
        video_texture_lookup_ms = duration_ms(video_timings.texture_view_lookup),
        video_resource_report_ms = duration_ms(video_timings.resource_lookup_report),
        video_lookup_action_ms = duration_ms(video_timings.lookup_action),
        video_acquisition_state = video_acquisition_state,
        texture_view_lookup_state = texture_view_lookup_state,
        renderer_submit_ms = duration_ms(app_timings.renderer_submit),
        renderer_elapsed_ms = renderer_timing
            .map(|timing| duration_ms(timing.renderer_elapsed))
            .unwrap_or_default(),
        egui_texture_update_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.egui_texture_update))
            .unwrap_or_default(),
        encoder_creation_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.encoder_creation))
            .unwrap_or_default(),
        egui_buffer_update_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.egui_buffer_update))
            .unwrap_or_default(),
        surface_acquire_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.surface_acquire))
            .unwrap_or_default(),
        surface_view_creation_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.surface_view_creation))
            .unwrap_or_default(),
        video_render_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.video_render))
            .unwrap_or_default(),
        egui_render_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.egui_render))
            .unwrap_or_default(),
        queue_submit_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.queue_submit))
            .unwrap_or_default(),
        device_poll_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.device_poll))
            .unwrap_or_default(),
        pre_present_notify_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.pre_present_notify))
            .unwrap_or_default(),
        surface_present_ms = renderer_timing
            .map(|timing| duration_ms(timing.stages.surface_present))
            .unwrap_or_default(),
        slowest_renderer_stage = slowest_renderer_stage
            .map(|stage| stage.name)
            .unwrap_or("none"),
        slowest_renderer_stage_ms = slowest_renderer_stage
            .map(|stage| duration_ms(stage.elapsed))
            .unwrap_or_default(),
        surface_presented_frames = surface_frame_counters.presented,
        surface_dropped_frames = surface_frame_counters.dropped,
        playback_presented_frames = frame_counters.presented,
        playback_dropped_frames = frame_counters.dropped,
        playback_repeated_frames = frame_counters.repeated,
        queue_pending_audio_packets = queues.pending_audio_packets,
        queue_pending_video_packets = queues.pending_video_packets,
        queue_staged_video_backlog_recovery_packets = queues
            .staged_video_backlog_recovery_packets,
        queue_staged_video_backlog_recovery_bytes = queues.staged_video_backlog_recovery_bytes,
        queue_present_depth = queues.present_queue_depth,
        queue_decoder_send_depth = queues.decoder_send_queue_depth,
        queue_decoder_in_flight_packets = queues.decoder_in_flight_packets,
        queue_decoder_ready_depth = queues.decoder_ready_queue_depth.unwrap_or_default(),
        active_render_leases = queues.active_render_leases,
        deferred_render_releases = queues.deferred_render_releases,
        texture_slots_capacity = texture_slots.map(|slots| slots.capacity).unwrap_or_default(),
        texture_slots_created = texture_slots.map(|slots| slots.slots).unwrap_or_default(),
        texture_slots_in_use = texture_slots.map(|slots| slots.in_use).unwrap_or_default(),
        texture_slots_free_surfaces = texture_slots
            .map(|slots| slots.free_surfaces)
            .unwrap_or_default(),
        texture_slots_waiting_gpu_completion = texture_slots
            .map(|slots| slots.waiting_gpu_completion)
            .unwrap_or_default(),
        texture_slots_waiting_decoder_reuse = texture_slots
            .map(|slots| slots.waiting_decoder_reuse)
            .unwrap_or_default(),
        texture_import_failures = texture_slots
            .map(|slots| slots.import_failures)
            .unwrap_or_default(),
        texture_imports_created = texture_slots
            .map(|slots| slots.imports_created)
            .unwrap_or_default(),
        texture_imports_reused = texture_slots
            .map(|slots| slots.imports_reused)
            .unwrap_or_default(),
        texture_imports_replaced = texture_slots
            .map(|slots| slots.imports_replaced)
            .unwrap_or_default(),
        render_resource_lock_busy_count = diagnostics.render_resource_lock_busy_count,
        render_resource_previous_frame_reuse_count = diagnostics
            .render_resource_previous_frame_reuse_count,
        render_resource_lock_wait_samples = render_resource_lock_wait.samples,
        render_resource_lock_wait_avg_ms = duration_ms(render_resource_lock_wait.average),
        render_resource_lock_wait_worst_ms = latency_worst_ms(render_resource_lock_wait),
        gpu_submit_present_samples = gpu_submit_present.samples,
        gpu_submit_present_avg_ms = duration_ms(gpu_submit_present.average),
        gpu_submit_present_worst_ms = latency_worst_ms(gpu_submit_present),
        release_ack_samples = release_acknowledgement.samples,
        release_ack_avg_ms = duration_ms(release_acknowledgement.average),
        release_ack_worst_ms = latency_worst_ms(release_acknowledgement),
        worker_scheduler_samples = worker_scheduler.samples,
        worker_scheduler_avg_ms = duration_ms(worker_scheduler.average),
        worker_scheduler_worst_ms = latency_worst_ms(worker_scheduler),
        demux_read_samples = demux_read.samples,
        demux_read_avg_ms = duration_ms(demux_read.average),
        demux_read_worst_ms = latency_worst_ms(demux_read),
        decoder_submit_samples = decoder_submit.samples,
        decoder_submit_avg_ms = duration_ms(decoder_submit.average),
        decoder_submit_worst_ms = latency_worst_ms(decoder_submit),
        decoder_event_drain_samples = decoder_event_drain.samples,
        decoder_event_drain_avg_ms = duration_ms(decoder_event_drain.average),
        decoder_event_drain_worst_ms = latency_worst_ms(decoder_event_drain),
        dma_buf_import_samples = dma_buf_import.samples,
        dma_buf_import_avg_ms = duration_ms(dma_buf_import.average),
        dma_buf_import_worst_ms = latency_worst_ms(dma_buf_import),
        decoded_frame_publish_channel_full_count = publish_pressure
            .frame_publish_channel_full_count,
        decoded_frame_publish_retry_count = publish_pressure.pending_publish_retry_count,
        decoded_frame_publish_max_ms = duration_ms(
            publish_pressure.max_decoded_frame_publish_latency,
        ),
        decoder_control_len = decoder_control_channel
            .map(|pressure| pressure.control_channel_len)
            .unwrap_or_default(),
        decoder_control_capacity = decoder_control_channel
            .map(|pressure| pressure.control_channel_capacity)
            .unwrap_or_default(),
        decoder_control_full_count = decoder_control_channel
            .map(|pressure| pressure.control_channel_full_count)
            .unwrap_or_default(),
        decoder_release_control_send_fail_count = decoder_control_channel
            .map(|pressure| pressure.release_control_send_fail_count)
            .unwrap_or_default(),
        decoder_flush_control_send_fail_count = decoder_control_channel
            .map(|pressure| pressure.flush_control_send_fail_count)
            .unwrap_or_default(),
        "slow render frame stage timings"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Startup frame получает стабильный fallback budget до появления измеренного video cadence.
    #[test]
    fn startup_frame_budget_falls_back_until_player_reports_video_cadence() {
        let mut player_snapshot = PlayerSnapshot::default();

        assert_eq!(
            render_frame_budget(&player_snapshot),
            DEFAULT_RENDER_FRAME_BUDGET
        );

        player_snapshot.video_frame_duration_estimate = Duration::from_millis(40);
        assert_eq!(
            render_frame_budget(&player_snapshot),
            Duration::from_millis(40)
        );
    }
}
