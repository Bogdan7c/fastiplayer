//! Неблокирующая пересборка media после смены настроек (задача C2 плана ux-edge-cases-2).
//!
//! Раньше Apply в настройках вызывал `install_prepared_media_strong` и блокирующий `recv()`
//! прямо на UI-потоке: окно не перерисовывалось до 15 секунд. Теперь этот модуль — маленький
//! владелец одной пересборки, продвигаемой по кадрам (по образцу `local_open_install` сессии 18):
//!
//! ```text
//! Preparing   фоновый поток готовит источник (prepare_local_file / web / yt-dlp)
//!   -> Installing    общий слот stepwise strong-open (Ready -> authorize -> Installed)
//!   -> RestoringState receipt восстановления позиции/дорожек/громкости
//!   -> AppRouteApplyResult (Applied / Failed / PartialFailure / RuntimeBusy)
//! ```
//!
//! Инварианты:
//! - UI-поток не блокируется ни на подготовке, ни на `recv()`, ни на receipt-е восстановления;
//! - слот `pending_strong_media_open` общий: владелец опрашивает его только пока там лежит
//!   именно его request, чужой terminal он не забирает;
//! - результат забирается ровно один раз: после `Finished` владелец пуст;
//! - протокол coordinator-а (Ready -> authorize -> barrier -> Installed) не меняется: его целиком
//!   ведёт stepwise-путь, владелец только читает итог;
//! - различие «доказанный сбой до barrier-а» / «могло пересечь barrier» сохраняется
//!   (`media_reconfigure_install_failure`): от него зависит, нужен ли откат.

use player_core::MediaInstallVideoBackendConstraint;
use render_wgpu_shell::Renderer;

use super::*;
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::PlaylistRuntime;
use crate::settings_runtime::MediaRebuildProgress;

mod prepare;
mod restore;

use prepare::{PrepareJobPoll, SettingsMediaPrepareJob};
pub(crate) use prepare::{RebuildPrepareRequest, RebuildSourceInput, prepare_rebuild_source};
use restore::{PendingPlaybackRestore, PlaybackRestorePoll};

/// Сохраняет различие между доказанным pre-barrier failure и mutation, требующей rollback.
pub(crate) fn media_reconfigure_install_failure(
    error: StrongMediaOpenError,
) -> AppRouteApplyResult {
    tracing::warn!(%error, "Reassembly media when changing settings did not reach the installation in the player");
    let message = crate::settings_rebuild_message::install_rebuild_failed_message();
    if error.may_have_crossed_install_barrier() {
        AppRouteApplyResult::PartialFailure { message }
    } else {
        AppRouteApplyResult::Failed { message }
    }
}

/// Задание пересборки: что готовить и с какими параметрами ставить.
pub(crate) struct SettingsMediaRebuildStart {
    /// Подготовка источника (выполняется в фоновом потоке).
    pub(crate) prepare: RebuildPrepareRequest,
    /// Окно воспроизведения (CUE-трек и т.п.), которое нужно сохранить у нового источника.
    pub(crate) playback_window: Option<player_core::MediaPlaybackWindow>,
    /// Целевая политика backend-а: берётся из ещё не записанного документа настроек.
    pub(crate) backend_constraint: MediaInstallVideoBackendConstraint,
}

/// Параметры установки, которые снимаются в момент начала установки, а не Apply.
struct RebuildContext {
    playback_window: Option<player_core::MediaPlaybackWindow>,
    backend_constraint: MediaInstallVideoBackendConstraint,
    /// Controls старого media, снятые перед установкой (подготовка может идти секундами,
    /// поэтому позиция берётся не в момент нажатия Apply).
    snapshot: Arc<PlayerSnapshot>,
}

/// Фаза одной пересборки.
enum RebuildPhase {
    Preparing(SettingsMediaPrepareJob<PreparedSingleMediaOpen>),
    Installing { request_id: MediaOpenRequestId },
    RestoringState(PendingPlaybackRestore),
}

