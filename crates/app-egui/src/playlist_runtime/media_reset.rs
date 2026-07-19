//! Process-lifetime latest-only transport state полного Clear media reset.

use player_core::{
    ExactMediaTransportOutcome, ExactMediaTransportRequest, MediaInstanceId, PlayerWorkerSendError,
};

use super::PlaylistRuntime;
use super::controller::ControllerClearMediaResetCommit;

/// Безопасное сообщение не раскрывает player/controller internals.
const MEDIA_RESET_FAILURE_MESSAGE: &str = "Очередь очищена, но воспроизведение не удалось сбросить";

/// Runtime хранит только ещё не принятую worker-ом команду.
#[derive(Debug, Default)]
pub(super) struct PlaylistMediaResetOwner {
    /// Новый Clear заменяет старый неотправленный reset, сохраняя latest-only семантику.
    pending: Option<ExactMediaTransportRequest>,
}

/// Receipt решает, можно ли очищать renderer-bound app state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistMediaResetReceiptDisposition {
    /// Target был сброшен либо уже отсутствовал без нового current media.
    ClearAppMediaState,
    /// Новый current media выиграл race и не должен быть затронут старым Clear.
    SupersededByNewMedia,
    /// Player owner не смог подтвердить полный reset.
    Failed,
}

impl PlaylistMediaResetOwner {
    /// Добавляет только реальный exact target; Clear без active media не создаёт команду.
    pub(super) fn schedule(&mut self, request: Option<ExactMediaTransportRequest>) {
        if let Some(request) = request {
            self.pending = Some(request);
        }
    }

    /// Driver читает Copy request, не извлекая его до подтверждённого enqueue.
    pub(super) const fn pending_request(&self) -> Option<ExactMediaTransportRequest> {
        self.pending
    }

    /// Удаляет только тот request, который действительно принял worker.
    pub(super) fn mark_dispatched(&mut self, request: ExactMediaTransportRequest) {
        if self.pending == Some(request) {
            self.pending = None;
        }
    }

    /// Terminal disconnect завершает exact pending request без бесконечных retry.
    pub(super) fn abandon_disconnected(&mut self, request: ExactMediaTransportRequest) {
        self.mark_dispatched(request);
    }
}

impl PlaylistRuntime {
    /// Возвращает latest-only reset общему frame transport driver-у.
    pub(crate) const fn pending_media_reset_request(&self) -> Option<ExactMediaTransportRequest> {
        self.media_reset.pending_request()
    }

    /// Background scheduler продолжает будить driver, пока bounded queue была `Full`.
    pub(crate) const fn has_pending_media_reset(&self) -> bool {
        self.pending_media_reset_request().is_some()
    }

    /// Accepted enqueue передаёт дальнейшую ответственность request-owned receipt-у.
    pub(crate) fn mark_media_reset_dispatched(&mut self, request: ExactMediaTransportRequest) {
        self.media_reset.mark_dispatched(request);
    }

    /// `Full` сохраняет pending request, а disconnect завершает его безопасной ошибкой.
    pub(crate) fn report_media_reset_send_error(
        &mut self,
        request: ExactMediaTransportRequest,
        error: PlayerWorkerSendError,
    ) {
        match error {
            PlayerWorkerSendError::Full => {}
            PlayerWorkerSendError::Disconnected => {
                self.media_reset.abandon_disconnected(request);
                self.set_playlist_safe_feedback(MEDIA_RESET_FAILURE_MESSAGE);
                tracing::warn!(
                    media_instance_id = request.media_instance_id.get(),
                    "Clear media reset не принят: player worker отключён"
                );
            }
        }
    }

    /// Correlated terminal outcome не позволяет старому Clear остановить новое media.
    pub(crate) fn apply_media_reset_receipt(
        &mut self,
        requested_media_instance_id: MediaInstanceId,
        outcome: Result<ExactMediaTransportOutcome, player_core::ExactMediaTransportReceiptError>,
    ) -> PlaylistMediaResetReceiptDisposition {
        let disposition = match outcome {
            Ok(ExactMediaTransportOutcome::Applied { media_instance_id })
                if media_instance_id == requested_media_instance_id =>
            {
                PlaylistMediaResetReceiptDisposition::ClearAppMediaState
            }
            Ok(ExactMediaTransportOutcome::StaleInstance {
                requested_media_instance_id: stale_request,
                current_media_instance_id: Some(_),
            }) if stale_request == requested_media_instance_id => {
                PlaylistMediaResetReceiptDisposition::SupersededByNewMedia
            }
            Ok(ExactMediaTransportOutcome::StaleInstance {
                requested_media_instance_id: stale_request,
                current_media_instance_id: None,
            }) if stale_request == requested_media_instance_id => {
                PlaylistMediaResetReceiptDisposition::ClearAppMediaState
            }
            Ok(outcome) => {
                tracing::warn!(
                    ?outcome,
                    "Clear media reset завершился typed terminal failure"
                );
                PlaylistMediaResetReceiptDisposition::Failed
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "Clear media reset потерял authoritative owner outcome"
                );
                PlaylistMediaResetReceiptDisposition::Failed
            }
        };

        match disposition {
            PlaylistMediaResetReceiptDisposition::ClearAppMediaState => {
                let Some(controller) = self.controller.as_mut() else {
                    self.set_playlist_safe_feedback(MEDIA_RESET_FAILURE_MESSAGE);
                    return PlaylistMediaResetReceiptDisposition::Failed;
                };
                match controller.commit_clear_media_reset_stopped() {
                    ControllerClearMediaResetCommit::CommittedStopped => {}
                    ControllerClearMediaResetCommit::SupersededByActiveMedia => {
                        return PlaylistMediaResetReceiptDisposition::SupersededByNewMedia;
                    }
                }
            }
            PlaylistMediaResetReceiptDisposition::SupersededByNewMedia => {}
            PlaylistMediaResetReceiptDisposition::Failed => {
                self.set_playlist_safe_feedback(MEDIA_RESET_FAILURE_MESSAGE);
            }
        }
        disposition
    }
}

#[cfg(test)]
mod tests;
