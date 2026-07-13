use super::*;

impl WorkerDecoderActivityState {
    /// Готовит optional decoder activity wait source из snapshot-а, снятого до planning.
    fn wait_source_for_status(
        &mut self,
        activity_status: &VideoDecoderActivityStatus,
    ) -> Option<DecoderActivityWaitSource> {
        match activity_status {
            VideoDecoderActivityStatus::Available { snapshot } => {
                self.wait_source_for_available_snapshot(snapshot)
            }
            VideoDecoderActivityStatus::Unavailable(reason) => {
                self.disable_current_source_if_terminal(reason);
                None
            }
            VideoDecoderActivityStatus::AbsentDecoder | VideoDecoderActivityStatus::Unsupported => {
                None
            }
        }
    }

    /// Возвращает wait source для доступного notifier-а или `None`, если source отключён.
    fn wait_source_for_available_snapshot(
        &mut self,
        snapshot: &VideoDecoderActivitySnapshot,
    ) -> Option<DecoderActivityWaitSource> {
        let captured_epoch = snapshot.captured_epoch()?;
        let pulse_receiver = snapshot.pulse_receiver()?;
        let source_id = self.source_id_for_receiver(&pulse_receiver, captured_epoch);

        if self.disabled_source_id == Some(source_id) {
            return None;
        }

        Some(DecoderActivityWaitSource {
            source_id,
            observed_epoch: self.last_observed_epoch,
            snapshot: snapshot.clone(),
            pulse_receiver,
        })
    }

    /// Назначает новый source id только при реальной замене receiver/channel-а.
    fn source_id_for_receiver(
        &mut self,
        pulse_receiver: &Receiver<()>,
        captured_epoch: VideoDecoderActivityEpoch,
    ) -> DecoderActivitySourceId {
        if let Some(active_source) = self.active_source.as_ref()
            && active_source.pulse_receiver.same_channel(pulse_receiver)
        {
            return active_source.source_id;
        }

        let source_id = self.next_source_id;
        self.next_source_id = self.next_source_id.saturating_add(1);
        self.active_source = Some(DecoderActivitySource {
            source_id,
            pulse_receiver: pulse_receiver.clone(),
        });
        self.last_observed_epoch = captured_epoch;
        source_id
    }

    /// Проверяет, отключён ли source после terminal notifier outcome.
    #[must_use]
    fn source_is_disabled(&self, source_id: DecoderActivitySourceId) -> bool {
        self.disabled_source_id == Some(source_id)
    }

    /// Запоминает epoch, из-за которого worker уже проснулся и запустит playback tick.
    fn mark_activity_observed(
        &mut self,
        source_id: DecoderActivitySourceId,
        epoch: VideoDecoderActivityEpoch,
    ) {
        if self
            .active_source
            .as_ref()
            .is_some_and(|active_source| active_source.source_id == source_id)
        {
            self.last_observed_epoch = epoch;
        }
    }

    /// Отключает source после fatal/disconnected outcome, чтобы не читать его в следующем select.
    fn disable_source_if_terminal(
        &mut self,
        source_id: DecoderActivitySourceId,
        reason: &VideoDecoderActivityUnavailableReason,
    ) {
        if Self::terminal_unavailable_reason(reason) {
            self.disabled_source_id = Some(source_id);
        }
    }

    /// Отключает текущий source, если handle уже вернул terminal unavailable snapshot.
    fn disable_current_source_if_terminal(
        &mut self,
        reason: &VideoDecoderActivityUnavailableReason,
    ) {
        if !Self::terminal_unavailable_reason(reason) {
            return;
        }

        self.disabled_source_id = self
            .active_source
            .as_ref()
            .map(|active_source| active_source.source_id);
    }

    /// Только fatal/disconnected notifier означает, что этот source нельзя снова включать.
    fn terminal_unavailable_reason(reason: &VideoDecoderActivityUnavailableReason) -> bool {
        matches!(
            reason,
            VideoDecoderActivityUnavailableReason::DisconnectedNotifier
                | VideoDecoderActivityUnavailableReason::FatalNotifier(_)
        )
    }
}