/// Одна пересборка в полёте.
struct ActiveRebuild {
    context: RebuildContext,
    phase: RebuildPhase,
}

/// Результат одного шага автомата.
enum RebuildStep {
    /// Фаза сменилась, можно сразу продолжать в этом же опросе.
    Continue(RebuildPhase),
    /// Ждём; фаза возвращается владельцу без изменений.
    Waiting(RebuildPhase),
    /// Пересборка завершена.
    Finished(AppRouteApplyResult),
}

/// Владелец не больше одной пересборки за раз.
#[derive(Default)]
pub(super) struct SettingsMediaRebuildOwner {
    active: Option<ActiveRebuild>,
}

impl AppState {
    /// Идёт ли пересборка (для планировщика опроса и защиты от второй пересборки).
    pub(crate) const fn has_settings_media_rebuild(&self) -> bool {
        self.settings_media_rebuild.active.is_some()
    }

    /// Запускает фоновую подготовку и сразу возвращает управление.
    pub(crate) fn begin_settings_media_rebuild(
        &mut self,
        start: SettingsMediaRebuildStart,
    ) -> MediaRebuildProgress {
        if self.settings_media_rebuild.active.is_some() {
            return MediaRebuildProgress::Finished(AppRouteApplyResult::RuntimeBusy {
                activity: fastiplayer_settings::SettingsBoundaryActivity::PipelineLifecycle,
            });
        }
        let job = match SettingsMediaPrepareJob::spawn(start.prepare) {
            Ok(job) => job,
            Err(message) => {
                return MediaRebuildProgress::Finished(AppRouteApplyResult::Failed { message });
            }
        };
        self.settings_media_rebuild.active = Some(ActiveRebuild {
            context: RebuildContext {
                playback_window: start.playback_window,
                backend_constraint: start.backend_constraint,
                snapshot: self.last_player_snapshot.clone(),
            },
            phase: RebuildPhase::Preparing(job),
        });
        MediaRebuildProgress::Pending
    }

    /// Неблокирующе продвигает пересборку. Без активной пересборки возвращает ошибку
    /// контракта (вызывающий опрашивает только то, что сам начал).
    pub(crate) fn poll_settings_media_rebuild(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
    ) -> MediaRebuildProgress {
        let Some(mut active) = self.settings_media_rebuild.active.take() else {
            tracing::error!("Polled a media rebuild without an active rebuild");
            return MediaRebuildProgress::Finished(AppRouteApplyResult::Failed {
                message: crate::settings_rebuild_message::install_rebuild_failed_message(),
            });
        };
        let mut phase = active.phase;
        loop {
            match self.step_settings_media_rebuild(
                phase,
                &mut active.context,
                playlist_runtime,
                renderer,
            ) {
                RebuildStep::Continue(next_phase) => phase = next_phase,
                RebuildStep::Waiting(waiting_phase) => {
                    active.phase = waiting_phase;
                    self.settings_media_rebuild.active = Some(active);
                    return MediaRebuildProgress::Pending;
                }
                RebuildStep::Finished(result) => return MediaRebuildProgress::Finished(result),
            }
        }
    }

    /// Отмена на lifecycle-границе (suspend/shutdown): AppState и слот уничтожаются следом.
    ///
    /// Подготовка прерывается cooperative-токеном; установка в слоте получает lossless cancel
    /// (после enqueue у owner-а она всё равно обязана дойти до Installed — правило coordinator-а).
    pub(crate) fn cancel_settings_media_rebuild(&mut self, playlist_runtime: &mut PlaylistRuntime) {
        let Some(active) = self.settings_media_rebuild.active.take() else {
            return;
        };
        match active.phase {
            RebuildPhase::Preparing(mut job) => job.cancel(),
            RebuildPhase::Installing { .. } => {
                if let Err(error) = self.supersede_pending_prepared_media_strong(playlist_runtime) {
                    tracing::warn!(%error, "Failed to cancel the installation of reassembled media");
                }
            }
            RebuildPhase::RestoringState(_) => {}
        }
    }

