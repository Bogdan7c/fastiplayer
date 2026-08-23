//! Terminal policy renderer-bound startup install без preparation ownership.

use std::sync::Arc;

use crate::media_open::ActiveMediaSource;
use crate::state::{StrongMediaOpenError, StrongMediaOpenPoll};

use super::StartupMediaController;
use super::orchestration::StartupMediaPhase;

/// Разрешённая terminal policy не позволяет флагу supersede скрыть fatal outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupInstallFailurePolicy {
    /// Доказанный cancel/rejection до enqueue разрешает применить retained winner.
    ApplyRetainedCancelWin,
    /// Обычная pre-barrier ошибка запускает restore skip либо CLI fallback.
    HandlePreBarrierFailure(crate::media_open::MediaOpenRequestId),
    /// Missing, fatal и post-barrier ошибки остаются sticky и запрещают retained apply.
    StickyFatal,
}

/// Классифицирует ошибку отдельно от mutation-кода terminal ветки.
fn startup_install_failure_policy(
    error: &StrongMediaOpenError,
    superseded: bool,
) -> StartupInstallFailurePolicy {
    if error.is_proven_pre_barrier_failure() && superseded {
        StartupInstallFailurePolicy::ApplyRetainedCancelWin
    } else if error.is_proven_pre_barrier_failure()
        && let Some(request_id) = error.terminal_request_id()
    {
        StartupInstallFailurePolicy::HandlePreBarrierFailure(request_id)
    } else {
        StartupInstallFailurePolicy::StickyFatal
    }
}

impl StartupMediaController {
    /// Забирает exactly-once terminal renderer transaction без ожидания worker-а.
    pub(super) fn poll_pending_install(
        &mut self,
        app_state: &mut crate::state::AppState,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
    ) -> bool {
        let Some(pending_context) = self.orchestration.pending_install.take() else {
            return false;
        };
        match app_state.poll_prepared_media_strong(playlist_runtime) {
            StrongMediaOpenPoll::Pending => {
                self.orchestration.pending_install = Some(pending_context);
                false
            }
            StrongMediaOpenPoll::Installed(installed) => {
                app_state.record_installed_media(installed.as_ref());
                if let Some(warning) = installed.position_warning {
                    let message = format!(
                        "Сохранённая позиция {}.{:03} с недоступна; media открыто на {}.{:03} с и оставлено на паузе",
                        warning.requested_position.as_secs(),
                        warning.requested_position.subsec_millis(),
                        warning.available_position.as_secs(),
                        warning.available_position.subsec_millis(),
                    );
                    self.startup_error = Some(message.clone());
                    app_state.set_startup_error(message);
                }
                if matches!(
                    installed.source.physical_source(),
                    ActiveMediaSource::DirectMediaUrl(_)
                ) {
                    tracing::info!("Startup direct media Installed");
                }
                if let Some((path, media_kind)) = pending_context.local_discovery
                    && let Err(error) = playlist_runtime
                        .start_sibling_discovery_for_installed_target(path, media_kind)
                {
                    tracing::warn!(error = %error, "CLI target установлен без sibling discovery");
                }
                let retained = match playlist_runtime.apply_retained_startup_actions() {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        let safe_error =
                            format!("retained startup action failed after Installed: {error}");
                        self.startup_error = Some(safe_error.clone());
                        app_state.set_startup_error(safe_error);
                        self.orchestration.phase = StartupMediaPhase::Failed;
                        return true;
                    }
                };
                self.startup_error = None;
                self.orchestration.phase = if pending_context.superseded
                    || !matches!(
                        retained,
                        crate::playlist_runtime::RetainedStartupApplyOutcome::NoAction
                    ) {
                    StartupMediaPhase::Idle
                } else {
                    StartupMediaPhase::Activated
                };
                true
            }
            StrongMediaOpenPoll::Failed(error) => {
                let safe_error = error.to_string();
                match startup_install_failure_policy(&error, pending_context.superseded) {
                    StartupInstallFailurePolicy::ApplyRetainedCancelWin => {
                        if let Err(retained_error) =
                            playlist_runtime.apply_retained_startup_actions()
                        {
                            let safe_error = format!(
                                "retained startup action failed after cancel-win: {retained_error}"
                            );
                            self.startup_error = Some(safe_error.clone());
                            app_state.set_startup_error(safe_error);
                            self.orchestration.phase = StartupMediaPhase::Failed;
                            return true;
                        }
                        self.orchestration.phase = StartupMediaPhase::Idle;
                        true
                    }
                    StartupInstallFailurePolicy::HandlePreBarrierFailure(request_id) => {
                        playlist_runtime.discard_retained_startup_actions();
                        if !pending_context.is_cli
                            && let Some(next) = playlist_runtime
                                .report_startup_restore_install_failure(
                                    request_id,
                                    Arc::<str>::from(safe_error.clone()),
                                )
                        {
                            self.start_restored_target(next, app_state, playlist_runtime);
                            return true;
                        }
                        self.handle_install_failure(safe_error, pending_context.is_cli, app_state);
                        true
                    }
                    StartupInstallFailurePolicy::StickyFatal => {
                        // Missing/fatal/post-barrier failure остаётся sticky даже после supersede.
                        playlist_runtime.discard_retained_startup_actions();
                        self.startup_error = Some(safe_error.clone());
                        app_state.set_startup_error(safe_error);
                        self.orchestration.phase = StartupMediaPhase::Failed;
                        true
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use player_core::MediaInstallCancellationCause;

    use crate::media_open::{
        MediaOpenInvariantViolation, MediaOpenRequestId, MediaOpenTerminalOutcome,
    };

    use super::*;

    /// Supersede меняет policy только для доказанного pre-barrier cancel-win.
    #[test]
    fn supersede_does_not_mask_missing_fatal_or_post_barrier_failure() {
        let request_id = MediaOpenRequestId::from_non_zero(
            NonZeroU64::new(23).expect("fixture request id is non-zero"),
        );
        let cancelled = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::Cancelled {
            request_id,
            cause: MediaInstallCancellationCause::Superseded,
        });
        let fatal = StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::FatalInvariant {
            request_id,
            violation: MediaOpenInvariantViolation::MissingPlayerControlResolution,
        });

        assert_eq!(
            startup_install_failure_policy(&cancelled, true),
            StartupInstallFailurePolicy::ApplyRetainedCancelWin
        );
        assert_eq!(
            startup_install_failure_policy(&cancelled, false),
            StartupInstallFailurePolicy::HandlePreBarrierFailure(request_id)
        );
        assert_eq!(
            startup_install_failure_policy(&StrongMediaOpenError::MissingTerminal, true),
            StartupInstallFailurePolicy::StickyFatal
        );
        assert_eq!(
            startup_install_failure_policy(&fatal, true),
            StartupInstallFailurePolicy::StickyFatal
        );
        assert_eq!(
            startup_install_failure_policy(
                &StrongMediaOpenError::MissingAuthorizationBarrier,
                true,
            ),
            StartupInstallFailurePolicy::StickyFatal
        );
    }
}
