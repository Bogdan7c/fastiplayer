use super::*;

impl PlayerCommandSender {
    /// Проверяет общий terminal gate до любой новой owner mutation/admission.
    fn ensure_admission_open(&self) -> Result<(), PlayerWorkerSendError> {
        if self.admission_closed.load(Ordering::Acquire) {
            return Err(PlayerWorkerSendError::Disconnected);
        }
        Ok(())
    }

    /// Единая неблокирующая отправка ordinary worker command с terminal admission gate.
    pub(super) fn try_send_worker_command(
        &self,
        command: WorkerCommand,
    ) -> Result<(), PlayerWorkerSendError> {
        self.ensure_admission_open()?;
        self.command_tx
            .try_send(command)
            .map_err(PlayerWorkerSendError::from)
    }

    /// Ставит exact-instance transport и возвращает receipt фактического owner apply.
    pub fn exact_media_transport(
        &self,
        request: ExactMediaTransportRequest,
    ) -> Result<ExactMediaTransportReceipt, PlayerWorkerSendError> {
        // Capacity-one reply хранит ровно один authoritative outcome без polling/FIFO.
        let (outcome_tx, outcome_rx) = bounded(1);
        self.try_send_worker_command(WorkerCommand::ExactMediaTransport {
            request,
            outcome_tx,
        })?;
        Ok(ExactMediaTransportReceipt::new(
            request.media_instance_id,
            outcome_rx,
        ))
    }

    /// Ставит exact-instance restore и возвращает receipt, а не ложный success по enqueue.
    pub fn restore_installed_media_state(
        &self,
        restore: InstalledMediaStateRestore,
    ) -> Result<InstalledMediaStateRestoreReceipt, PlayerWorkerSendError> {
        let request_id = restore.request_id;
        let (outcome_tx, outcome_rx) = bounded(1);
        self.try_send_worker_command(WorkerCommand::RestoreInstalledMediaState {
            restore,
            outcome_tx,
        })?;
        Ok(InstalledMediaStateRestoreReceipt::new(
            request_id, outcome_rx,
        ))
    }

    /// Ставит strong staged media transaction без блокировки caller thread-а.
    ///
    /// Caller обязан заранее stage-ить exact Session 00C app half за переданным port-ом.
    /// Reversible registration закрывает race, где быстрый owner успевает завершить preparation
    /// до возврата try_send; transport Full/Disconnected восстанавливает прежний staged slot.
    pub fn stage_prepared_media_install(
        &self,
        request_id: MediaInstallRequestId,
        prepared_media: PreparedMedia,
        initial_intent: PlaybackIntent,
        initial_intent_revision: PlaybackIntentRevision,
        video_resource_port: MediaInstallVideoResourcePort,
    ) -> Result<MediaInstallReceipt, PlayerWorkerSendError> {
        let (receipt, install_port) = MediaInstallReceipt::new(request_id);
        let registration = self.playback_intent_control.begin_staged_registration(
            request_id,
            AcceptedPlaybackIntent {
                revision: initial_intent_revision,
                intent: initial_intent,
            },
        );
        let send_result = self.try_send_worker_command(WorkerCommand::StagePreparedMediaInstall(
            Box::new(StagePreparedMediaInstallCommand {
                request_id,
                prepared_media,
                initial_intent,
                initial_intent_revision,
                install_port,
                video_resource_port,
            }),
        ));
        if let Err(error) = send_result {
            self.playback_intent_control
                .rollback_staged_registration(registration);
            return Err(error);
        }
        Ok(receipt)
    }

    /// Доставляет exact authorization и возвращает receipt фактического owner outcome.
    pub fn authorize_install_commit(
        &self,
        authorization: AuthorizeInstallCommit,
    ) -> Result<MediaInstallControlReceipt, PlayerWorkerSendError> {
        self.send_media_install_control(
            MediaInstallControl::Authorize(authorization),
            authorization.request_id,
        )
    }

    /// Доставляет exact pre-barrier cancellation в тот же ordered worker stream.
    pub fn cancel_media_install(
        &self,
        cancellation: CancelMediaInstall,
    ) -> Result<MediaInstallControlReceipt, PlayerWorkerSendError> {
        self.send_media_install_control(
            MediaInstallControl::Cancel(cancellation),
            cancellation.request_id,
        )
    }

    /// Lossless ставит exact pre-barrier cancel после уже доказанного dispatch rejection-а.
    ///
    /// Этот cleanup path использует тот же ordered owner stream и отличается только тем, что
    /// ждёт свободное место в bounded channel вместо повторного `try_send`/polling-а.
    pub fn cancel_media_install_lossless(
        &self,
        cancellation: CancelMediaInstall,
    ) -> Result<MediaInstallControlReceipt, PlayerWorkerSendError> {
        self.send_media_install_control_lossless(
            MediaInstallControl::Cancel(cancellation),
            cancellation.request_id,
        )
    }

