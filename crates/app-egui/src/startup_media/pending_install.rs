//! Terminal policy renderer-bound startup install без preparation ownership.

use std::path::PathBuf;
use std::sync::Arc;

use crate::media_open::ActiveMediaSource;
use crate::state::{StrongMediaOpenError, StrongMediaOpenPoll, StrongMediaOpenUserOutcome};

use super::StartupMediaController;
use super::orchestration::StartupMediaPhase;

/// Локальный файл, который устанавливает startup (CLI или восстановленный элемент).
pub(super) struct StartupLocalTarget {
    /// Путь нужен только для имени файла в тексте ошибки и для sibling discovery;
    /// в лог он не попадает.
    pub(super) path: PathBuf,
    /// Искать ли соседние файлы после успешной установки.
    pub(super) sibling_discovery: StartupSiblingDiscovery,
}

/// Решение о sibling discovery после успешной startup-установки локального файла.
pub(super) enum StartupSiblingDiscovery {
    /// Только CLI target запускает поиск соседей после domain commit-а.
    AfterInstall(playlist_discovery::LocalMediaKind),
    /// Восстановленный элемент очереди: очередь уже есть, соседей не ищем.
    Skip,
}

/// Что устанавливает startup: от этого зависит шаблон текста ошибки установки.
pub(super) enum StartupInstallTarget {
    /// Локальный файл (CLI или восстановленный): имя файла в тексте и sibling discovery.
    Local(StartupLocalTarget),
    /// Web-ссылка: текст подписывается безопасным доменом (UX сессия 08).
    Web {
        /// Домен сайта без пути/query/логина; `None` — домен неизвестен.
        display_host: Option<String>,
    },
    /// Первый элемент startup-плейлиста: источник заранее неизвестен, поэтому окно
    /// сохраняет прежний технический текст.
    PlaylistEntry,
}

impl StartupInstallTarget {
    /// Цель-ссылка с уже безопасным доменом (`StartupUrlLocator::display_host`).
    pub(super) fn web(display_host: Option<&str>) -> Self {
        Self::Web {
            display_host: display_host.map(str::to_owned),
        }
    }
}

/// Тексты неудачной startup-установки: для окна и для бейджа строки очереди.
struct StartupInstallFailureTexts {
    /// Сообщение в окне.
    user_message: String,
    /// Короткая причина для строки восстановленного элемента очереди.
    row_summary: String,
}

