//! Показ текущего рендера в транзакции настроек (сессия 02a, «одно окно — одна surface»).
//!
//! Здесь только перевод между контрактом `RenderShell` (`reconfigure/detach/reattach
//! _presentation`) и типизированными ошибками настроек (`fastiplayer-settings`). Порядок шагов
//! транзакции держит `RendererLifecycleCoordinator`.

use fastiplayer_settings::{
    RenderCommittedSettingsUpdate, RendererRecreationApplyError, RendererRecreationApplyErrorKind,
    RendererRecreationRollbackError, RendererRecreationRollbackErrorKind,
};
use render_shell_api::{
    PresentationDetachOutcome, PresentationReattachError, PresentationReattachOutcome,
    PresentationReconfigureError, PresentationReconfigured,
};
use tracing::{info, warn};

use super::{LiveRendererRecreation, apply_error};
use crate::render_backend::presentation_settings;

/// Почему перенастройка показа на месте не применена — в терминах отката настроек.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PresentationReconfigureFailure {
    /// Отказ без последствий: прежняя конфигурация показа работает (`ApplyFailed`).
    Rejected(RendererRecreationApplyError),

    /// Отказ, и прежнюю конфигурацию вернуть не удалось: у окна нет рабочей surface
    /// (`ApplyAndRollbackFailed` с `SurfaceInvalidated`).
    PresentationLost {
        /// Почему не применилась новая конфигурация.
        apply_error: RendererRecreationApplyError,
        /// Почему не вернулась прежняя.
        rollback_error: RendererRecreationRollbackError,
    },
}

impl LiveRendererRecreation<'_> {
    /// Применяет `[render.vulkan]` к surface текущего рендера (тот же маппинг, что при создании).
    pub(super) fn reconfigure_current_presentation(
        &mut self,
        next: &RenderCommittedSettingsUpdate,
    ) -> Result<(), PresentationReconfigureFailure> {
        let settings =
            presentation_settings(self.renderer.family(), &next.vulkan).map_err(|missing| {
                PresentationReconfigureFailure::Rejected(apply_error(
                    RendererRecreationApplyErrorKind::PresentationReconfigure,
                    format!(
                        "renderer family '{}' is not built into this application",
                        missing.family.stable_id()
                    ),
                ))
            })?;
        match self.renderer.reconfigure_presentation(settings) {
            Ok(outcome) => {
                log_reconfigured(outcome);
                Ok(())
            }
            Err(error) => Err(reconfigure_failure_from_contract(error)),
        }
    }

    /// Старый рендер отпускает окно перед созданием кандидата.
    pub(super) fn detach_current_presentation(&mut self) {
        match self.renderer.detach_presentation() {
            PresentationDetachOutcome::Detached => {
                info!("Old renderer released the window before candidate creation");
            }
            PresentationDetachOutcome::AlreadyDetached => {
                // Окно отпущено прошлой неудачной транзакцией; кандидат всё равно один.
                warn!("Old renderer had already released the window");
            }
        }
    }

    /// Старый рендер возвращает себе окно при откате.
    pub(super) fn reattach_current_presentation(
        &mut self,
    ) -> Result<(), RendererRecreationRollbackError> {
        match self.renderer.reattach_presentation() {
            Ok(PresentationReattachOutcome::Reattached) => {
                info!("Old renderer took the window back after a failed recreation");
                Ok(())
            }
            Ok(PresentationReattachOutcome::AlreadyAttached) => Ok(()),
            Err(error) => Err(reattach_rollback_error(&error)),
        }
    }
}

/// Пишет в лог, как именно применились настройки показа.
fn log_reconfigured(outcome: PresentationReconfigured) {
    match outcome {
        PresentationReconfigured::AsRequested => {
            info!("Presentation settings applied in place");
        }
        PresentationReconfigured::PresentModeFallback { requested } => warn!(
            ?requested,
            "Requested present mode is unavailable; the fallback mode was applied in place"
        ),
        PresentationReconfigured::AlreadyActive => {
            info!("Presentation settings already active; the surface was not touched");
        }
    }
}

