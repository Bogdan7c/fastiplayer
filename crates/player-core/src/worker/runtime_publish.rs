use super::*;

impl LatestSnapshotPublisher {
    /// Создаёт publisher с private drain receiver clone.
    pub(super) fn new(
        snapshot_tx: Sender<PlayerSnapshot>,
        snapshot_rx_for_drain_latest: Receiver<PlayerSnapshot>,
    ) -> Self {
        Self {
            snapshot_tx,
            snapshot_rx_for_drain_latest,
        }
    }

    /// Публикует latest snapshot, удаляя устаревший pending snapshot.
    fn publish(&self, snapshot: PlayerSnapshot) {
        drain_receiver_without_blocking(&self.snapshot_rx_for_drain_latest);
        if let Err(error) = self.snapshot_tx.try_send(snapshot) {
            match error {
                TrySendError::Full(_) => debug!("Latest snapshot channel is full"),
                TrySendError::Disconnected(_) => debug!("Snapshot receiver disconnected"),
            }
        }
    }
}

impl PlayerWorkerRuntime {
    /// Обрабатывает timeout, который не был command/render event-ом.
    pub(super) fn handle_worker_timeout(&mut self, deadline: WorkerWakeupDeadline) {
        let WorkerWakeupDeadline::Playback { plan, deadline } = deadline;
        self.run_tick_for_wakeup_plan(plan, deadline);
    }

    /// Выполняет playback tick по media-clock-driven wakeup plan.
    pub(super) fn run_tick_for_wakeup_plan(
        &mut self,
        plan: PlayerWorkerWakeupPlan,
        deadline: Instant,
    ) {
        let now = Instant::now();
        let tick_late_by = now.saturating_duration_since(deadline);
        self.session
            .record_worker_wakeup(plan.diagnostics(tick_late_by));
        let tick_result = self.session.tick(PlayerTickContext::with_timing(
            now,
            self.config.tick_config,
            tick_late_by,
        ));
        self.last_tick_at = now;
        self.publish_tick_result(tick_result);
        self.log_active_seek_stall_if_needed(now);
        self.log_diagnostics_summary_if_due(now);
        self.publish_session_outputs();
    }

    /// Пишет throttled warn-log, если active seek уже выглядит как зависший transition.
    pub(super) fn log_active_seek_stall_if_needed(&mut self, now: Instant) {
        let Some(active_seek) = self
            .session
            .active_seek_diagnostics(now, &self.config.tick_config)
        else {
            self.last_seek_stall_log_key = None;
            self.last_seek_stall_log_at = None;
            return;
        };

        let active_seek_key = (active_seek.generation, active_seek.kind);
        if self.last_seek_stall_log_key != Some(active_seek_key) {
            self.last_seek_stall_log_key = Some(active_seek_key);
            self.last_seek_stall_log_at = None;
        }

        let log_after = seek_stall_log_after(active_seek, self.config.tick_config);
        if active_seek.age < log_after {
            return;
        }

        if self.last_seek_stall_log_at.is_some_and(|last_log_at| {
            now.saturating_duration_since(last_log_at) < SEEK_STALL_LOG_INTERVAL
        }) {
            return;
        }

        self.last_seek_stall_log_at = Some(now);
        let scheduler_timing =
            scheduler_timing_diagnostics(&self.session, &self.config.tick_config, now);
        log_active_seek_stall(active_seek, scheduler_timing);
    }

