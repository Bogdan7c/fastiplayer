//! Renderer-bound пошаговая strong install транзакция для UI/event loop.

use super::*;

mod admission;
mod compensation;
mod live_same_item_restore;
use compensation::PostInstalledCompensationPoll;
mod resume;
use resume::InstalledResumeCommit;
mod same_lineage;
use same_lineage::{PendingStrongLineageCommit, SameLineageRestorePosition};

/// Ошибка до staging возвращает exact playlist plan его runtime-владельцу.
pub(crate) struct UnstagedPlaylistMediaOpenError {
    pub(crate) error: StrongMediaOpenError,
    pub(crate) install: Box<crate::playlist_runtime::PlannedPlaylistInstall>,
}

/// Итог одного poll-а для playlist transport.
pub(crate) enum PlaylistStrongMediaOpenPoll {
    /// Обычный итог strong-протокола (pending, installed или ошибка без плана).
    Strong(StrongMediaOpenPoll),
    /// Файл очереди не подготовился до controller admission; план возвращается владельцу.
    TargetFailedBeforeAdmission(Box<UnstagedPlaylistMediaOpenError>),
}

/// Не теряет план очереди, если сам файл не подготовился до controller admission.
///
/// Controller принимает план только на staging, то есть после успешной подготовки файла.
/// Раньше при ошибке подготовки (файл удалён, формат битый) план выбрасывался вместе с
/// pending-транзакцией, controller не узнавал request ID, и автоматический пропуск
/// (`playlist.error_behavior = skip`) никогда не продолжал очередь (сессия 07).
///
/// `admission` — кому предназначался ещё не принятый план (`None`, если staging уже
/// прошёл). План возвращается только при ошибке самого target-а: отмена и прочие ошибки
/// протокола по-прежнему не продолжают очередь.
fn return_unadmitted_playlist_plan(
    error: Box<StrongMediaOpenError>,
    admission: Option<PendingStrongMediaAdmission>,
) -> PlaylistStrongMediaOpenPoll {
    let unadmitted_install = admission.and_then(|admission| match admission {
        PendingStrongMediaAdmission::Playlist(PreparedPlaylistTarget::Planned {
            install, ..
        }) => Some(install),
        PendingStrongMediaAdmission::Playlist(
            PreparedPlaylistTarget::QueueReplacement(_)
            | PreparedPlaylistTarget::RestoredCurrent(_),
        )
        | PendingStrongMediaAdmission::SameLineage => None,
    });
    match unadmitted_install {
        Some(install) if target_itself_failed(&error) => {
            PlaylistStrongMediaOpenPoll::TargetFailedBeforeAdmission(Box::new(
                UnstagedPlaylistMediaOpenError {
                    error: *error,
                    install: Box::new(install),
                },
            ))
        }
        Some(_) | None => PlaylistStrongMediaOpenPoll::Strong(StrongMediaOpenPoll::Failed(error)),
    }
}

/// Ошибка относится к самому файлу (не открылся, отвергнут player-ом), а не к отмене.
///
/// Отмена во время подготовки приходит как `PreparationFailed { kind: Cancelled }`: её
/// вызвал пользователь или смена очереди, и продолжать пропуском нельзя.
const fn target_itself_failed(error: &StrongMediaOpenError) -> bool {
    match error {
        StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PreparationFailed {
            kind,
            ..
        }) => !matches!(
            kind,
            crate::media_open::MediaPreparationFailureKind::Cancelled
        ),
        StrongMediaOpenError::Terminal(
            MediaOpenTerminalOutcome::PlayerRejected { .. }
            | MediaOpenTerminalOutcome::PlayerFailed { .. },
        ) => true,
        _ => false,
    }
}

/// Renderer-bound ownership незавершённой strong install транзакции.
pub(crate) struct PendingStrongMediaOpen {
    request_id: MediaOpenRequestId,
    candidate_owner: Option<ProductionCandidateOwner>,
    source: Option<ActiveMediaSource>,
    intent: PlaybackIntent,
    intent_revision: PlaybackIntentRevision,
    startup_position: crate::playlist_runtime::StartupPosition,
    position_restore_strategy: super::PreparedPositionRestoreStrategy,
    lineage_commit: PendingStrongLineageCommit,
    pre_barrier_failure: Option<StrongMediaOpenError>,
    phase: PendingStrongMediaOpenPhase,
}