/// Отказ контракта → путь отката настроек. Различие «прежнее работает» / «показа нет»
/// сохраняется: только `PresentationLost` даёт ошибку отката.
pub(super) fn reconfigure_failure_from_contract(
    error: PresentationReconfigureError,
) -> PresentationReconfigureFailure {
    match error {
        PresentationReconfigureError::PresentationLost { message } => {
            PresentationReconfigureFailure::PresentationLost {
                apply_error: apply_error(
                    RendererRecreationApplyErrorKind::PresentationReconfigure,
                    "surface rejected the new presentation settings",
                ),
                rollback_error: RendererRecreationRollbackError {
                    kind: RendererRecreationRollbackErrorKind::SurfaceInvalidated,
                    message,
                },
            }
        }
        rejected @ (PresentationReconfigureError::PresentationDetached
        | PresentationReconfigureError::AlphaPreferenceChangeUnsupported
        | PresentationReconfigureError::RejectedPreviousRestored { .. }) => {
            PresentationReconfigureFailure::Rejected(apply_error(
                RendererRecreationApplyErrorKind::PresentationReconfigure,
                rejected.to_string(),
            ))
        }
    }
}

/// Провал возврата окна — откат не удался: у старого рендера нет surface.
pub(super) fn reattach_rollback_error(
    error: &PresentationReattachError,
) -> RendererRecreationRollbackError {
    RendererRecreationRollbackError {
        kind: RendererRecreationRollbackErrorKind::SurfaceInvalidated,
        message: format!("old renderer could not take the window back: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use fastiplayer_settings::{
        RendererRecreationApplyErrorKind, RendererRecreationRollbackErrorKind,
    };
    use render_shell_api::{PresentationReattachError, PresentationReconfigureError};

    use super::{
        PresentationReconfigureFailure, reattach_rollback_error, reconfigure_failure_from_contract,
    };

    /// Отказы, после которых прежний показ работает, — `Rejected`; только потеря surface даёт
    /// ошибку отката `SurfaceInvalidated`. Сообщение рендера не теряется.
    #[test]
    fn only_lost_presentation_becomes_a_rollback_failure() {
        let rejected = [
            PresentationReconfigureError::PresentationDetached,
            PresentationReconfigureError::AlphaPreferenceChangeUnsupported,
            PresentationReconfigureError::RejectedPreviousRestored {
                message: "driver said no".to_owned(),
            },
        ];
        for error in rejected {
            let expected_message = error.to_string();
            match reconfigure_failure_from_contract(error) {
                PresentationReconfigureFailure::Rejected(apply_error) => {
                    assert_eq!(
                        apply_error.kind,
                        RendererRecreationApplyErrorKind::PresentationReconfigure
                    );
                    assert_eq!(apply_error.message, expected_message);
                }
                other => panic!("recoverable rejection must stay Rejected, got {other:?}"),
            }
        }

        let lost =
            reconfigure_failure_from_contract(PresentationReconfigureError::PresentationLost {
                message: "both configurations rejected".to_owned(),
            });
        match lost {
            PresentationReconfigureFailure::PresentationLost {
                apply_error,
                rollback_error,
            } => {
                assert_eq!(
                    apply_error.kind,
                    RendererRecreationApplyErrorKind::PresentationReconfigure
                );
                assert_eq!(
                    rollback_error.kind,
                    RendererRecreationRollbackErrorKind::SurfaceInvalidated
                );
                assert_eq!(rollback_error.message, "both configurations rejected");
            }
            other => panic!("lost presentation must be a rollback failure, got {other:?}"),
        }
    }

    /// Провал возврата окна — `SurfaceInvalidated` с причиной рендера.
    #[test]
    fn failed_reattach_is_surface_invalidated() {
        let error = reattach_rollback_error(&PresentationReattachError::ConfigureRejected {
            message: "format mismatch".to_owned(),
        });

        assert_eq!(
            error.kind,
            RendererRecreationRollbackErrorKind::SurfaceInvalidated
        );
        assert!(
            error.message.contains("format mismatch"),
            "{}",
            error.message
        );
    }
}
