use std::time::Duration;

use frame_server_core::ScrubEventDiagnostics;
use media_core::PacketKeyframe;

use crate::{
    PipelineLatencyStage, PipelinePauseReason, PipelineQueueDepthSnapshot,
    PlaybackDiagnosticsLogSummary, PlaybackDiagnosticsSnapshot, SeekBootstrapDiagnosticsSnapshot,
    TextureSlotPressureSnapshot, VideoDropReason, WorkerWakeupDiagnosticsSnapshot,
};

use super::PlayerSession;

impl PlayerSession {
    /// Сбрасывает media-specific diagnostics вместе с media lifecycle reset-ом session.
    pub(in crate::session) fn reset_diagnostics_for_media(&mut self) {
        self.diagnostics.reset();
    }

    /// Возвращает read-only diagnostics snapshot с актуальными queue depths.
    #[must_use]
    pub(in crate::session) fn diagnostics_snapshot(&self) -> PlaybackDiagnosticsSnapshot {
        self.diagnostics_snapshot_with_queues(self.diagnostic_queue_depths())
    }

    /// Возвращает diagnostics snapshot с уже снятыми queue depths.
    ///
    /// Метод нужен вызывающим путям, которые должны использовать один и тот же queue snapshot
    /// для нескольких related diagnostics decisions без повторного чтения pipeline state.
    #[must_use]
    pub(in crate::session) fn diagnostics_snapshot_with_queues(
        &self,
        queues: PipelineQueueDepthSnapshot,
    ) -> PlaybackDiagnosticsSnapshot {
        self.diagnostics.snapshot_with_queues(queues)
    }