    /// Общий bounded request/reply boundary control-команд install transaction-а.
    fn send_media_install_control(
        &self,
        control: MediaInstallControl,
        request_id: MediaInstallRequestId,
    ) -> Result<MediaInstallControlReceipt, PlayerWorkerSendError> {
        let (receipt, outcome_tx) = MediaInstallControlReceipt::new(request_id);
        self.try_send_worker_command(WorkerCommand::MediaInstallControl(
            MediaInstallControlCommand {
                control,
                outcome_tx,
            },
        ))?;
        Ok(receipt)
    }

    /// Доставляет cleanup control без потери на временном backpressure.
    fn send_media_install_control_lossless(
        &self,
        control: MediaInstallControl,
        request_id: MediaInstallRequestId,
    ) -> Result<MediaInstallControlReceipt, PlayerWorkerSendError> {
        let (receipt, outcome_tx) = MediaInstallControlReceipt::new(request_id);
        self.ensure_admission_open()?;
        self.command_tx
            .send(WorkerCommand::MediaInstallControl(
                MediaInstallControlCommand {
                    control,
                    outcome_tx,
                },
            ))
            .map_err(|_| PlayerWorkerSendError::Disconnected)?;
        Ok(receipt)
    }

    /// Ставит compatibility media install без блокировки caller thread-а.
    ///
    /// Успешный return означает только command acceptance. Фактический lifecycle outcome
    /// приходит через отдельный request-owned receipt и не теряется при event backpressure.
    pub(super) fn load_prepared_media_compatibility(
        &self,
        request_id: MediaInstallRequestId,
        prepared_media: PreparedMedia,
        autoplay: bool,
    ) -> Result<MediaInstallReceipt, PlayerWorkerSendError> {
        let (receipt, install_port) = MediaInstallReceipt::new(request_id);
        self.try_send_worker_command(WorkerCommand::LoadPreparedMedia {
            request_id,
            prepared_media,
            autoplay,
            install_port,
        })?;
        Ok(receipt)
    }

    /// Линеаризует D52 update независимо от capacity ordinary worker queue.
    ///
    /// Capacity-one wake может быть full только когда owner уже гарантированно проснётся;
    /// payload при этом не теряется, потому что хранится в shared latest-only slot-е.
    pub fn update_playback_intent(
        &self,
        update: PlaybackIntentUpdate,
    ) -> Result<PlaybackIntentUpdateReceipt, PlayerWorkerSendError> {
        self.ensure_admission_open()?;
        let submitted = self.playback_intent_control.submit_update(update);
        if submitted.wake_player_owner {
            match self.playback_intent_wake_tx.try_send(()) {
                Ok(()) | Err(TrySendError::Full(())) => {}
                Err(TrySendError::Disconnected(())) => {
                    return Err(PlayerWorkerSendError::Disconnected);
                }
            }
        }
        Ok(submitted.receipt)
    }

    /// Собирает sender fixture с отдельным D52 wake channel.
    #[cfg(test)]
    pub(super) fn for_tests(command_tx: Sender<WorkerCommand>) -> (Self, Receiver<()>) {
        let playback_intent_control = Arc::new(PlaybackIntentControl::default());
        let (playback_intent_wake_tx, playback_intent_wake_rx) = bounded(1);
        let admission_closed = Arc::new(AtomicBool::new(false));
        (
            Self {
                command_tx,
                playback_intent_control,
                playback_intent_wake_tx,
                admission_closed,
            },
            playback_intent_wake_rx,
        )
    }

    /// Отправляет команду без блокировки render/UI thread.
    pub fn try_send(&self, command: PlayerCommand) -> Result<(), PlayerWorkerSendError> {
        self.try_send_worker_command(WorkerCommand::Player(command))
    }

    /// Применяет committed runtime settings и ждёт реальный worker report.
    ///
    /// Это settings-specific API: caller получает результат применения, а не
    /// только факт, что команда поместилась в bounded очередь worker-а.
    pub fn apply_runtime_settings(
        &self,
        update: PlayerRuntimeSettingsUpdate,
    ) -> PlayerRuntimeApplyResult {
        let (response_tx, response_rx) = bounded(1);

        if self.ensure_admission_open().is_err() {
            return Err(PlayerRuntimeApplyError::Disconnected);
        }

        match self
            .command_tx
            .try_send(WorkerCommand::ApplyRuntimeSettings {
                update: Box::new(update),
                response_tx,
            }) {
            Ok(()) => {}
            Err(TrySendError::Full(_command)) => return Err(PlayerRuntimeApplyError::Backpressure),
            Err(TrySendError::Disconnected(_command)) => {
                return Err(PlayerRuntimeApplyError::Disconnected);
            }
        }

        response_rx
            .recv()
            .map_err(|_error| PlayerRuntimeApplyError::Disconnected)
    }
}