    /// Пишет короткую diagnostics summary только при включённом debug tracing.
    pub(super) fn log_diagnostics_summary_if_due(&mut self, now: Instant) {
        if !tracing::enabled!(tracing::Level::DEBUG) {
            return;
        }

        if now.saturating_duration_since(self.last_diagnostics_summary_at)
            < DIAGNOSTICS_SUMMARY_INTERVAL
        {
            return;
        }

        let summary = self.session.diagnostics_log_summary();
        if !summary.has_activity() {
            return;
        }

        self.last_diagnostics_summary_at = now;
        let worst_stage = summary
            .worst_stage
            .map(|stage| stage.metric_name())
            .unwrap_or("none");
        let worst_latency_ms = summary
            .worst_latency
            .map(|latency| latency.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let wake_reason = summary
            .worker_wakeup
            .reason
            .map(|reason| reason.metric_name())
            .unwrap_or("none");
        let wake_delay_ms = summary.worker_wakeup.planned_delay.map(duration_to_millis);
        let wake_late_ms = duration_to_millis(summary.worker_wakeup.tick_late_by);
        let pts_target_ms = summary
            .worker_wakeup
            .frame_timing
            .map(|timing| timing.front_frame_delta_from_target_us as f64 / 1000.0);
        let texture_slots = summary.queues.texture_slots;
        let control_channel = summary.queues.decoder_control_channel;
        let latencies = summary.worst_latencies;
        let render_resource_lock_wait = latencies.render_resource_lock_wait;
        let publish_pressure = summary.decoder_frame_publish_pressure;
        debug!(
            drops = summary.drops_total,
            drops_playback_or_render = summary.drops.playback_or_render,
            drops_seek_discard = summary.drops.seek_discard,
            drops_late = summary.drops.late,
            drops_queue = summary.drops.queue_overflow,
            drops_stale_generation = summary.drops.stale_generation,
            drops_seek_preroll = summary.drops.seek_preroll,
            drops_decoder_starvation = summary.drops.decoder_starvation,
            seek_bootstrap_dropped_until_keyframe = summary.seek_bootstrap.dropped_until_keyframe,
            seek_bootstrap_first_accepted_keyframe = ?summary
                .seek_bootstrap
                .first_accepted_keyframe,
            pauses = summary.pauses_total,
            pauses_sync_waiting = summary.pauses.sync_waiting,
            pauses_present_queue = summary.pauses.waiting_for_present_queue,
            pauses_gpu_release = summary.pauses.waiting_for_gpu_release,
            repeated_video_frames = summary.repeated_video_frames,
            render_resource_lock_busy_count = summary.render_resource_lock_busy_count,
            render_resource_previous_frame_reuse_count = summary.render_resource_previous_frame_reuse_count,
            decoder_publish_channel_full_count = publish_pressure.frame_publish_channel_full_count,
            decoder_publish_retry_count = publish_pressure.pending_publish_retry_count,
            decoder_publish_total_ms = duration_to_millis(
                publish_pressure.total_decoded_frame_publish_latency
            ),
            decoder_publish_max_ms = duration_to_millis(
                publish_pressure.max_decoded_frame_publish_latency
            ),
            memory_path = ?summary.zero_copy_memory_path,
            worst_stage,
            worst_latency_ms,
            demux_worst_ms = ?worst_latency_millis(latencies.demux_read),
            decoder_submit_worst_ms = ?worst_latency_millis(latencies.decoder_submit),
            decoder_sync_worst_ms = ?worst_latency_millis(latencies.hardware_sync),
            import_worst_ms = ?worst_latency_millis(latencies.dma_buf_import),
            worker_worst_ms = ?worst_latency_millis(latencies.worker_scheduler),
            render_acquire_worst_ms = ?worst_latency_millis(latencies.render_acquire),
            render_resource_lock_wait_count = render_resource_lock_wait.samples,
            render_resource_lock_wait_avg_ms = duration_to_millis(render_resource_lock_wait.average),
            render_resource_lock_wait_max_ms = ?worst_latency_millis(render_resource_lock_wait),
            gpu_submit_present_worst_ms = ?worst_latency_millis(latencies.gpu_submit_present),
            release_ack_worst_ms = ?worst_latency_millis(latencies.release_acknowledgement),
            wake_reason,
            wake_delay_ms = ?wake_delay_ms,
            wake_late_ms,
            pts_target_ms = ?pts_target_ms,
            pending_video_packets = summary.queues.pending_video_packets,
            staged_video_backlog_recovery_packets = summary
                .queues
                .staged_video_backlog_recovery_packets,
            staged_video_backlog_recovery_bytes = summary.queues.staged_video_backlog_recovery_bytes,
            present_queue_depth = summary.queues.present_queue_depth,
            decoder_in_flight_packets = summary.queues.decoder_in_flight_packets,
            decoder_control_channel_len = ?control_channel.map(|pressure| pressure.control_channel_len),
            decoder_control_channel_capacity = ?control_channel.map(|pressure| pressure.control_channel_capacity),
            decoder_control_channel_full_count = ?control_channel.map(|pressure| pressure.control_channel_full_count),
            decoder_release_control_send_fail_count = ?control_channel.map(|pressure| pressure.release_control_send_fail_count),
            decoder_flush_control_send_fail_count = ?control_channel.map(|pressure| pressure.flush_control_send_fail_count),
            active_render_leases = summary.queues.active_render_leases,
            texture_in_use = ?texture_slots.map(|slots| slots.in_use),
            texture_capacity = ?texture_slots.map(|slots| slots.capacity),
            texture_free = ?texture_slots.map(|slots| slots.free_surfaces),
            texture_waiting_gpu = ?texture_slots.map(|slots| slots.waiting_gpu_completion),
            imports_created = ?texture_slots.map(|slots| slots.imports_created),
            imports_reused = ?texture_slots.map(|slots| slots.imports_reused),
            imports_replaced = ?texture_slots.map(|slots| slots.imports_replaced),
            import_failures = ?texture_slots.map(|slots| slots.import_failures),
            "Playback diagnostics summary"
        );
    }

    /// Публикует latest snapshot и накопленные session events.
    pub(super) fn publish_session_outputs(&mut self) {
        self.render_bridge
            .publish_latest_present_frame(&mut self.session);

        let snapshot = self
            .session
            .snapshot_with_frame_counters(FrameCounters::default());
        self.snapshot_publisher.publish(snapshot);

        for event in self.session.take_events() {
            self.publish_worker_event(PlayerWorkerEvent::Player(event));
        }
        for event in self.session.take_scrub_events() {
            self.publish_worker_event(PlayerWorkerEvent::Scrub(event));
        }
    }

    /// Публикует tick telemetry без блокировки worker-а.
    fn publish_tick_result(&self, tick_result: PlayerTickResult) {
        self.publish_worker_event(PlayerWorkerEvent::Tick(tick_result));
    }

    /// Публикует worker event, сбрасывая событие при переполнении receiver-а.
    pub(super) fn publish_worker_event(&self, event: PlayerWorkerEvent) {
        if let Err(error) = self.event_tx.try_send(event) {
            match error {
                TrySendError::Full(_) => debug!("Player worker event channel is full"),
                TrySendError::Disconnected(_) => debug!("Player worker event receiver dropped"),
            }
        }
    }
}

/// Возвращает возраст seek-а, после которого diagnostics warning становится полезным.
fn seek_stall_log_after(
    _active_seek: ActiveSeekDiagnosticsSnapshot,
    tick_config: PlayerTickConfig,
) -> Duration {
    tick_config
        .seek_commit_timeout
        .mul_f64(0.05)
        .max(FINAL_SEEK_STALL_LOG_MIN_AFTER)
        .min(FINAL_SEEK_STALL_LOG_MAX_AFTER)
        .min(tick_config.seek_commit_timeout)
}

/// Пишет один structured event, достаточный для локализации active seek blocker-а.
fn log_active_seek_stall(
    active_seek: ActiveSeekDiagnosticsSnapshot,
    scheduler_timing: SchedulerTimingDiagnosticsSnapshot,
) {
    let queues = active_seek.queues;
    let texture_slots = queues.texture_slots;
    let preroll = active_seek.accurate_preroll;
    let preroll_stages = preroll.stages;
    let preroll_counters = preroll.counters;
    let preroll_demux = preroll_counters.demux_events;

    warn!(
        kind = active_seek.kind,
        blocker = %active_seek.blocker.metric_name(),
        blocker_state = ?active_seek.blocker,
        generation = active_seek.generation,
        pipeline_generation = active_seek.pipeline_generation,
        selected_video_track_id = ?active_seek.selected_video_track_id,
        selected_audio_track_id = ?active_seek.selected_audio_track_id,
        age_ms = duration_to_millis(active_seek.age),
        target_ms = duration_to_millis(active_seek.target),
        actual_ms = duration_to_millis(active_seek.actual),
        audio_clock_ms = duration_to_millis(scheduler_timing.audio_clock),
        presentation_clock_position_ms =
            duration_to_millis(scheduler_timing.presentation_clock_position),
        target_media_time_for_present_ms =
            duration_to_millis(scheduler_timing.target_media_time_for_present),
        resume_intent = active_seek.resume_intent,
        seek_mode = ?active_seek.seek_mode,
        video_gate_ready = active_seek.video_gate_ready,
        audio_gate_ready = active_seek.audio_gate_ready,
        target_frame_presented = active_seek.target_frame_presented,
        ready_video_frames = active_seek.ready_video_frames,
        required_video_frames = active_seek.required_video_frames,
        present_frame_pts_ms = ?active_seek.present_frame_pts.map(duration_to_millis),
        front_queued_frame_pts_ms = ?active_seek.front_queued_frame_pts.map(duration_to_millis),
        demuxing_active = active_seek.demuxing_active,
        draining_after_eof = active_seek.draining_after_eof,
        stale_frame = active_seek.stale_frame,
        stale_generation_discards = active_seek.stale_generation_discards,
        seek_bootstrap_dropped_until_keyframe = active_seek
            .seek_bootstrap
            .dropped_until_keyframe,
        seek_bootstrap_first_accepted_keyframe = ?active_seek
            .seek_bootstrap
            .first_accepted_keyframe,
        last_pause_reason = ?active_seek.last_pause_reason,
        accurate_preroll_active = preroll.active,
        first_post_seek_packet_elapsed_ms = ?preroll_stages
            .first_post_seek_packet_elapsed
            .map(duration_to_millis),
        first_target_video_packet_elapsed_ms = ?preroll_stages
            .first_target_or_after_video_packet_elapsed
            .map(duration_to_millis),
        first_decoded_target_frame_elapsed_ms = ?preroll_stages
            .first_decoded_target_frame_elapsed
            .map(duration_to_millis),
        first_queued_target_frame_elapsed_ms = ?preroll_stages
            .first_queued_target_frame_elapsed
            .map(duration_to_millis),
        first_presented_target_frame_elapsed_ms = ?preroll_stages
            .first_presented_target_frame_elapsed
            .map(duration_to_millis),
        seek_preroll_demux_audio_packets = preroll_demux.audio_packets,
        seek_preroll_demux_video_packets = preroll_demux.video_packets,
        seek_preroll_demux_eof = preroll_demux.end_of_stream,
        seek_preroll_demux_tracks_changed = preroll_demux.tracks_changed,
        seek_preroll_demux_errors = preroll_demux.errors,
        skipped_audio_preroll_packets = preroll_counters.skipped_audio_preroll_packets,
        seek_video_packets_sent = preroll_counters.seek_video_packets_sent,
        video_preroll_packets_sent = preroll_counters.video_preroll_packets_sent,
        target_or_after_video_packets_sent =
            preroll_counters.target_or_after_video_packets_sent,
        decoded_pre_target_frames_dropped =
            preroll_counters.decoded_pre_target_frames_dropped,
        seek_preroll_decoder_backpressure_pauses =
            preroll_counters.decoder_backpressure_pauses,
        pending_audio_packets = queues.pending_audio_packets,
        pending_video_packets = queues.pending_video_packets,
        staged_video_backlog_recovery_packets = queues.staged_video_backlog_recovery_packets,
        staged_video_backlog_recovery_bytes = queues.staged_video_backlog_recovery_bytes,
        present_queue_depth = queues.present_queue_depth,
        decoder_send_queue_depth = queues.decoder_send_queue_depth,
        decoder_in_flight_packets = queues.decoder_in_flight_packets,
        active_render_leases = queues.active_render_leases,
        deferred_render_releases = queues.deferred_render_releases,
        texture_capacity = ?texture_slots.map(|slots| slots.capacity),
        texture_in_use = ?texture_slots.map(|slots| slots.in_use),
        texture_available = ?texture_slots.map(|slots| slots.available_slots()),
        texture_free_surfaces = ?texture_slots.map(|slots| slots.free_surfaces),
        texture_waiting_gpu = ?texture_slots.map(|slots| slots.waiting_gpu_completion),
        texture_waiting_decoder_reuse = ?texture_slots.map(|slots| slots.waiting_decoder_reuse),
        texture_import_failures = ?texture_slots.map(|slots| slots.import_failures),
        imports_created = ?texture_slots.map(|slots| slots.imports_created),
        imports_reused = ?texture_slots.map(|slots| slots.imports_reused),
        imports_replaced = ?texture_slots.map(|slots| slots.imports_replaced),
        "Active seek transaction is still waiting"
    );
}

/// Опустошает receiver без ожидания; используется для latest/coalescing каналов.
fn drain_receiver_without_blocking<T>(receiver: &Receiver<T>) {
    while receiver.try_recv().is_ok() {}
}

/// Конвертирует latency в миллисекунды для compact diagnostics logs.
fn duration_to_millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Возвращает worst latency одного stage в миллисекундах, если stage уже видел samples.
fn worst_latency_millis(counter: LatencyCounterSnapshot) -> Option<f64> {
    counter
        .worst
        .map(|sample| duration_to_millis(sample.duration))
}