    /// Записывает latency sample с queue attribution на момент события.
    pub(crate) fn record_pipeline_latency(
        &mut self,
        stage: PipelineLatencyStage,
        duration: Duration,
        pts: Option<Duration>,
        memory_path: Option<video_core::FrameMemoryPath>,
    ) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics
            .record_latency(stage, duration, pts, memory_path, queues);
    }

    /// Записывает decoded frame diagnostics из backend-neutral frame contract.
    pub(crate) fn record_decoded_frame_diagnostics(&mut self, frame: &video_core::DecodedFrame) {
        let mut queues = self.diagnostic_queue_depths();
        queues.decoder_ready_queue_depth = frame.diagnostics.decoder_ready_queue_depth;
        queues.texture_slots =
            frame
                .diagnostics
                .resource_pool
                .map(|resource_pool| TextureSlotPressureSnapshot {
                    capacity: resource_pool.capacity,
                    slots: resource_pool.slots,
                    in_use: resource_pool.in_use,
                    free_surfaces: resource_pool.free_surfaces,
                    waiting_gpu_completion: resource_pool.waiting_gpu_completion,
                    waiting_decoder_reuse: resource_pool.waiting_decoder_reuse,
                    import_failures: resource_pool.import_failures,
                    imports_created: resource_pool.imports_created,
                    imports_reused: resource_pool.imports_reused,
                    imports_replaced: resource_pool.imports_replaced,
                });
        self.diagnostics.observe_decoded_frame(frame, queues);
    }

    /// Записывает pressure counters decoder-thread publish boundary.
    pub(crate) fn record_decoded_frame_publish_pressure(
        &mut self,
        pressure: video_core::VideoFramePublishPressureDiagnostics,
    ) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics
            .record_decoded_frame_publish_pressure(pressure, queues);
    }

    /// Записывает typed video drop reason с queue attribution на момент drop-а.
    pub(crate) fn record_video_drop(&mut self, pts: Option<Duration>, reason: VideoDropReason) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics.record_drop(pts, reason, queues);
    }

    /// Начинает новое diagnostics окно ожидания decode-start packet-а после seek/flush.
    pub(crate) fn record_video_decoder_bootstrap_started(&mut self) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics.start_seek_bootstrap(queues);
    }

    /// Записывает packet, отброшенный из-за ожидания post-flush keyframe/decode-start.
    pub(crate) fn record_video_packet_dropped_until_keyframe(
        &mut self,
    ) -> SeekBootstrapDiagnosticsSnapshot {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics
            .record_seek_bootstrap_drop_until_keyframe(queues)
    }

    /// Записывает первый packet, который decoder bootstrap принимает как decode-start.
    pub(crate) fn record_video_decoder_bootstrap_accepted(
        &mut self,
        keyframe: PacketKeyframe,
    ) -> SeekBootstrapDiagnosticsSnapshot {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics
            .record_seek_bootstrap_first_accepted(keyframe, queues)
    }

    /// Записывает typed pipeline pause с queue attribution на момент backpressure.
    pub(crate) fn record_pipeline_pause(&mut self, reason: PipelinePauseReason) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics.record_pause(reason, queues);
    }

    /// Записывает повтор текущего present frame отдельно от drop counters.
    pub(crate) fn record_repeated_video_frame(&mut self) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics.record_repeated_video_frame(queues);
    }

    /// Записывает последнее решение worker wakeup planner-а.
    pub(crate) fn record_worker_wakeup(&mut self, wakeup: WorkerWakeupDiagnosticsSnapshot) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics.record_worker_wakeup(wakeup, queues);
    }

    /// Записывает diagnostics normalized scrub event-а, когда caller уже работает с event.
    pub(crate) fn record_scrub_event_diagnostics(&mut self, diagnostics: ScrubEventDiagnostics) {
        self.diagnostics.record_scrub_event_diagnostics(diagnostics);
    }

    /// Записывает результат render acquire.
    pub(crate) fn record_render_acquire_wait(&mut self, wait: Duration) {
        self.record_pipeline_latency(PipelineLatencyStage::RenderAcquire, wait, None, None);
    }

    /// Записывает ожидание resource pool lock-а внутри renderer materialization boundary.
    pub(crate) fn record_render_resource_lock_wait(
        &mut self,
        wait: Duration,
        pts: Option<Duration>,
        memory_path: Option<video_core::FrameMemoryPath>,
    ) {
        self.record_pipeline_latency(
            PipelineLatencyStage::RenderResourceLockWait,
            wait,
            pts,
            memory_path,
        );
    }

    /// Записывает busy outcome non-blocking renderer resource lookup-а.
    pub(crate) fn record_render_resource_lock_busy(&mut self) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics.record_render_resource_lock_busy(queues);
    }

    /// Записывает reuse предыдущего renderable frame-а из-за busy resource lock-а.
    pub(crate) fn record_render_resource_previous_frame_reuse(&mut self) {
        let queues = self.diagnostic_queue_depths();
        self.diagnostics
            .record_render_resource_previous_frame_reuse(queues);
    }

    /// Записывает latency release ack от render side до worker.
    pub(crate) fn record_release_ack_latency(&mut self, latency: Duration) {
        self.record_pipeline_latency(
            PipelineLatencyStage::ReleaseAcknowledgement,
            latency,
            None,
            None,
        );
    }

    /// Записывает renderer-side submit/present latency без доступа render loop-а к session.
    pub(crate) fn record_gpu_submit_present_latency(&mut self, latency: Duration) {
        self.record_pipeline_latency(PipelineLatencyStage::GpuSubmitPresent, latency, None, None);
    }

    /// Возвращает компактную diagnostics summary для throttled debug logs.
    #[must_use]
    pub(crate) fn diagnostics_log_summary(&self) -> PlaybackDiagnosticsLogSummary {
        self.diagnostics.log_summary(self.diagnostic_queue_depths())
    }

    /// Собирает codec/render-neutral queue depths без изменения pipeline queues.
    pub(in crate::session) fn diagnostic_queue_depths(&self) -> PipelineQueueDepthSnapshot {
        let decoder_send_queue_depth = self
            .pipeline
            .video_decoder_packet_queue_depth()
            .unwrap_or(self.pipeline.pending_video_packet_len());
        let decoder_resource_snapshot = self.pipeline.video_decoder_resource_snapshot();

        PipelineQueueDepthSnapshot {
            pending_audio_packets: self.pipeline.pending_audio_packet_len(),
            pending_video_packets: self.pipeline.pending_video_packet_len(),
            present_queue_depth: self.pipeline.video_present_queue_len(),
            decoder_send_queue_depth,
            decoder_in_flight_packets: self.pipeline.video_decode_in_flight_packets(),
            decoder_ready_queue_depth: None,
            active_render_leases: self.pipeline.active_render_lease_count(),
            deferred_render_releases: self.pipeline.deferred_render_release_count(),
            texture_slots: decoder_resource_snapshot.map(|texture_stats| {
                TextureSlotPressureSnapshot {
                    capacity: texture_stats.capacity,
                    slots: texture_stats.slots,
                    in_use: texture_stats.in_use,
                    free_surfaces: texture_stats.free_surfaces,
                    waiting_gpu_completion: texture_stats.waiting_gpu_completion,
                    waiting_decoder_reuse: texture_stats.waiting_decoder_reuse,
                    import_failures: texture_stats.import_failures,
                    imports_created: texture_stats.imports_created,
                    imports_reused: texture_stats.imports_reused,
                    imports_replaced: texture_stats.imports_replaced,
                }
            }),
            decoder_control_channel: self.pipeline.video_decoder_control_channel_pressure(),
        }
    }
}
