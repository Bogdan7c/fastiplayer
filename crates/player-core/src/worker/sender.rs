use super::*;

impl PlayerCommandSender {
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
