use super::*;

impl PlayerWorker {
    /// Запускает worker thread и сразу публикует empty snapshot.
    pub fn spawn(config: PlayerWorkerConfig) -> PlayerResult<Self> {
        validate_runtime_default_volume(config.default_volume).map_err(|message| {
            PlayerError::new(
                PlayerErrorKind::InvalidCommand,
                format!("invalid player worker default volume: {message}"),
            )
        })?;

        let (command_tx, command_rx) = bounded(COMMAND_CHANNEL_CAPACITY);
        let (snapshot_tx, snapshot_rx) = bounded(SNAPSHOT_CHANNEL_CAPACITY);
        let (event_tx, event_rx) = bounded(EVENT_CHANNEL_CAPACITY);
        let (render_bridge, render_bridge_client) = RenderLeaseBridge::new();
        let (shutdown_tx, shutdown_rx) = bounded(1);
        let decoder_thread_config = config.decoder_thread_config;
        let audio_decoder_factory = Arc::clone(&config.audio_decoder_factory);
        let audio_output_factory = Arc::clone(&config.audio_output_factory);
        let timeline_hover_prepare_handoff = config.timeline_hover_prepare_handoff.clone();
        let frame_server_config = config.frame_server_config;

        let command_sender = PlayerCommandSender { command_tx };
        let snapshot_rx_for_worker = snapshot_rx.clone();

        let worker_started_at = Instant::now();
        let join_handle = thread::Builder::new()
            .name("player-worker".into())
            .spawn(move || {
                let mut session =
                    PlayerSession::with_audio_factories_and_timeline_hover_prepare_handoff(
                        audio_decoder_factory,
                        audio_output_factory,
                        timeline_hover_prepare_handoff,
                        frame_server_config,
                    );
                if let Err(error) =
                    session.dispatch_command(PlayerCommand::SetVolume(config.default_volume))
                {
                    warn!(error = %error, "Не удалось применить worker default volume при старте");
                    session.mark_fatal_error(error);
                }

                let runtime = PlayerWorkerRuntime {
                    session,
                    worker_scheduler: WorkerScheduler,
                    decoder_activity: WorkerDecoderActivityState::default(),
                    command_rx,
                    snapshot_publisher: LatestSnapshotPublisher::new(
                        snapshot_tx,
                        snapshot_rx_for_worker,
                    ),
                    event_tx,
                    render_bridge,
                    shutdown_rx,
                    config,
                    last_tick_at: worker_started_at,
                    last_diagnostics_summary_at: worker_started_at,
                    last_seek_stall_log_key: None,
                    last_seek_stall_log_at: None,
                };
                runtime.run();
            })
            .map_err(|error| {
                PlayerError::new(
                    PlayerErrorKind::RuntimeError,
                    format!("failed to spawn player worker: {error}"),
                )
            })?;

        Ok(Self {
            command_sender,
            snapshot_rx,
            cached_snapshot: PlayerSnapshot::empty(),
            event_rx,
            render_bridge_client,
            decoder_thread_config,
            shutdown_tx,
            join_handle: Some(join_handle),
        })
    }

    /// Возвращает cloneable sender для long-lived UI callbacks.
    #[must_use]
    pub fn command_sender(&self) -> PlayerCommandSender {
        self.command_sender.clone()
    }

    /// Возвращает decoder-thread limits, которые shell использует при сборке backend factory.
    #[must_use]
    pub const fn decoder_thread_config(&self) -> PlayerVideoDecoderThreadConfig {
        self.decoder_thread_config
    }

    /// Отправляет обычную player command без блокировки.
    pub fn try_send_command(&self, command: PlayerCommand) -> Result<(), PlayerWorkerSendError> {
        self.command_sender.try_send(command)
    }

    /// Применяет committed runtime settings через request/reply worker boundary.
    pub fn apply_runtime_settings(
        &self,
        update: PlayerRuntimeSettingsUpdate,
    ) -> PlayerRuntimeApplyResult {
        self.command_sender.apply_runtime_settings(update)
    }

