//! Неблокирующая post-Installed release/reconciliation транзакция.

use super::*;

/// Poll обязан вернуть ownership исходной ошибки, пока release receipt ещё не готов.
pub(super) enum PostInstalledCompensationPoll {
    Pending { failure: StrongMediaOpenError },
    Completed(StrongMediaOpenPoll),
}

impl PostInstalledCompensationPoll {
    /// Завершённый cleanup уже содержит окончательный strong-open outcome.
    pub(super) fn into_strong_poll(self) -> StrongMediaOpenPoll {
        match self {
            Self::Pending { .. } => StrongMediaOpenPoll::Pending,
            Self::Completed(outcome) => outcome,
        }
    }
}

impl AppState {
    /// Ошибка после Installed сначала превращается в exact release obligation.
    pub(super) fn begin_post_installed_compensation(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        pending: &mut PendingStrongMediaOpen,
        installed: InstalledSingleMediaOpen,
        failure: StrongMediaOpenError,
    ) -> StrongMediaOpenPoll {
        let MediaInstallCompletion::Installed {
            media_instance_id, ..
        } = installed.completion
        else {
            playlist_runtime.report_post_installed_compensation_failure();
            return StrongMediaOpenPoll::completed(Err(
                StrongMediaOpenError::PostInstalledCompensationFailed {
                    request_id: pending.request_id,
                    failure: Box::new(failure),
                    cleanup: PostInstalledCompensationFailure::ReleaseReceipt,
                },
            ));
        };
        match self
            .player_worker
            .release_installed_media(player_core::InstalledMediaRelease {
                request_id: installed.player_request_id,
                media_instance_id,
            }) {
            Ok(receipt) => {
                pending.phase = PendingStrongMediaOpenPhase::PostInstalledRelease {
                    installed,
                    failure,
                    receipt,
                };
                StrongMediaOpenPoll::Pending
            }
            Err(error) => {
                playlist_runtime.report_post_installed_compensation_failure();
                StrongMediaOpenPoll::completed(Err(
                    StrongMediaOpenError::PostInstalledCompensationFailed {
                        request_id: pending.request_id,
                        failure: Box::new(failure),
                        cleanup: PostInstalledCompensationFailure::ReleaseDispatch(
                            resume::player_dispatch_rejection(error),
                        ),
                    },
                ))
            }
        }
    }

    /// Applied receipt предшествует controller reconciliation и публикации исходной ошибки.
    pub(super) fn poll_post_installed_compensation(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        pending: &PendingStrongMediaOpen,
        installed: &InstalledSingleMediaOpen,
        receipt: &player_core::InstalledMediaReleaseReceipt,
        failure: StrongMediaOpenError,
    ) -> PostInstalledCompensationPoll {
        let expected_instance = match installed.completion {
            MediaInstallCompletion::Installed {
                media_instance_id, ..
            } => media_instance_id,
            _ => {
                playlist_runtime.report_post_installed_compensation_failure();
                return Self::completed_post_installed_compensation_failure(
                    pending.request_id,
                    failure,
                    PostInstalledCompensationFailure::ReleaseReceipt,
                );
            }
        };
        let outcome = match receipt.try_take_outcome() {
            Ok(Some(outcome)) => outcome,
            Ok(None) => return PostInstalledCompensationPoll::Pending { failure },
            Err(_) => {
                playlist_runtime.report_post_installed_compensation_failure();
                return Self::completed_post_installed_compensation_failure(
                    pending.request_id,
                    failure,
                    PostInstalledCompensationFailure::ReleaseReceipt,
                );
            }
        };
        match outcome {
            player_core::InstalledMediaReleaseOutcome::Applied { media_instance_id }
                if media_instance_id == expected_instance => {}
            outcome => {
                playlist_runtime.report_post_installed_compensation_failure();
                return Self::completed_post_installed_compensation_failure(
                    pending.request_id,
                    failure,
                    PostInstalledCompensationFailure::ReleaseOutcome(outcome),
                );
            }
        }
        if let Err(error) = playlist_runtime.reconcile_released_post_installed_candidate(
            pending.request_id,
            installed.player_request_id,
        ) {
            return Self::completed_post_installed_compensation_failure(
                pending.request_id,
                failure,
                PostInstalledCompensationFailure::Controller(error),
            );
        }
        self.clear_released_installed_media_source(expected_instance);
        self.finish_backend_swap_video_freeze();
        PostInstalledCompensationPoll::Completed(StrongMediaOpenPoll::completed(Err(
            StrongMediaOpenError::PostInstalledCompensated {
                request_id: pending.request_id,
                failure: Box::new(failure),
            },
        )))
    }

    /// Cleanup failure всегда остаётся отдельным fatal outcome.
    fn completed_post_installed_compensation_failure(
        request_id: MediaOpenRequestId,
        failure: StrongMediaOpenError,
        cleanup: PostInstalledCompensationFailure,
    ) -> PostInstalledCompensationPoll {
        PostInstalledCompensationPoll::Completed(StrongMediaOpenPoll::completed(Err(
            StrongMediaOpenError::PostInstalledCompensationFailed {
                request_id,
                failure: Box::new(failure),
                cleanup,
            },
        )))
    }
}