impl PendingStrongMediaOpen {
    /// Возвращает exact coordinator request для outer consumer correlation.
    #[must_use]
    pub(crate) const fn request_id(&self) -> MediaOpenRequestId {
        self.request_id
    }
}

struct PendingStrongMediaStaging {
    video_resource_port: player_core::MediaInstallVideoResourcePort,
    admission: PendingStrongMediaAdmission,
}

/// Staging явно различает queue admission и S25 install без queue mutation.
enum PendingStrongMediaAdmission {
    /// Queue/controller получит existing planned install guard.
    Playlist(PreparedPlaylistTarget),
    /// Same-item switch не создаёт reservation, traversal visit либо новую lineage.
    SameLineage,
}

/// Player protocol и post-Installed intent acknowledgement остаются разными этапами.
enum PendingStrongMediaOpenPhase {
    /// Временный маркер показывает, что текущая фаза вынута только на время одного poll.
    Polling,
    Protocol {
        deferred_rejection: Option<PlaylistMediaOpenGateError>,
        pending_staging: Option<PendingStrongMediaStaging>,
    },
    PositionRestore {
        installed: InstalledSingleMediaOpen,
        media_instance_id: player_core::MediaInstanceId,
        restore: live_same_item_restore::PendingPositionRestore,
    },
    PlaybackIntent {
        installed: InstalledSingleMediaOpen,
        resume_commit: InstalledResumeCommit,
        receipt: player_core::PlaybackIntentUpdateReceipt,
    },
    PostInstalledRelease {
        installed: InstalledSingleMediaOpen,
        failure: StrongMediaOpenError,
        receipt: player_core::InstalledMediaReleaseReceipt,
    },
}

impl PendingStrongMediaOpenPhase {
    /// Забирает текущую фазу, не подменяя её другой реальной фазой транзакции.
    fn take_for_poll(&mut self) -> Self {
        std::mem::replace(self, Self::Polling)
    }

    /// Возвращает ожидающую фазу, только если вложенный шаг не установил successor.
    fn retain_after_pending_poll(&mut self, waiting_phase: Self) {
        if matches!(self, Self::Polling) {
            *self = waiting_phase;
        }
    }
}

impl AppState {
    /// Синхронизирует renderer-bound post-Installed phase с accepted D52 revision.
    pub(crate) fn update_pending_strong_playlist_intent(
        &mut self,
        request_id: MediaOpenRequestId,
        revision: PlaybackIntentRevision,
        intent: PlaybackIntent,
    ) {
        let Some(pending) = self.pending_strong_media_open.as_mut() else {
            return;
        };
        if pending.request_id == request_id {
            pending.intent = intent;
            pending.intent_revision = revision;
        }
    }

    /// Продвигает не более одного логического этапа и никогда не ждёт worker receipt.
    ///
    /// Для владельцев вне очереди (startup, VOD recovery, same-item switch): ошибка target-а
    /// до admission остаётся обычной ошибкой, как и раньше.
    pub(crate) fn poll_prepared_media_strong(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
    ) -> StrongMediaOpenPoll {
        match self.poll_prepared_playlist_media_strong(playlist_runtime) {
            PlaylistStrongMediaOpenPoll::Strong(poll) => poll,
            PlaylistStrongMediaOpenPoll::TargetFailedBeforeAdmission(unstaged) => {
                StrongMediaOpenPoll::Failed(Box::new(unstaged.error))
            }
        }
    }

