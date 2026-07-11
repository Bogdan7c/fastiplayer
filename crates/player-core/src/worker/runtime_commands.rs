use super::*;

impl PlayerWorkerRuntime {
    /// Обрабатывает wakeup от основной очереди команд.
    pub(super) fn handle_command_wakeup(
        &mut self,
        command_result: Result<WorkerCommand, crossbeam_channel::RecvError>,
    ) -> bool {
        match command_result {
            Ok(command) => {
                self.handle_worker_command(command);
                self.publish_session_outputs();
                self.session.is_shutdown_requested()
            }
            Err(_) => {
                self.handle_shutdown_request();
                true
            }
        }
    }

    /// Забирает команду без блокировки, чтобы render/tick не starvation-ились.
    pub(super) fn receive_next_command(&self) -> Option<WorkerCommand> {
        match self.command_rx.try_recv() {
            Ok(command) => Some(command),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(WorkerCommand::Player(PlayerCommand::Shutdown)),
        }
    }

    /// Обрабатывает одну worker command.
    pub(super) fn handle_worker_command(&mut self, command: WorkerCommand) {
        match command {
            WorkerCommand::Player(player_command) => self.handle_player_command(player_command),
            WorkerCommand::LoadPreparedMedia {
                prepared_media,
                autoplay,
            } => {
                self.session
                    .load_prepared_media_with_autoplay(prepared_media, autoplay);
            }
            WorkerCommand::MediaOpenFailed { request, error } => {
                self.session.fail_media_open_with_error(request, error);
            }
            WorkerCommand::SetVideoBackend {
                started_backend,
                intent,
                accepted_decoder_config,
                response_tx,
            } => {
                let result = self
                    .session
                    .install_video_backend_with_intent(started_backend, intent);
                if result.is_ok()
                    && let Some(decoder_config) = accepted_decoder_config
                {
                    self.config.decoder_thread_config = decoder_config;
                }
                if response_tx.send(result).is_err() {
                    warn!("Video backend commit receiver was dropped");
                }
            }
            WorkerCommand::RejectPendingVideoBackend { reason } => {
                self.session
                    .reject_pending_video_backend_with_reason(reason);
            }
            WorkerCommand::SetSystemCapabilities(capabilities) => {
                self.session.set_system_capabilities(capabilities);
            }
            WorkerCommand::MarkFatalError(error) => {
                self.session.mark_fatal_error(error);
            }
            WorkerCommand::RenderError(error) => {
                self.handle_render_error(error);
            }
            WorkerCommand::CheckRuntimeReconfigureBoundary { response_tx } => {
                let activity = self.session.runtime_reconfigure_boundary_activity();
                if response_tx.send(activity).is_err() {
                    warn!("Runtime reconfigure preflight receiver was dropped");
                }
            }
            WorkerCommand::ApplyRuntimeSettings {
                update,
                response_tx,
            } => {
                let report = self.apply_runtime_settings(*update);
                if response_tx.send(report).is_err() {
                    warn!("Settings runtime apply report receiver was dropped");
                }
            }
        }
    }

    /// Применяет typed runtime settings, которыми владеет worker.
    pub(super) fn apply_runtime_settings(
        &mut self,
        update: PlayerRuntimeSettingsUpdate,
    ) -> PlayerRuntimeApplyReport {
        let mut report = PlayerRuntimeApplyReport::empty();

        if update.is_empty() {
            report.push(PlayerRuntimeApplyGroupReport::invalid(
                PlayerRuntimeApplyGroup::Request,
                std::iter::empty(),
                "player runtime settings update is empty",
            ));
            return report;
        }

        if update.requires_pipeline_lifecycle_boundary()
            && let Some(activity) = self.session.runtime_reconfigure_boundary_activity()
        {
            report.push(PlayerRuntimeApplyGroupReport::runtime_busy(
                PlayerRuntimeApplyGroup::Request,
                update.affected_settings(),
                activity,
                "player pipeline boundary is busy; settings update was not queued or applied",
            ));
            return report;
        }

        if let Some(tick_update) = update.tick_config {
            self.apply_runtime_tick_config(tick_update, &mut report);
        }

        if let Some(default_volume_update) = update.default_volume {
            self.apply_runtime_default_volume(default_volume_update, &mut report);
        }

        if let Some(audio_output_update) = update.audio_output_recreate {
            self.apply_runtime_audio_output_recreate(audio_output_update, &mut report);
        }

        if let Some(decoder_thread_update) = update.decoder_thread_config {
            self.apply_runtime_decoder_thread_config(decoder_thread_update, &mut report);
        }

        if let Some(video_backend_update) = update.video_backend {
            self.apply_runtime_video_backend(video_backend_update, &mut report);
        }

        if let Some(frame_server_policy_update) = update.frame_server_policy {
            self.apply_runtime_frame_server_policy(frame_server_policy_update, &mut report);
        }

        if !update.unsupported_settings.is_empty() {
            report.push(PlayerRuntimeApplyGroupReport::unsupported(
                PlayerRuntimeApplyGroup::UnsupportedSettings,
                update.unsupported_settings,
                "player-core has no runtime apply boundary for these settings yet",
            ));
        }

        report
    }