/// Строит тексты ошибки startup-установки без побочных эффектов.
///
/// Для локального файла с известной причиной — тот же шаблон, что у кнопки Open
/// («Не удалось открыть «clip.mkv»: формат видео не поддерживается»); неклассифицированная
/// ошибка — «внутренняя ошибка плеера». Для web-ссылки — «Не удалось открыть ссылку
/// (youtube.com): …». Бейдж строки — короткая причина, если она известна.
fn startup_install_failure_texts(
    error: &StrongMediaOpenError,
    target: &StartupInstallTarget,
) -> StartupInstallFailureTexts {
    let technical_text = error.to_string();
    let row_summary = error.user_failure_reason().map_or_else(
        || technical_text.clone(),
        crate::local_open_message::local_open_failure_row_summary,
    );
    let user_message = match (target, error.user_outcome()) {
        (StartupInstallTarget::Local(local), StrongMediaOpenUserOutcome::Failed(reason)) => {
            crate::local_open_message::local_open_failure_message(&local.path, reason)
        }
        (
            StartupInstallTarget::Web { display_host },
            StrongMediaOpenUserOutcome::Failed(reason),
        ) => crate::web_open_message::web_open_failure_message(display_host.as_deref(), reason),
        // Отмена без supersede и элемент startup-плейлиста: прежнее поведение (риски сессии 03).
        (
            StartupInstallTarget::Local(_) | StartupInstallTarget::Web { .. },
            StrongMediaOpenUserOutcome::Cancelled | StrongMediaOpenUserOutcome::Busy,
        )
        | (StartupInstallTarget::PlaylistEntry, _) => technical_text,
    };
    StartupInstallFailureTexts {
        user_message,
        row_summary,
    }
}

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
                        "Сохранённая позиция {}.{:03} с недоступна; media открыто на {}.{:03} с",
                        warning.requested_position.as_secs(),
                        warning.requested_position.subsec_millis(),
                        warning.available_position.as_secs(),
                        warning.available_position.subsec_millis(),
                    );
                    self.startup_error = Some(message.clone());
                    // Это информация, а не ошибка: media открыто, только с другой позиции.
                    // Раньше текст шёл красной «вечной» ошибкой; теперь — инфо-toast (сессия 04).
                    // Отметка startup readiness сохранена как была у `set_startup_error`.
                    app_state.abort_startup_readiness(
                        crate::startup_readiness::StartupReadinessAbortReason::PreparationFailed,
                    );
                    app_state.notify_info(message);
                }
                if matches!(
                    installed.source.physical_source(),
                    ActiveMediaSource::Web(intent)
                        if intent.ingress() == web_media_core::WebMediaIngressKind::DirectResource
                ) {
                    tracing::info!("Startup direct media Installed");
                }
                if let StartupInstallTarget::Local(StartupLocalTarget {
                    path,
                    sibling_discovery: StartupSiblingDiscovery::AfterInstall(media_kind),
                }) = pending_context.target
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
                let failure_texts = startup_install_failure_texts(&error, &pending_context.target);
                let failure_policy =
                    startup_install_failure_policy(&error, pending_context.superseded);
                // Restore fallback может сразу перейти к следующему item и не дойти до общего
                // install-failure handler, поэтому terminal owner фиксирует safe cause здесь.
                tracing::warn!(
                    error = %safe_error,
                    terminal_request_id = ?error.terminal_request_id(),
                    may_have_crossed_install_barrier = error.may_have_crossed_install_barrier(),
                    superseded = pending_context.superseded,
                    ?failure_policy,
                    "Startup strong media install failed"
                );
                match failure_policy {
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
                                    Arc::<str>::from(failure_texts.row_summary),
                                )
                        {
                            self.start_restored_target(next, app_state, playlist_runtime);
                            return true;
                        }
                        self.handle_install_failure(
                            failure_texts.user_message,
                            pending_context.is_cli,
                            app_state,
                        );
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

    fn player_failed(
        reason: crate::media_open::PlayerInstallFailureReason,
    ) -> StrongMediaOpenError {
        StrongMediaOpenError::Terminal(MediaOpenTerminalOutcome::PlayerFailed {
            request_id: MediaOpenRequestId::from_non_zero(NonZeroU64::MIN),
            reason,
        })
    }

    fn local_target(path: &str) -> StartupInstallTarget {
        StartupInstallTarget::Local(StartupLocalTarget {
            path: PathBuf::from(path),
            sibling_discovery: StartupSiblingDiscovery::Skip,
        })
    }

    /// CLI/восстановленный локальный файл получает тот же текст, что и кнопка Open.
    #[test]
    fn local_startup_player_failure_names_file_and_reason() {
        let error =
            player_failed(crate::media_open::PlayerInstallFailureReason::NoSuitableVideoDecoder);
        let target = local_target("/home/private-parent-dir/movie.mkv");

        let texts = startup_install_failure_texts(&error, &target);

        assert_eq!(
            texts.user_message,
            "Не удалось открыть «movie.mkv»: нет подходящего видеодекодера (проверьте настройку декодера)"
        );
        assert_eq!(
            texts.row_summary,
            "Нет подходящего видеодекодера (проверьте настройку декодера)"
        );
        assert!(!texts.user_message.contains("private-parent-dir"));
    }

    /// Web-ссылка, которую player отказался установить: понятный текст с доменом
    /// вместо технического дампа (UX сессия 08), строка очереди — короткая причина.
    #[test]
    fn web_startup_player_failure_names_host_and_reason() {
        let error =
            player_failed(crate::media_open::PlayerInstallFailureReason::UnsupportedVideoFormat);

        let texts =
            startup_install_failure_texts(&error, &StartupInstallTarget::web(Some("example.test")));

        assert_eq!(
            texts.user_message,
            "Не удалось открыть ссылку (example.test): формат видео не поддерживается"
        );
        assert_eq!(texts.row_summary, "Формат видео не поддерживается");
    }

    /// Элемент startup-плейлиста (источник неизвестен) сохраняет прежний текст окна.
    #[test]
    fn playlist_entry_player_failure_keeps_previous_window_text() {
        let error =
            player_failed(crate::media_open::PlayerInstallFailureReason::UnsupportedVideoFormat);

        let texts = startup_install_failure_texts(&error, &StartupInstallTarget::PlaylistEntry);

        assert_eq!(texts.user_message, error.to_string());
        assert_eq!(texts.row_summary, "Формат видео не поддерживается");
    }

    /// Неклассифицированная ошибка не превращается в выдуманную причину для строки.
    #[test]
    fn unclassified_startup_failure_keeps_previous_texts() {
        let error = StrongMediaOpenError::MissingTerminal;
        let target = local_target("/videos/clip.mkv");

        let texts = startup_install_failure_texts(&error, &target);

        assert_eq!(
            texts.user_message,
            "Не удалось открыть «clip.mkv»: внутренняя ошибка плеера"
        );
        assert_eq!(texts.row_summary, error.to_string());
    }
}