impl PlannedWorkerWait {
    /// Возвращает timeout выбранного playback wakeup-а.
    #[must_use]
    const fn timeout(&self) -> Duration {
        self.wakeup.timeout()
    }

    /// Возвращает deadline выбранного playback wakeup-а.
    #[must_use]
    pub(super) const fn deadline(&self) -> WorkerWakeupDeadline {
        self.wakeup.deadline()
    }
}

impl PlayerWorkerRuntime {
    /// Главный цикл worker thread.
    pub(super) fn run(mut self) {
        self.publish_session_outputs();

        loop {
            self.drain_render_feedback();
            self.drain_playback_intent_updates();

            if self.shutdown_rx.try_recv().is_ok() {
                self.handle_shutdown_request();
                break;
            }

            let processed_commands = self.drain_pending_command_batch();
            self.service_worker_fairness_checkpoint(processed_commands);
            self.log_active_seek_stall_if_needed(Instant::now());

            if self.session.is_shutdown_requested() {
                break;
            }

            if processed_commands == MAX_COMMANDS_PER_LOOP {
                continue;
            }

            if self.wait_for_worker_wakeup() {
                break;
            }
        }

        self.publish_session_outputs();
    }

    /// Снимает render feedback, которым worker владеет как частью session lifecycle.
    fn drain_render_feedback(&mut self) {
        self.render_bridge.drain_releases(&mut self.session);
        self.render_bridge.drain_diagnostics(&mut self.session);
    }

    /// Обрабатывает bounded batch pending command-ов без монополизации worker loop-а.
    pub(super) fn drain_pending_command_batch(&mut self) -> usize {
        let mut processed_commands = 0;

        for _ in 0..MAX_COMMANDS_PER_LOOP {
            self.drain_playback_intent_updates();
            let Some(command) = self.receive_next_command() else {
                break;
            };

            self.handle_worker_command(command);
            self.publish_session_outputs();
            processed_commands += 1;

            if self.session.is_shutdown_requested() {
                break;
            }
        }

        processed_commands
    }

    /// Обязательная fairness-точка после command batch-а.
    pub(super) fn service_worker_fairness_checkpoint(&mut self, processed_commands: usize) {
        self.drain_render_feedback();
        if processed_commands > 0 {
            self.run_overdue_playback_tick();
        }
    }

    /// Ждёт ближайший command/render/shutdown wakeup вместо fixed idle polling.
    fn wait_for_worker_wakeup(&mut self) -> bool {
        match self.plan_next_worker_wakeup_with_decoder_activity() {
            Some(wait_plan) if wait_plan.timeout().is_zero() => {
                self.handle_worker_timeout(wait_plan.deadline());
                false
            }
            Some(wait_plan) => self.wait_for_worker_wakeup_with_timeout(wait_plan),
            None => self.wait_for_worker_wakeup_until_event(),
        }
    }

    /// Блокируется до события или ближайшего playback deadline-а.
    pub(super) fn wait_for_worker_wakeup_with_timeout(
        &mut self,
        wait_plan: PlannedWorkerWait,
    ) -> bool {
        loop {
            let wakeup = wait_plan.wakeup;
            let timeout = Self::remaining_wakeup_timeout(wakeup);
            if timeout.is_zero() {
                self.handle_worker_timeout(wakeup.deadline());
                return false;
            }

            if let Some(shutdown_requested) = self.handle_ready_command_or_shutdown_before_select()
            {
                return shutdown_requested;
            }

            let decoder_activity = wait_plan
                .decoder_activity
                .as_ref()
                .filter(|activity| !self.decoder_activity.source_is_disabled(activity.source_id));

            let wait_outcome = if let Some(decoder_activity) = decoder_activity {
                if let DecoderActivityWaitAction::RunPlaybackTick =
                    self.check_decoder_activity_before_select(decoder_activity)
                {
                    self.handle_worker_timeout(wakeup.deadline());
                    return false;
                }

                self.wait_for_worker_timed_event_with_decoder_activity(
                    wakeup,
                    decoder_activity,
                    timeout,
                )
            } else {
                self.wait_for_worker_timed_event_without_decoder_activity(wakeup, timeout)
            };

            match wait_outcome {
                WorkerTimedWaitOutcome::ContinueWaiting => {}
                WorkerTimedWaitOutcome::Finished { shutdown_requested } => {
                    return shutdown_requested;
                }
            }
        }
    }