    /// In-place применяет только worker-owned tick config.
    fn apply_runtime_tick_config(
        &mut self,
        update: PlayerRuntimeTickConfigUpdate,
        report: &mut PlayerRuntimeApplyReport,
    ) {
        if let Err(message) = validate_runtime_tick_config(&update.tick_config) {
            report.push(PlayerRuntimeApplyGroupReport::invalid(
                PlayerRuntimeApplyGroup::TickConfig,
                update.affected_settings,
                message,
            ));
            return;
        }

        let change = if self.config.tick_config == update.tick_config {
            PlayerRuntimeAcceptedChange::Unchanged
        } else {
            self.config.tick_config = update.tick_config;
            PlayerRuntimeAcceptedChange::Applied
        };

        report.push(PlayerRuntimeApplyGroupReport::accepted(
            PlayerRuntimeApplyGroup::TickConfig,
            update.affected_settings,
            change,
            "player worker tick config updated in-place",
        ));
    }

    /// Обновляет default-volume policy без изменения текущей громкости session.
    fn apply_runtime_default_volume(
        &mut self,
        update: PlayerRuntimeDefaultVolumeUpdate,
        report: &mut PlayerRuntimeApplyReport,
    ) {
        if let Err(message) = validate_runtime_default_volume(update.default_volume) {
            report.push(PlayerRuntimeApplyGroupReport::invalid(
                PlayerRuntimeApplyGroup::DefaultVolume,
                update.affected_settings,
                message,
            ));
            return;
        }

        let change = if (self.config.default_volume - update.default_volume).abs() <= f32::EPSILON {
            PlayerRuntimeAcceptedChange::Unchanged
        } else {
            self.config.default_volume = update.default_volume;
            PlayerRuntimeAcceptedChange::Applied
        };

        report.push(PlayerRuntimeApplyGroupReport::accepted(
            PlayerRuntimeApplyGroup::DefaultVolume,
            update.affected_settings,
            change,
            "player default volume policy updated; current playback volume is unchanged",
        ));
    }

    /// Пересоздаёт active audio output после app-owned device policy commit-а.
    fn apply_runtime_audio_output_recreate(
        &mut self,
        update: PlayerRuntimeAudioOutputRecreateUpdate,
        report: &mut PlayerRuntimeApplyReport,
    ) {
        match self.session.recreate_active_audio_output() {
            Ok(change) => report.push(PlayerRuntimeApplyGroupReport::accepted(
                PlayerRuntimeApplyGroup::AudioOutput,
                update.affected_settings,
                change,
                "audio output device policy applied and active output recreated",
            )),
            Err(error) => report.push(PlayerRuntimeApplyGroupReport::fatal(
                PlayerRuntimeApplyGroup::AudioOutput,
                update.affected_settings,
                format!("audio output recreation failed before owner commit: {error}"),
            )),
        }
    }