    /// Один шаг автомата пересборки.
    fn step_settings_media_rebuild(
        &mut self,
        phase: RebuildPhase,
        context: &mut RebuildContext,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
    ) -> RebuildStep {
        match phase {
            RebuildPhase::Preparing(mut job) => match job.poll() {
                PrepareJobPoll::Running => RebuildStep::Waiting(RebuildPhase::Preparing(job)),
                PrepareJobPoll::Finished(Err(message)) => {
                    RebuildStep::Finished(AppRouteApplyResult::Failed { message })
                }
                PrepareJobPoll::Finished(Ok(prepared)) => {
                    self.begin_rebuild_install(prepared, context, playlist_runtime, renderer)
                }
            },
            RebuildPhase::Installing { request_id } => {
                self.poll_rebuild_install(request_id, context, playlist_runtime)
            }
            RebuildPhase::RestoringState(restore) => match restore.poll() {
                PlaybackRestorePoll::Pending => {
                    RebuildStep::Waiting(RebuildPhase::RestoringState(restore))
                }
                PlaybackRestorePoll::Applied => RebuildStep::Finished(AppRouteApplyResult::Applied),
                PlaybackRestorePoll::Failed(message) => {
                    RebuildStep::Finished(AppRouteApplyResult::PartialFailure { message })
                }
            },
        }
    }

    /// Подготовка готова: ставим источник через общий stepwise strong-open.
    fn begin_rebuild_install(
        &mut self,
        prepared: PreparedSingleMediaOpen,
        context: &mut RebuildContext,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &Renderer,
    ) -> RebuildStep {
        // Controls снимаются только сейчас: пока готовился источник, воспроизведение шло.
        context.snapshot = self.refresh_player_snapshot();
        let desired_intent = playback_intent_from_snapshot(&context.snapshot);
        let prepared = prepared.with_playback_window(context.playback_window);
        if let Err(error) = self.begin_prepared_media_strong_with_backend_constraint(
            playlist_runtime,
            renderer,
            prepared,
            desired_intent,
            context.backend_constraint.clone(),
        ) {
            return RebuildStep::Finished(media_reconfigure_install_failure(error));
        }
        match self.pending_prepared_media_strong_request_id() {
            Some(request_id) => RebuildStep::Continue(RebuildPhase::Installing { request_id }),
            None => {
                // begin при успехе всегда кладёт транзакцию в слот; без неё опрашивать нечего.
                tracing::error!(
                    "Stepwise strong-open started, but the slot is empty: reassembly lost"
                );
                RebuildStep::Finished(AppRouteApplyResult::Failed {
                    message: crate::settings_rebuild_message::install_rebuild_failed_message(),
                })
            }
        }
    }

    /// Опрашивает общий слот; чужой request не трогает.
    fn poll_rebuild_install(
        &mut self,
        request_id: MediaOpenRequestId,
        context: &RebuildContext,
        playlist_runtime: &mut PlaylistRuntime,
    ) -> RebuildStep {
        if self.pending_prepared_media_strong_request_id() != Some(request_id) {
            tracing::error!("Rebuild media lost its request in the overall strong-open slot");
            return RebuildStep::Finished(AppRouteApplyResult::Failed {
                message: crate::settings_rebuild_message::install_rebuild_failed_message(),
            });
        }
        match self.poll_prepared_media_strong(playlist_runtime) {
            StrongMediaOpenPoll::Pending => {
                RebuildStep::Waiting(RebuildPhase::Installing { request_id })
            }
            StrongMediaOpenPoll::Failed(error) => {
                RebuildStep::Finished(media_reconfigure_install_failure(*error))
            }
            StrongMediaOpenPoll::Installed(installed) => {
                self.record_installed_media(&installed);
                match self.dispatch_playback_restore(&context.snapshot, &installed) {
                    Ok(restore) => RebuildStep::Continue(RebuildPhase::RestoringState(restore)),
                    Err(message) => {
                        RebuildStep::Finished(AppRouteApplyResult::PartialFailure { message })
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "settings_media_rebuild/tests.rs"]
mod tests;
