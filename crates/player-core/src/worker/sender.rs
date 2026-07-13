use super::*;

impl PlayerCommandSender {
    /// Ставит strong staged media transaction без блокировки caller thread-а.
    ///
    /// Caller обязан заранее stage-ить exact Session 00C app half за переданным port-ом.
    pub fn stage_prepared_media_install(
        &self,
        request_id: MediaInstallRequestId,
        prepared_media: PreparedMedia,
        autoplay: bool,
        video_resource_port: MediaInstallVideoResourcePort,
    ) -> Result<MediaInstallReceipt, PlayerWorkerSendError> {
        let (receipt, install_port) = MediaInstallReceipt::new(request_id);
        self.command_tx
            .try_send(WorkerCommand::StagePreparedMediaInstall(Box::new(
                StagePreparedMediaInstallCommand {
                    request_id,
                    prepared_media,
                    autoplay,
                    install_port,
                    video_resource_port,
                },
            )))
            .map_err(PlayerWorkerSendError::from)?;
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

    /// Общий bounded request/reply boundary control-команд install transaction-а.
    fn send_media_install_control(
        &self,
        control: MediaInstallControl,
        request_id: MediaInstallRequestId,
    ) -> Result<MediaInstallControlReceipt, PlayerWorkerSendError> {
        let (receipt, outcome_tx) = MediaInstallControlReceipt::new(request_id);
        self.command_tx
            .try_send(WorkerCommand::MediaInstallControl(
                MediaInstallControlCommand {
                    control,
                    outcome_tx,
                },
            ))
            .map_err(PlayerWorkerSendError::from)?;
        Ok(receipt)
    }

    /// Ставит compatibility media install без блокировки caller thread-а.
    ///
    /// Успешный return означает только command acceptance. Фактический lifecycle outcome
    /// приходит через отдельный request-owned receipt и не теряется при event backpressure.
    pub fn load_prepared_media_compatibility_with_receipt(
        &self,
        request_id: MediaInstallRequestId,
        prepared_media: PreparedMedia,
        autoplay: bool,
    ) -> Result<MediaInstallReceipt, PlayerWorkerSendError> {
        let (receipt, install_port) = MediaInstallReceipt::new(request_id);
        self.command_tx
            .try_send(WorkerCommand::LoadPreparedMedia {
                request_id,
                prepared_media,
                autoplay,
                install_port,
            })
            .map_err(PlayerWorkerSendError::from)?;
        Ok(receipt)
    }

    /// Отправляет команду без блокировки render/UI thread.
    pub fn try_send(&self, command: PlayerCommand) -> Result<(), PlayerWorkerSendError> {
        self.command_tx
            .try_send(WorkerCommand::Player(command))
            .map_err(PlayerWorkerSendError::from)
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