    /// Передаёт уже подготовленный media во владение worker thread.
    pub fn load_prepared_media(
        &self,
        prepared_media: PreparedMedia,
        autoplay: bool,
    ) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::LoadPreparedMedia {
                prepared_media,
                autoplay,
            })
            .map_err(PlayerWorkerSendError::from)
    }

    /// Передаёт уже открытый streaming demuxer во владение worker thread.
    pub fn load_demuxer(
        &self,
        label: String,
        demuxer: Box<dyn media_core::Demuxer + Send>,
        autoplay: bool,
    ) -> Result<(), PlayerWorkerSendError> {
        let prepared_media = PreparedMedia::from_external_label(label, demuxer);
        self.load_prepared_media(prepared_media, autoplay)
    }

    /// Публикует ошибку adapter-а, который не смог подготовить media.
    pub fn fail_media_open(
        &self,
        request: MediaOpenRequest,
        error: PlayerError,
    ) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::MediaOpenFailed { request, error })
            .map_err(PlayerWorkerSendError::from)
    }

    /// Устанавливает video decoder backend, уже запущенный shell composition root-ом.
    pub fn set_video_backend(
        &self,
        started_backend: StartedVideoBackend,
    ) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::SetVideoBackend { started_backend })
            .map_err(PlayerWorkerSendError::from)
    }

    /// Сообщает worker-у, что shell не нашёл совместимый backend для отложенного видео.
    pub fn reject_pending_video_backend(
        &self,
        reason: String,
    ) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::RejectPendingVideoBackend { reason })
            .map_err(PlayerWorkerSendError::from)
    }

    /// Передаёт capability report из shell/backend layer в worker.
    pub fn set_system_capabilities(
        &self,
        capabilities: SystemCapabilities,
    ) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::SetSystemCapabilities(capabilities))
            .map_err(PlayerWorkerSendError::from)
    }

    /// Передаёт fatal render error в player state machine.
    pub fn mark_fatal_error(&self, error: PlayerError) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::MarkFatalError(error))
            .map_err(PlayerWorkerSendError::from)
    }

    /// Передаёт typed render bridge error в worker-owned player session.
    pub fn report_render_error(
        &self,
        error: PlayerRenderError,
    ) -> Result<(), PlayerWorkerSendError> {
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::RenderError(error))
            .map_err(PlayerWorkerSendError::from)
    }

    /// Возвращает последний snapshot, не блокируя UI.
    #[must_use]
    pub fn latest_snapshot(&mut self, frame_counters: FrameCounters) -> PlayerSnapshot {
        for snapshot in self.snapshot_rx.try_iter() {
            self.cached_snapshot = snapshot;
        }

        let mut snapshot = self.cached_snapshot.clone();
        snapshot.frame_counters = frame_counters;
        snapshot
    }

    /// Забирает накопленные worker events без блокировки.
    #[must_use]
    pub fn drain_events(&self) -> Vec<PlayerWorkerEvent> {
        self.event_rx.try_iter().collect()
    }

    /// Пытается получить текущий кадр для renderer-а без раскрытия `PlayerSession`.
    #[must_use]
    pub fn try_acquire_present_frame(&self) -> Option<VideoFrameLease> {
        self.render_bridge_client.try_acquire_present_frame()
    }

    /// Пытается получить scrub visual override frame без доступа к `PlayerSession`.
    #[must_use]
    pub fn try_acquire_scrub_visual_override_frame(&self) -> Option<VideoFrameLease> {
        self.render_bridge_client
            .try_acquire_scrub_visual_override_frame()
    }

    /// Сообщает worker-у renderer submit/present timing без блокировки render thread.
    pub fn report_gpu_submit_present_latency(&self, submit_present_elapsed: Duration) {
        self.render_bridge_client
            .report_gpu_submit_present_latency(submit_present_elapsed);
    }

    /// Сообщает worker-у, что renderer повторил previous valid frame из-за busy texture lock-а.
    pub fn report_render_resource_previous_frame_reuse(&self) {
        self.render_bridge_client
            .report_resource_previous_frame_reuse();
    }

    /// Запрашивает shutdown и ждёт завершения worker thread.
    pub fn shutdown(&mut self) -> Result<(), PlayerWorkerJoinError> {
        let _ = self.try_send_command(PlayerCommand::Shutdown);
        let _ = self.shutdown_tx.try_send(());

        let Some(join_handle) = self.join_handle.take() else {
            return Ok(());
        };

        join_handle.join().map_err(|_| PlayerWorkerJoinError)
    }
}

impl Drop for PlayerWorker {
    /// Drop path не должен оставлять фоновые player threads.
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            warn!(error = %error, "Player worker shutdown failed during drop");
        }
    }
}