    /// То же, что [`Self::poll_prepared_media_strong`], но для playlist transport: если файл
    /// очереди не подготовился до controller admission, план возвращается вместе с ошибкой.
    pub(crate) fn poll_prepared_playlist_media_strong(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
    ) -> PlaylistStrongMediaOpenPoll {
        let Some(mut pending) = self.pending_strong_media_open.take() else {
            return PlaylistStrongMediaOpenPoll::Strong(StrongMediaOpenPoll::Pending);
        };
        let phase = pending.phase.take_for_poll();
        let result = match phase {
            PendingStrongMediaOpenPhase::Polling => PlaylistStrongMediaOpenPoll::Strong(
                StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::PendingPhaseStateLost)),
            ),
            PendingStrongMediaOpenPhase::Protocol {
                mut deferred_rejection,
                mut pending_staging,
            } => {
                let result = self.poll_strong_media_protocol(
                    playlist_runtime,
                    &mut pending,
                    &mut deferred_rejection,
                    &mut pending_staging,
                );
                match result {
                    StrongMediaOpenPoll::Pending => {
                        pending.phase.retain_after_pending_poll(
                            PendingStrongMediaOpenPhase::Protocol {
                                deferred_rejection,
                                pending_staging,
                            },
                        );
                        PlaylistStrongMediaOpenPoll::Strong(StrongMediaOpenPoll::Pending)
                    }
                    StrongMediaOpenPoll::Failed(error) => return_unadmitted_playlist_plan(
                        error,
                        pending_staging.map(|staging| staging.admission),
                    ),
                    completed => PlaylistStrongMediaOpenPoll::Strong(completed),
                }
            }
            PendingStrongMediaOpenPhase::PlaybackIntent {
                mut installed,
                resume_commit,
                receipt,
            } => {
                let result = self.poll_strong_media_intent(
                    playlist_runtime,
                    &mut pending,
                    &mut installed,
                    resume_commit,
                    &receipt,
                );
                if matches!(result, StrongMediaOpenPoll::Pending) {
                    pending.phase.retain_after_pending_poll(
                        PendingStrongMediaOpenPhase::PlaybackIntent {
                            installed,
                            resume_commit,
                            receipt,
                        },
                    );
                }
                PlaylistStrongMediaOpenPoll::Strong(result)
            }
            PendingStrongMediaOpenPhase::PositionRestore {
                mut installed,
                media_instance_id,
                restore,
            } => {
                let result = self.poll_strong_media_position_restore(
                    playlist_runtime,
                    &mut pending,
                    &mut installed,
                    media_instance_id,
                    &restore,
                );
                if matches!(result, StrongMediaOpenPoll::Pending) {
                    pending.phase.retain_after_pending_poll(
                        PendingStrongMediaOpenPhase::PositionRestore {
                            installed,
                            media_instance_id,
                            restore,
                        },
                    );
                }
                PlaylistStrongMediaOpenPoll::Strong(result)
            }
            PendingStrongMediaOpenPhase::PostInstalledRelease {
                installed,
                failure,
                receipt,
            } => {
                let result = self.poll_post_installed_compensation(
                    playlist_runtime,
                    &pending,
                    &installed,
                    &receipt,
                    failure,
                );
                if let PostInstalledCompensationPoll::Pending { failure } = result {
                    pending.phase.retain_after_pending_poll(
                        PendingStrongMediaOpenPhase::PostInstalledRelease {
                            installed,
                            failure,
                            receipt,
                        },
                    );
                    PlaylistStrongMediaOpenPoll::Strong(StrongMediaOpenPoll::Pending)
                } else {
                    PlaylistStrongMediaOpenPoll::Strong(result.into_strong_poll())
                }
            }
        };
        match result {
            PlaylistStrongMediaOpenPoll::Strong(StrongMediaOpenPoll::Pending) => {
                self.pending_strong_media_open = Some(pending);
                PlaylistStrongMediaOpenPoll::Strong(StrongMediaOpenPoll::Pending)
            }
            completed => completed,
        }
    }

    /// `true`, пока scheduler обязан продолжать доставлять wake/poll.
    #[must_use]
    pub(crate) const fn has_pending_prepared_media_strong(&self) -> bool {
        self.pending_strong_media_open.is_some()
    }

    /// Запрашивает lossless cancel startup transaction; enqueue-win всё равно будет drained.
    pub(crate) fn supersede_pending_prepared_media_strong(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
    ) -> Result<(), StrongMediaOpenError> {
        let Some(pending) = self.pending_strong_media_open.as_ref() else {
            return Ok(());
        };
        if !matches!(pending.phase, PendingStrongMediaOpenPhase::Protocol { .. }) {
            return Ok(());
        }
        playlist_runtime
            .cancel_media_open_lossless(
                pending.request_id,
                MediaInstallCancellationCause::Superseded,
            )
            .map(|_| ())
            .map_err(StrongMediaOpenError::Command)
    }

    /// Coordinator phase machine уже сама drain-ит только готовые slots.
    fn poll_strong_media_protocol(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        pending: &mut PendingStrongMediaOpen,
        deferred_rejection: &mut Option<PlaylistMediaOpenGateError>,
        pending_staging: &mut Option<PendingStrongMediaStaging>,
    ) -> StrongMediaOpenPoll {
        let Some(snapshot) = playlist_runtime.media_open_snapshot() else {
            return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::MissingTerminal));
        };
        if snapshot.request_id != pending.request_id {
            return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::Command(
                MediaOpenCommandError::StaleRequest,
            )));
        }
        match snapshot.same_lineage_position {
            crate::media_open::SameLineagePositionPreparationPhase::ReadyForPositionPreparation => {
                if let Err(error) =
                    self.capture_same_lineage_restore_before_barrier(playlist_runtime, pending)
                {
                    pending.pre_barrier_failure = Some(error);
                    return match playlist_runtime.cancel_media_open_lossless(
                        pending.request_id,
                        MediaInstallCancellationCause::StructuralInvalidation,
                    ) {
                        Ok(_) => StrongMediaOpenPoll::Pending,
                        Err(error) => StrongMediaOpenPoll::completed(Err(
                            StrongMediaOpenError::Command(error),
                        )),
                    };
                }
                return match playlist_runtime
                    .prepare_same_lineage_media_open_position(pending.request_id)
                {
                    Ok(()) => StrongMediaOpenPoll::Pending,
                    Err(error) => {
                        pending.pre_barrier_failure =
                            Some(StrongMediaOpenError::PlaylistGate(error));
                        match playlist_runtime.cancel_media_open_lossless(
                            pending.request_id,
                            MediaInstallCancellationCause::StructuralInvalidation,
                        ) {
                            Ok(_) => StrongMediaOpenPoll::Pending,
                            Err(error) => StrongMediaOpenPoll::completed(Err(
                                StrongMediaOpenError::Command(error),
                            )),
                        }
                    }
                };
            }
            crate::media_open::SameLineagePositionPreparationPhase::WaitingForPlayerReady
            | crate::media_open::SameLineagePositionPreparationPhase::PreparationDispatched => {
                return StrongMediaOpenPoll::Pending;
            }
            crate::media_open::SameLineagePositionPreparationPhase::NotRequired
            | crate::media_open::SameLineagePositionPreparationPhase::ReadyToCommit => {}
        }
        match snapshot.phase {
            MediaOpenPhase::Accepted
            | MediaOpenPhase::Preparing
            | MediaOpenPhase::PlayerStaging
            | MediaOpenPhase::AuthorizationDispatchPending
            | MediaOpenPhase::EnqueuedAtPlayerOwner => StrongMediaOpenPoll::Pending,
            MediaOpenPhase::Prepared => self.stage_prepared_playlist_source(
                playlist_runtime,
                pending,
                deferred_rejection,
                pending_staging,
                snapshot,
            ),
            MediaOpenPhase::ReadyToCommit => {
                if deferred_rejection.is_some() {
                    return StrongMediaOpenPoll::Pending;
                }
                if let Err(error) =
                    self.capture_same_lineage_restore_before_barrier(playlist_runtime, pending)
                {
                    pending.pre_barrier_failure = Some(error);
                    return match playlist_runtime.cancel_media_open_lossless(
                        pending.request_id,
                        MediaInstallCancellationCause::StructuralInvalidation,
                    ) {
                        Ok(_) => StrongMediaOpenPoll::Pending,
                        Err(error) => StrongMediaOpenPoll::completed(Err(
                            StrongMediaOpenError::Command(error),
                        )),
                    };
                }
                if matches!(
                    pending.lineage_commit,
                    PendingStrongLineageCommit::SameLineage { .. }
                ) {
                    return match playlist_runtime
                        .authorize_ready_same_lineage_media_open(pending.request_id)
                    {
                        Ok(()) => StrongMediaOpenPoll::Pending,
                        Err(error) => {
                            *deferred_rejection = Some(error);
                            match playlist_runtime.cancel_media_open_lossless(
                                pending.request_id,
                                MediaInstallCancellationCause::StructuralInvalidation,
                            ) {
                                Ok(_) => StrongMediaOpenPoll::Pending,
                                Err(error) => StrongMediaOpenPoll::completed(Err(
                                    StrongMediaOpenError::Command(error),
                                )),
                            }
                        }
                    };
                }
                let authorization = if playlist_runtime.playlist_install_matches(pending.request_id)
                {
                    playlist_runtime.authorize_ready_target_install(pending.request_id)
                } else {
                    playlist_runtime.authorize_ready_media_open(pending.request_id)
                };
                match authorization {
                    Ok(AuthorizationDispatchResolution::EnqueuedAtPlayerOwner) => {
                        StrongMediaOpenPoll::Pending
                    }
                    Ok(_) => StrongMediaOpenPoll::completed(Err(
                        StrongMediaOpenError::MissingAuthorizationBarrier,
                    )),
                    Err(error) => {
                        *deferred_rejection = Some(error);
                        match playlist_runtime.cancel_media_open_lossless(
                            pending.request_id,
                            MediaInstallCancellationCause::StructuralInvalidation,
                        ) {
                            Ok(_) => StrongMediaOpenPoll::Pending,
                            Err(error) => StrongMediaOpenPoll::completed(Err(
                                StrongMediaOpenError::Command(error),
                            )),
                        }
                    }
                }
            }
            MediaOpenPhase::Installed => {
                self.begin_post_installed_intent(playlist_runtime, pending)
            }
            MediaOpenPhase::Failed => {
                let terminal = match playlist_runtime.take_media_open_terminal(pending.request_id) {
                    Ok(Some(terminal)) => terminal,
                    Ok(None) => {
                        return StrongMediaOpenPoll::completed(Err(
                            StrongMediaOpenError::MissingTerminal,
                        ));
                    }
                    Err(error) => {
                        return StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::Command(
                            error,
                        )));
                    }
                };
                if let Some(error) = pending.pre_barrier_failure.take() {
                    return StrongMediaOpenPoll::completed(Err(error));
                }
                StrongMediaOpenPoll::completed(Err(StrongMediaOpenError::Terminal(terminal)))
            }
        }
    }
}