    /// Принимает новый decoder-thread config после app-owned backend rebuild.
    fn apply_runtime_decoder_thread_config(
        &mut self,
        update: PlayerRuntimeDecoderThreadConfigUpdate,
        report: &mut PlayerRuntimeApplyReport,
    ) {
        if self.config.decoder_thread_config == update.decoder_thread_config {
            report.push(PlayerRuntimeApplyGroupReport::accepted(
                PlayerRuntimeApplyGroup::DecoderThreadConfig,
                update.affected_settings,
                PlayerRuntimeAcceptedChange::Unchanged,
                "decoder thread config already matches requested settings",
            ));
            return;
        }

        self.config.decoder_thread_config = update.decoder_thread_config;
        report.push(PlayerRuntimeApplyGroupReport::accepted(
            PlayerRuntimeApplyGroup::DecoderThreadConfig,
            update.affected_settings,
            PlayerRuntimeAcceptedChange::Applied,
            "decoder thread config accepted after controlled backend rebuild",
        ));
    }

    /// Подтверждает backend preference change; реальный rebuild делает app composition layer.
    fn apply_runtime_video_backend(
        &mut self,
        update: PlayerRuntimeVideoBackendUpdate,
        report: &mut PlayerRuntimeApplyReport,
    ) {
        report.push(PlayerRuntimeApplyGroupReport::accepted(
            PlayerRuntimeApplyGroup::VideoBackend,
            update.affected_settings,
            PlayerRuntimeAcceptedChange::Applied,
            "video backend preference applied via app-owned pipeline rebuild",
        ));
    }

    /// Обновляет session-owned frame-server policy без перезапуска live scrub work.
    fn apply_runtime_frame_server_policy(
        &mut self,
        update: PlayerRuntimeFrameServerPolicyUpdate,
        report: &mut PlayerRuntimeApplyReport,
    ) {
        let change = if self.config.frame_server_config == update.frame_server_config {
            PlayerRuntimeAcceptedChange::Unchanged
        } else {
            self.config.frame_server_config = update.frame_server_config;
            self.session
                .apply_frame_server_policy_config(update.frame_server_config);
            PlayerRuntimeAcceptedChange::Applied
        };

        report.push(PlayerRuntimeApplyGroupReport::accepted(
            PlayerRuntimeApplyGroup::FrameServerPolicy,
            update.affected_settings,
            change,
            "frame-server player policy updated in-place",
        ));
    }

    /// Сохраняет typed render error в snapshot и публикует worker event.
    fn handle_render_error(&mut self, error: PlayerRenderError) {
        self.publish_worker_event(PlayerWorkerEvent::RenderError(error.clone()));
        self.session.mark_fatal_error(error.to_player_error());
    }

    /// Применяет public player command с сохранением worker-owned load/shutdown boundary.
    fn handle_player_command(&mut self, command: PlayerCommand) {
        match command {
            PlayerCommand::OpenMedia(request) => self.handle_open_media_request(request),
            PlayerCommand::Stop => self.handle_stop_command(),
            PlayerCommand::Shutdown => self.handle_shutdown_request(),
            other_command => self.dispatch_player_command(other_command),
        }
    }

    /// Открывает media request без знания concrete container adapter-а.
    fn handle_open_media_request(&mut self, request: MediaOpenRequest) {
        match request.source.clone() {
            MediaSource::LocalFile(_) => {
                self.session.fail_media_open_with_error(
                    request,
                    PlayerError::new(
                        PlayerErrorKind::DemuxError,
                        "Локальный файл должен быть подготовлен adapter-слоем до player-core",
                    ),
                );
            }
            MediaSource::Url(_) | MediaSource::ExternalLabel(_) => {
                self.dispatch_player_command(PlayerCommand::OpenMedia(request));
            }
        }
    }

    /// Stop закрывает текущий media через обычный public command без worker-side seek-а.
    fn handle_stop_command(&mut self) {
        self.dispatch_player_command(PlayerCommand::Stop);
    }

    /// Shutdown закрывает session через обычный public command.
    pub(super) fn handle_shutdown_request(&mut self) {
        self.dispatch_player_command(PlayerCommand::Shutdown);
    }

    /// Безопасно вызывает `PlayerSession::dispatch_command`, не смешивая reject и fatal error.
    fn dispatch_player_command(&mut self, command: PlayerCommand) {
        match self.session.dispatch_command(command) {
            Ok(PlayerCommandOutcome::Applied) => {}
            Ok(PlayerCommandOutcome::Rejected(rejection)) => {
                debug!(rejection = ?rejection, "Player worker command rejected non-fatally");
            }
            Err(error) => {
                warn!(error = %error, "Player worker command failed");
                self.session.mark_fatal_error(error);
            }
        }
    }
}
