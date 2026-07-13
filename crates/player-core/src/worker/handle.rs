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
        let playback_intent_control = Arc::new(PlaybackIntentControl::default());
        let (playback_intent_wake_tx, playback_intent_wake_rx) = bounded(1);
        let (snapshot_tx, snapshot_rx) = bounded(SNAPSHOT_CHANNEL_CAPACITY);
        let (event_tx, event_rx) = bounded(EVENT_CHANNEL_CAPACITY);
        let (render_bridge, render_bridge_client) = RenderLeaseBridge::new();
        let (shutdown_tx, shutdown_rx) = bounded(1);
        let decoder_thread_config = config.decoder_thread_config;
        let audio_decoder_factory = Arc::clone(&config.audio_decoder_factory);
        let audio_output_factory = Arc::clone(&config.audio_output_factory);
        let audio_tempo_processor_factory = Arc::clone(&config.audio_tempo_processor_factory);
        let frame_server_config = config.frame_server_config;

        let command_sender = PlayerCommandSender {
            command_tx,
            playback_intent_control: Arc::clone(&playback_intent_control),
            playback_intent_wake_tx: playback_intent_wake_tx.clone(),
        };
        let snapshot_rx_for_worker = snapshot_rx.clone();

        let worker_started_at = Instant::now();
        let join_handle = thread::Builder::new()
            .name("player-worker".into())
            .spawn(move || {
                let mut session = PlayerSession::with_audio_factories(
                    audio_decoder_factory,
                    audio_output_factory,
                )
                .with_audio_tempo_processor_factory(audio_tempo_processor_factory)
                .with_playback_intent_control(Arc::clone(&playback_intent_control));
                session.apply_frame_server_policy_config(frame_server_config);
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
                    playback_intent_control,
                    playback_intent_wake_rx,
                    _playback_intent_wake_tx_guard: playback_intent_wake_tx,
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

    /// Проверяет exclusive pipeline boundary без mutation, reservation или hidden queue.
    pub fn runtime_reconfigure_boundary_activity(
        &self,
    ) -> Result<Option<PlayerRuntimeBoundaryActivity>, PlayerRuntimeApplyError> {
        let (response_tx, response_rx) = bounded(1);
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::CheckRuntimeReconfigureBoundary { response_tx })
            .map_err(|error| match error {
                TrySendError::Full(_) => PlayerRuntimeApplyError::Backpressure,
                TrySendError::Disconnected(_) => PlayerRuntimeApplyError::Disconnected,
            })?;

        response_rx
            .recv()
            .map_err(|_| PlayerRuntimeApplyError::Disconnected)
    }

    /// Единственный временный app compatibility facade до Session 10D.
    ///
    /// Facade возвращает typed completion receipt и внутри вызывает тот же strong player
    /// install algorithm. Caller обязан различать command acceptance и фактический terminal.
    pub fn load_prepared_media(
        &self,
        prepared_media: PreparedMedia,
        autoplay: bool,
    ) -> Result<MediaInstallReceipt, PlayerWorkerSendError> {
        self.command_sender.load_prepared_media_compatibility(
            MediaInstallRequestId::new_unique(),
            prepared_media,
            autoplay,
        )
    }

    /// Ставит strong staged media transaction поверх exact Session 00C resource port-а.
    pub fn stage_prepared_media_install(
        &self,
        request_id: MediaInstallRequestId,
        prepared_media: PreparedMedia,
        initial_intent: PlaybackIntent,
        initial_intent_revision: PlaybackIntentRevision,
        video_resource_port: MediaInstallVideoResourcePort,
    ) -> Result<MediaInstallReceipt, PlayerWorkerSendError> {
        self.command_sender.stage_prepared_media_install(
            request_id,
            prepared_media,
            initial_intent,
            initial_intent_revision,
            video_resource_port,
        )
    }

    /// Обновляет staged либо exact just-installed playback intent через отдельный D52 path.
    pub fn update_playback_intent(
        &self,
        update: PlaybackIntentUpdate,
    ) -> Result<PlaybackIntentUpdateReceipt, PlayerWorkerSendError> {
        self.command_sender.update_playback_intent(update)
    }

    /// Доставляет authorization без превращения queue acceptance в install outcome.
    pub fn authorize_install_commit(
        &self,
        authorization: AuthorizeInstallCommit,
    ) -> Result<MediaInstallControlReceipt, PlayerWorkerSendError> {
        self.command_sender.authorize_install_commit(authorization)
    }

    /// Доставляет exact typed cancellation до commit barrier.
    pub fn cancel_media_install(
        &self,
        cancellation: CancelMediaInstall,
    ) -> Result<MediaInstallControlReceipt, PlayerWorkerSendError> {
        self.command_sender.cancel_media_install(cancellation)
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

    /// Устанавливает video backend и ждёт фактический worker-side commit/rollback.
    pub fn set_video_backend(
        &self,
        started_backend: StartedVideoBackend,
        intent: PlayerVideoBackendInstallIntent,
        accepted_decoder_config: Option<PlayerVideoDecoderThreadConfig>,
    ) -> Result<(), PlayerRuntimeApplyError> {
        let (response_tx, response_rx) = bounded(1);
        self.command_sender
            .command_tx
            .try_send(WorkerCommand::SetVideoBackend {
                started_backend,
                intent,
                accepted_decoder_config,
                response_tx,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => PlayerRuntimeApplyError::Backpressure,
                TrySendError::Disconnected(_) => PlayerRuntimeApplyError::Disconnected,
            })?;

        response_rx
            .recv()
            .map_err(|_| PlayerRuntimeApplyError::Disconnected)?
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