#[cfg(test)]
mod unadmitted_plan_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// Pending receipt возвращает вынутую фазу, а явный successor остаётся владельцем slot-а.
    #[test]
    fn pending_poll_retains_waiting_phase_without_overwriting_successor() {
        let mut waiting_slot = PendingStrongMediaOpenPhase::Protocol {
            deferred_rejection: None,
            pending_staging: None,
        };
        let waiting_phase = waiting_slot.take_for_poll();
        assert!(matches!(waiting_slot, PendingStrongMediaOpenPhase::Polling));
        waiting_slot.retain_after_pending_poll(waiting_phase);
        assert!(matches!(
            waiting_slot,
            PendingStrongMediaOpenPhase::Protocol { .. }
        ));

        let mut advanced_slot = PendingStrongMediaOpenPhase::Protocol {
            deferred_rejection: None,
            pending_staging: None,
        };
        advanced_slot.retain_after_pending_poll(PendingStrongMediaOpenPhase::Polling);
        assert!(matches!(
            advanced_slot,
            PendingStrongMediaOpenPhase::Protocol { .. }
        ));
    }

    /// Cancel-win разрешает fallback, а missing/fatal terminal остаётся sticky fatal.
    #[test]
    fn fallback_classification_accepts_only_proven_pre_barrier_terminal() {
        let request_id = MediaOpenRequestId::from_non_zero(
            NonZeroU64::new(17).expect("fixture request id is non-zero"),
        );
        let cancelled = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::Cancelled {
            request_id,
            cause: MediaInstallCancellationCause::Superseded,
        });
        let fatal = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::FatalInvariant {
            request_id,
            violation:
                crate::media_open::MediaOpenInvariantViolation::MissingPlayerControlResolution,
        });

        assert!(cancelled.is_proven_pre_barrier_failure());
        assert_eq!(cancelled.terminal_request_id(), Some(request_id));
        assert!(!fatal.is_proven_pre_barrier_failure());
        assert_eq!(fatal.terminal_request_id(), Some(request_id));
        assert!(!StrongMediaOpenError::MissingTerminal.is_proven_pre_barrier_failure());
        assert!(StrongMediaOpenError::PendingPhaseStateLost.may_have_crossed_install_barrier());
        assert!(!StrongMediaOpenError::PendingPhaseStateLost.is_proven_pre_barrier_failure());
    }
}
