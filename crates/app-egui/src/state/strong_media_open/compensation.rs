//! Блокирующая компенсация для legacy strong-open callers вне UI poll driver-а.

use super::{
    AppState, InstalledSingleMediaOpen, MediaOpenRequestId, PlaylistRuntime,
    PostInstalledCompensationFailure, StrongMediaOpenError,
};

impl AppState {
    /// Освобождает exact установленный candidate и только потом публикует исходную ошибку.
    pub(super) fn compensate_post_installed_failure_blocking(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        request_id: MediaOpenRequestId,
        installed: &InstalledSingleMediaOpen,
        failure: StrongMediaOpenError,
    ) -> StrongMediaOpenError {
        let media_instance_id = match installed.completion {
            player_core::MediaInstallCompletion::Installed {
                media_instance_id, ..
            } => media_instance_id,
            _ => {
                playlist_runtime.report_post_installed_compensation_failure();
                return StrongMediaOpenError::PostInstalledCompensationFailed {
                    request_id,
                    failure: Box::new(failure),
                    cleanup: PostInstalledCompensationFailure::ReleaseReceipt,
                };
            }
        };
        let release_receipt =
            match self
                .player_worker
                .release_installed_media(player_core::InstalledMediaRelease {
                    request_id: installed.player_request_id,
                    media_instance_id,
                }) {
                Ok(receipt) => receipt,
                Err(error) => {
                    playlist_runtime.report_post_installed_compensation_failure();
                    return StrongMediaOpenError::PostInstalledCompensationFailed {
                        request_id,
                        failure: Box::new(failure),
                        cleanup: PostInstalledCompensationFailure::ReleaseDispatch(
                            player_dispatch_rejection(error),
                        ),
                    };
                }
            };
        match release_receipt.wait_for_outcome() {
            Ok(player_core::InstalledMediaReleaseOutcome::Applied {
                media_instance_id: released_instance,
            }) if released_instance == media_instance_id => {}
            Ok(outcome) => {
                playlist_runtime.report_post_installed_compensation_failure();
                return StrongMediaOpenError::PostInstalledCompensationFailed {
                    request_id,
                    failure: Box::new(failure),
                    cleanup: PostInstalledCompensationFailure::ReleaseOutcome(outcome),
                };
            }
            Err(_) => {
                playlist_runtime.report_post_installed_compensation_failure();
                return StrongMediaOpenError::PostInstalledCompensationFailed {
                    request_id,
                    failure: Box::new(failure),
                    cleanup: PostInstalledCompensationFailure::ReleaseReceipt,
                };
            }
        }
        if let Err(error) = playlist_runtime
            .reconcile_released_post_installed_candidate(request_id, installed.player_request_id)
        {
            return StrongMediaOpenError::PostInstalledCompensationFailed {
                request_id,
                failure: Box::new(failure),
                cleanup: PostInstalledCompensationFailure::Controller(error),
            };
        }
        self.clear_released_installed_media_source(media_instance_id);
        self.finish_backend_swap_video_freeze();
        StrongMediaOpenError::PostInstalledCompensated {
            request_id,
            failure: Box::new(failure),
        }
    }
}

/// Player worker transport error остаётся typed и не раскрывает устройство канала.
fn player_dispatch_rejection(
    error: player_core::PlayerWorkerSendError,
) -> crate::media_open::PlayerDispatchRejection {
    match error {
        player_core::PlayerWorkerSendError::Full => {
            crate::media_open::PlayerDispatchRejection::Backpressure
        }
        player_core::PlayerWorkerSendError::Disconnected => {
            crate::media_open::PlayerDispatchRejection::Disconnected
        }
    }
}