    /// Даёт command/shutdown приоритет над decoder activity, пришедшей после planning.
    fn handle_ready_command_or_shutdown_before_select(&mut self) -> Option<bool> {
        if !self.playback_intent_wake_rx.is_empty() {
            return Some(self.handle_playback_intent_wakeup());
        }
        if let Some(command) = self.receive_next_command() {
            self.handle_worker_command(command);
            self.publish_session_outputs();
            return Some(self.session.is_shutdown_requested());
        }

        match self.shutdown_rx.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) => {
                self.handle_shutdown_request();
                return Some(true);
            }
            Err(TryRecvError::Empty) => {}
        }

        None
    }

    /// Проверяет lost-wakeup окно между snapshot/planning и входом в `select!`.
    fn check_decoder_activity_before_select(
        &mut self,
        decoder_activity: &DecoderActivityWaitSource,
    ) -> DecoderActivityWaitAction {
        let activity_outcome = decoder_activity
            .snapshot
            .activity_since(decoder_activity.observed_epoch);
        self.handle_decoder_activity_wait_outcome(decoder_activity.source_id, activity_outcome)
    }

    /// Ждёт command/shutdown/render или decoder activity до выбранного playback deadline-а.
    fn wait_for_worker_timed_event_with_decoder_activity(
        &mut self,
        wakeup: PlannedWorkerWakeup,
        decoder_activity: &DecoderActivityWaitSource,
        timeout: Duration,
    ) -> WorkerTimedWaitOutcome {
        let decoder_pulse_receiver = decoder_activity.pulse_receiver.clone();

        crossbeam_channel::select_biased! {
            recv(self.playback_intent_wake_rx) -> _ => {
                WorkerTimedWaitOutcome::Finished {
                    shutdown_requested: self.handle_playback_intent_wakeup(),
                }
            }
            recv(self.command_rx) -> command_result => {
                WorkerTimedWaitOutcome::Finished {
                    shutdown_requested: self.handle_command_wakeup(command_result),
                }
            }
            recv(self.shutdown_rx) -> _ => {
                self.handle_shutdown_request();
                WorkerTimedWaitOutcome::Finished {
                    shutdown_requested: true,
                }
            }
            recv(decoder_pulse_receiver) -> activity_result => {
                let activity_outcome = decoder_activity
                    .snapshot
                    .activity_after_recv(decoder_activity.observed_epoch, activity_result);
                match self.handle_decoder_activity_wait_outcome(
                    decoder_activity.source_id,
                    activity_outcome,
                ) {
                    DecoderActivityWaitAction::RunPlaybackTick => {
                        self.handle_worker_timeout(wakeup.deadline());
                        WorkerTimedWaitOutcome::Finished {
                            shutdown_requested: false,
                        }
                    }
                    DecoderActivityWaitAction::ContinueWaiting => {
                        WorkerTimedWaitOutcome::ContinueWaiting
                    }
                }
            }
            recv(self.render_bridge.render_release_receiver()) -> release_result => {
                self.render_bridge
                    .handle_release_wakeup(&mut self.session, release_result);
                self.drain_render_feedback();
                WorkerTimedWaitOutcome::ContinueWaiting
            }
            recv(self.render_bridge.render_acquire_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_acquire_sample_wakeup(&mut self.session, sample_result);
                self.drain_render_feedback();
                WorkerTimedWaitOutcome::ContinueWaiting
            }
            recv(self.render_bridge.render_timing_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_timing_sample_wakeup(&mut self.session, sample_result);
                self.drain_render_feedback();
                WorkerTimedWaitOutcome::ContinueWaiting
            }
            recv(self.render_bridge.resource_lock_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_resource_lock_sample_wakeup(&mut self.session, sample_result);
                self.drain_render_feedback();
                WorkerTimedWaitOutcome::ContinueWaiting
            }
            recv(self.render_bridge.resource_previous_frame_reuse_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_resource_previous_frame_reuse_sample_wakeup(&mut self.session, sample_result);
                self.drain_render_feedback();
                WorkerTimedWaitOutcome::ContinueWaiting
            }
            default(timeout) => {
                self.handle_worker_timeout(wakeup.deadline());
                WorkerTimedWaitOutcome::Finished {
                    shutdown_requested: false,
                }
            },
        }
    }

    /// Ждёт command/shutdown/render или обычный fallback timeout без decoder receiver-а.
    fn wait_for_worker_timed_event_without_decoder_activity(
        &mut self,
        wakeup: PlannedWorkerWakeup,
        timeout: Duration,
    ) -> WorkerTimedWaitOutcome {
        crossbeam_channel::select_biased! {
            recv(self.playback_intent_wake_rx) -> _ => {
                WorkerTimedWaitOutcome::Finished {
                    shutdown_requested: self.handle_playback_intent_wakeup(),
                }
            }
            recv(self.command_rx) -> command_result => {
                WorkerTimedWaitOutcome::Finished {
                    shutdown_requested: self.handle_command_wakeup(command_result),
                }
            }
            recv(self.shutdown_rx) -> _ => {
                self.handle_shutdown_request();
                WorkerTimedWaitOutcome::Finished {
                    shutdown_requested: true,
                }
            }
            recv(self.render_bridge.render_release_receiver()) -> release_result => {
                self.render_bridge
                    .handle_release_wakeup(&mut self.session, release_result);
                self.drain_render_feedback();
                WorkerTimedWaitOutcome::ContinueWaiting
            }
            recv(self.render_bridge.render_acquire_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_acquire_sample_wakeup(&mut self.session, sample_result);
                self.drain_render_feedback();
                WorkerTimedWaitOutcome::ContinueWaiting
            }
            recv(self.render_bridge.render_timing_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_timing_sample_wakeup(&mut self.session, sample_result);
                self.drain_render_feedback();
                WorkerTimedWaitOutcome::ContinueWaiting
            }
            recv(self.render_bridge.resource_lock_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_resource_lock_sample_wakeup(&mut self.session, sample_result);
                self.drain_render_feedback();
                WorkerTimedWaitOutcome::ContinueWaiting
            }
            recv(self.render_bridge.resource_previous_frame_reuse_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_resource_previous_frame_reuse_sample_wakeup(&mut self.session, sample_result);
                self.drain_render_feedback();
                WorkerTimedWaitOutcome::ContinueWaiting
            }
            default(timeout) => {
                self.handle_worker_timeout(wakeup.deadline());
                WorkerTimedWaitOutcome::Finished {
                    shutdown_requested: false,
                }
            },
        }
    }

    /// Применяет typed activity outcome к worker-owned source state.
    fn handle_decoder_activity_wait_outcome(
        &mut self,
        source_id: DecoderActivitySourceId,
        activity_outcome: VideoDecoderActivityWaitOutcome,
    ) -> DecoderActivityWaitAction {
        match activity_outcome {
            VideoDecoderActivityWaitOutcome::ActivityReceived { epoch } => {
                self.decoder_activity
                    .mark_activity_observed(source_id, epoch);
                DecoderActivityWaitAction::RunPlaybackTick
            }
            VideoDecoderActivityWaitOutcome::NoNewActivityAfterEpoch { .. }
            | VideoDecoderActivityWaitOutcome::Timeout { .. } => {
                DecoderActivityWaitAction::ContinueWaiting
            }
            VideoDecoderActivityWaitOutcome::Unavailable { reason } => {
                self.decoder_activity
                    .disable_source_if_terminal(source_id, &reason);
                DecoderActivityWaitAction::ContinueWaiting
            }
        }
    }

    /// Считает оставшееся ожидание относительно уже выбранного абсолютного playback deadline-а.
    fn remaining_wakeup_timeout(wakeup: PlannedWorkerWakeup) -> Duration {
        match wakeup.deadline() {
            WorkerWakeupDeadline::Playback { deadline, .. } => {
                deadline.saturating_duration_since(Instant::now())
            }
        }
    }

    /// Блокируется без timeout, когда playback idle.
    fn wait_for_worker_wakeup_until_event(&mut self) -> bool {
        crossbeam_channel::select_biased! {
            recv(self.playback_intent_wake_rx) -> _ => {
                self.handle_playback_intent_wakeup()
            }
            recv(self.command_rx) -> command_result => {
                self.handle_command_wakeup(command_result)
            }
            recv(self.render_bridge.render_release_receiver()) -> release_result => {
                self.render_bridge
                    .handle_release_wakeup(&mut self.session, release_result);
                false
            }
            recv(self.render_bridge.render_acquire_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_acquire_sample_wakeup(&mut self.session, sample_result);
                false
            }
            recv(self.render_bridge.render_timing_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_timing_sample_wakeup(&mut self.session, sample_result);
                false
            }
            recv(self.render_bridge.resource_lock_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_resource_lock_sample_wakeup(&mut self.session, sample_result);
                false
            }
            recv(self.render_bridge.resource_previous_frame_reuse_sample_receiver()) -> sample_result => {
                self.render_bridge
                    .handle_resource_previous_frame_reuse_sample_wakeup(&mut self.session, sample_result);
                false
            }
            recv(self.shutdown_rx) -> _ => {
                self.handle_shutdown_request();
                true
            }
        }
    }

    /// Делегирует вычисление ближайшего самостоятельного wakeup-а чистому scheduler helper-у.
    #[cfg(test)]
    pub(super) fn plan_next_worker_wakeup(&self) -> Option<PlannedWorkerWakeup> {
        let decoder_activity_status = self.session.video_decoder_activity_status();

        self.plan_next_worker_wakeup_with_status(&decoder_activity_status)
    }

    /// Делегирует вычисление wakeup-а и attach-ит decoder activity только по intent flag-у.
    pub(super) fn plan_next_worker_wakeup_with_decoder_activity(
        &mut self,
    ) -> Option<PlannedWorkerWait> {
        let decoder_activity_status = self.session.video_decoder_activity_status();
        let decoder_activity = self
            .decoder_activity
            .wait_source_for_status(&decoder_activity_status);
        let wakeup = self.plan_next_worker_wakeup_with_status(&decoder_activity_status)?;
        let decoder_activity = match wakeup.deadline() {
            WorkerWakeupDeadline::Playback { plan, .. } if plan.wait_for_decoder_activity => {
                decoder_activity
            }
            WorkerWakeupDeadline::Playback { .. } => None,
        };

        Some(PlannedWorkerWait {
            wakeup,
            decoder_activity,
        })
    }

    /// Строит wakeup plan из уже снятого decoder activity status-а.
    fn plan_next_worker_wakeup_with_status(
        &self,
        decoder_activity_status: &VideoDecoderActivityStatus,
    ) -> Option<PlannedWorkerWakeup> {
        let now = Instant::now();
        self.worker_scheduler.next_worker_wakeup_deadline(
            now,
            &self.config.tick_config,
            self.config.decoder_readiness_poll_interval,
            self.config.coarse_wakeup_interval,
            |now, tick_config, decoder_readiness_poll_interval, coarse_wakeup_interval| {
                self.session
                    .worker_wakeup_plan_with_decoder_activity_status(
                        now,
                        tick_config,
                        decoder_readiness_poll_interval,
                        coarse_wakeup_interval,
                        decoder_activity_status,
                    )
            },
        )
    }

    /// Выполняет playback tick без ожидания, если media planner уже вернул due deadline.
    fn run_overdue_playback_tick(&mut self) {
        let Some(wakeup) = self.worker_scheduler.next_playback_wakeup_deadline(
            Instant::now(),
            &self.config.tick_config,
            self.config.decoder_readiness_poll_interval,
            self.config.coarse_wakeup_interval,
            |now, tick_config, decoder_readiness_poll_interval, coarse_wakeup_interval| {
                self.session.worker_wakeup_plan(
                    now,
                    tick_config,
                    decoder_readiness_poll_interval,
                    coarse_wakeup_interval,
                )
            },
        ) else {
            return;
        };

        if !wakeup.timeout().is_zero() {
            return;
        }

        let WorkerWakeupDeadline::Playback { plan, deadline } = wakeup.deadline();
        self.run_tick_for_wakeup_plan(plan, deadline);
    }
}
