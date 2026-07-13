use super::*;

impl PlayerCommandSender {
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
