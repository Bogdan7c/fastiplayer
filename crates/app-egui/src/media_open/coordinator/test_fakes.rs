//! Test-only запуск fake-подготовки и fake player port для unit-тестов coordinator-а.

use super::*;

impl MediaOpenCoordinator {
    pub(super) fn start_fake(
        &mut self,
        client_key: MediaOpenClientKey,
        safe_label: SafeMediaLabel,
        task: impl FnOnce() -> PreparationResult + Send + 'static,
    ) -> Result<MediaOpenStartOutcome, MediaOpenStartError> {
        self.start_with_task(
            client_key,
            MediaOpenStartMode::RequireIdle,
            safe_label,
            move |_cancellation| task(),
        )
    }

    pub(super) fn attach_fake_player(&mut self, player_port: Arc<dyn MediaOpenPlayerPort>) {
        self.player_port = Some(player_port);
    }

    pub(super) fn supersede_fake(
        &mut self,
        expected_request_id: MediaOpenRequestId,
        client_key: MediaOpenClientKey,
        safe_label: SafeMediaLabel,
        task: impl FnOnce() -> PreparationResult + Send + 'static,
    ) -> Result<MediaOpenStartOutcome, MediaOpenStartError> {
        let current = self.current.as_ref().ok_or(MediaOpenStartError::Busy)?;
        if current.request_id != expected_request_id
            || !matches!(
                current.phase,
                MediaOpenPhase::Accepted | MediaOpenPhase::Preparing | MediaOpenPhase::Prepared
            )
        {
            return Err(MediaOpenStartError::Busy);
        }
        current
            .cancellation
            .cancel(MediaInstallCancellationCause::Superseded);
        self.current = None;
        self.start_fake(client_key, safe_label, task)
    }
}
