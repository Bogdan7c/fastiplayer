//! Установка файла, выбранного кнопкой Open, без ожидания на UI-потоке (UX сессия 18).
//!
//! Раньше после подготовки файла вызывалась блокирующая обёртка strong-open, и окно не
//! перерисовывалось, пока player не ответит (до `staged_video_preflight_timeout`, 15 с).
//! Теперь Open входит в общий неблокирующий stepwise-путь (`begin_prepared_media_strong` /
//! `poll_prepared_media_strong`), как старт и плейлист, а этот модуль — его маленький владелец:
//! он помнит «свой» request и что сделать после `Installed`.
//!
//! Инварианты:
//! - слот `pending_strong_media_open` общий и единственный; владелец опрашивает его только
//!   пока там лежит именно его request, поэтому чужой terminal (старт, плейлист, VOD,
//!   переключение качества) он никогда не забирает;
//! - terminal своего request-а забирается ровно один раз: после `Installed`/`Failed` отметка
//!   снимается в том же опросе;
//! - протокол coordinator-а (Ready → authorize → barrier → Installed, compensation) не меняется:
//!   его целиком ведёт stepwise-путь, владелец только читает итог.

use std::path::PathBuf;

use playlist_discovery::LocalMediaKind;
use tracing::{error, info, warn};

use super::AppState;
use super::strong_media_open::{
    InstalledSingleMediaOpen, PreparedSingleMediaOpen, StrongMediaOpenError, StrongMediaOpenPoll,
    StrongMediaOpenUserOutcome,
};
use crate::local_open_message::{local_open_failure_message, local_open_preparing_message};
use crate::media_open::{ActiveMediaSource, MediaOpenRequestId, PlayerInstallFailureReason};
use crate::playlist_runtime::{PlaylistRuntime, StablePlaybackIntent};

/// Что сделать после успешной установки: то же, что раньше шло после блокирующего вызова.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LocalOpenInstallTarget {
    /// Путь выбранного файла: для поиска соседних файлов и текста ошибки (только имя).
    path: PathBuf,
    /// Тип файла из подготовки: sibling discovery ищет соседей того же вида.
    media_kind: LocalMediaKind,
    /// Автозапуск из настроек: файл ставится на паузе и запускается после начала очереди.
    desired_intent: StablePlaybackIntent,
}

impl LocalOpenInstallTarget {
    pub(super) const fn new(
        path: PathBuf,
        media_kind: LocalMediaKind,
        desired_intent: StablePlaybackIntent,
    ) -> Self {
        Self {
            path,
            media_kind,
            desired_intent,
        }
    }
}

/// Один шаг общего strong-open слота глазами владельца Open.
#[derive(Debug)]
pub(super) enum StrongOpenSlotPoll<Installed, Failure> {
    /// Player ещё не ответил; ждать на UI-потоке нельзя, опросим на следующем кадре.
    Pending,
    /// Транзакция дошла до точного `Installed` и уже не принадлежит слоту.
    Installed(Installed),
    /// Транзакция завершилась ошибкой (в том числе отменой или «занято»).
    Failed(Failure),
}

/// Доступ владельца к общему слоту: production — `AppState` + `PlaylistRuntime`.
///
/// Порт нужен, чтобы проверить правила владельца без настоящего renderer-а и player-а.
pub(super) trait StrongOpenSlotPort {
    type Installed;
    type Failure;

    /// Request, который сейчас лежит в общем слоте (`None` — слот пуст).
    fn pending_request_id(&self) -> Option<MediaOpenRequestId>;

    /// Неблокирующе продвигает транзакцию в слоте.
    fn poll(&mut self) -> StrongOpenSlotPoll<Self::Installed, Self::Failure>;
}

/// Итог одного опроса владельца.
#[derive(Debug)]
pub(super) enum LocalOpenInstallProgress<Installed, Failure> {
    /// Open сейчас ничего не устанавливает.
    Idle,
    /// Установка идёт; окно продолжает работать.
    Pending,
    /// Файл стал текущим media; осталось выполнить действия после установки.
    Installed {
        installed: Installed,
        target: LocalOpenInstallTarget,
    },
    /// Установка не удалась; что показать — решает `StrongMediaOpenError::user_outcome()`.
    Failed {
        error: Failure,
        target: LocalOpenInstallTarget,
    },
    /// В слоте уже не наш request: terminal потерян кем-то другим (нарушение инварианта).
    OwnershipLost { target: LocalOpenInstallTarget },
}

/// Отметка «этот request в общем слоте — установка из Open».
#[derive(Debug)]
struct TrackedLocalOpenInstall {
    request_id: MediaOpenRequestId,
    target: LocalOpenInstallTarget,
}

/// Владелец установки из Open: не больше одной за раз.
#[derive(Debug, Default)]
pub(super) struct LocalOpenInstallOwner {
    tracked: Option<TrackedLocalOpenInstall>,
}

impl LocalOpenInstallOwner {
    /// Идёт ли установка файла из Open.
    pub(super) const fn is_installing(&self) -> bool {
        self.tracked.is_some()
    }

    /// Файл, который сейчас устанавливается (для строки «Открываем «…»…»).
    pub(super) fn installing_path(&self) -> Option<&std::path::Path> {
        self.tracked
            .as_ref()
            .map(|tracked| tracked.target.path.as_path())
    }

    /// Принадлежит ли request установке из Open (нужно guard-у транспорта).
    pub(super) fn owns_request(&self, request_id: MediaOpenRequestId) -> bool {
        self.tracked
            .as_ref()
            .is_some_and(|tracked| tracked.request_id == request_id)
    }

    /// Запоминает только что начатую установку.
    ///
    /// Вызывающий обязан проверить `is_installing()` до начала: вторая установка из Open
    /// отклоняется раньше («Файл ещё открывается»), а общий слот и так не пустит второй request.
    /// Если инвариант всё же нарушен, прежняя отметка возвращается, а не теряется молча.
    pub(super) fn track(
        &mut self,
        request_id: MediaOpenRequestId,
        target: LocalOpenInstallTarget,
    ) -> Option<LocalOpenInstallTarget> {
        self.tracked
            .replace(TrackedLocalOpenInstall { request_id, target })
            .map(|previous| previous.target)
    }

    /// Неблокирующий опрос. Чужой request в слоте не трогается.
    pub(super) fn poll<Port: StrongOpenSlotPort>(
        &mut self,
        port: &mut Port,
    ) -> LocalOpenInstallProgress<Port::Installed, Port::Failure> {
        let Some(tracked) = self.tracked.take() else {
            return LocalOpenInstallProgress::Idle;
        };
        if port.pending_request_id() != Some(tracked.request_id) {
            return LocalOpenInstallProgress::OwnershipLost {
                target: tracked.target,
            };
        }
        match port.poll() {
            StrongOpenSlotPoll::Pending => {
                self.tracked = Some(tracked);
                LocalOpenInstallProgress::Pending
            }
            StrongOpenSlotPoll::Installed(installed) => LocalOpenInstallProgress::Installed {
                installed,
                target: tracked.target,
            },
            StrongOpenSlotPoll::Failed(error) => LocalOpenInstallProgress::Failed {
                error,
                target: tracked.target,
            },
        }
    }
}

/// Production-порт: общий слот `AppState` продвигается через stepwise strong-open.
struct AppStateStrongOpenSlot<'state> {
    app_state: &'state mut AppState,
    playlist_runtime: &'state mut PlaylistRuntime,
}

impl StrongOpenSlotPort for AppStateStrongOpenSlot<'_> {
    type Installed = Box<InstalledSingleMediaOpen>;
    type Failure = Box<StrongMediaOpenError>;

    fn pending_request_id(&self) -> Option<MediaOpenRequestId> {
        self.app_state.pending_prepared_media_strong_request_id()
    }

    fn poll(&mut self) -> StrongOpenSlotPoll<Self::Installed, Self::Failure> {
        match self
            .app_state
            .poll_prepared_media_strong(self.playlist_runtime)
        {
            StrongMediaOpenPoll::Pending => StrongOpenSlotPoll::Pending,
            StrongMediaOpenPoll::Installed(installed) => StrongOpenSlotPoll::Installed(installed),
            StrongMediaOpenPoll::Failed(error) => StrongOpenSlotPoll::Failed(error),
        }
    }
}

impl AppState {
    /// Начинает установку подготовленного файла из Open и сразу возвращает управление.
    ///
    /// Сам вызов не ждёт player: итог забирает `poll_local_open_install` на следующих кадрах.
    /// Пока идёт установка, видна строка «Открываем «…»…», выставленная при подготовке.
    pub(crate) fn begin_local_open_install(
        &mut self,
        prepared: crate::media_open::PreparedLocalOpenResult,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) {
        let path = prepared.source_path.clone();
        if self.local_open_install.is_installing() {
            // Обычно сюда не дойти: Open и внешние открытия во время установки отклоняются
            // до выбора файла. Строка прогресса возвращается к файлу, который реально ставится.
            info!("Установка из Open уже идёт; новый файл не устанавливается");
            if let Some(installing_path) = self.local_open_install.installing_path() {
                let progress_message = local_open_preparing_message(installing_path);
                self.set_startup_pending(progress_message);
            }
            self.notify_open_still_in_progress();
            return;
        }
        let target_draft =
            match crate::playlist_runtime::discovery::target_draft_from_prepared(&prepared) {
                Ok(target_draft) => target_draft,
                Err(error) => {
                    self.set_startup_error(format!(
                        "Не удалось подготовить metadata очереди для target: {error}"
                    ));
                    return;
                }
            };
        let desired_intent = if self.committed_config_snapshot.autoplay_for_new_media() {
            StablePlaybackIntent::Playing
        } else {
            StablePlaybackIntent::Paused
        };
        let target = LocalOpenInstallTarget::new(path.clone(), prepared.media_kind, desired_intent);
        let prepared_input = PreparedSingleMediaOpen::target_replacement(
            prepared.prepared_media,
            ActiveMediaSource::LocalFile(path.clone()),
            prepared.safe_label,
            target_draft,
        );
        // Файл ставится на паузе: автозапуск включит sibling discovery после начала очереди.
        if let Err(error) = self.begin_prepared_media_strong(
            playlist_runtime,
            renderer,
            prepared_input,
            player_core::PlaybackIntent::StartPaused,
        ) {
            self.report_prepared_local_install_failure(&path, &error);
            return;
        }
        let Some(request_id) = self.pending_prepared_media_strong_request_id() else {
            // begin при успехе всегда кладёт транзакцию в слот; без неё опрашивать нечего.
            error!("Stepwise strong-open начат, но слот пуст: установка из Open потеряна");
            self.set_startup_error(local_open_failure_message(
                &path,
                PlayerInstallFailureReason::InternalError,
            ));
            return;
        };
        if self.local_open_install.track(request_id, target).is_some() {
            error!("Установка из Open заменила прежнюю отметку: прежний terminal не наш");
        }
    }

    /// Неблокирующе продвигает установку из Open. Возвращает `true`, если что-то изменилось.
    pub(super) fn poll_local_open_install(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
    ) -> bool {
        // Владелец временно вынимается, чтобы порт мог занять `AppState` целиком.
        let mut owner = std::mem::take(&mut self.local_open_install);
        let progress = owner.poll(&mut AppStateStrongOpenSlot {
            app_state: self,
            playlist_runtime,
        });
        self.local_open_install = owner;
        match progress {
            LocalOpenInstallProgress::Idle | LocalOpenInstallProgress::Pending => false,
            LocalOpenInstallProgress::Installed { installed, target } => {
                self.finish_installed_local_open(&installed, target, playlist_runtime);
                true
            }
            LocalOpenInstallProgress::Failed { error, target } => {
                self.report_prepared_local_install_failure(&target.path, &error);
                true
            }
            LocalOpenInstallProgress::OwnershipLost { target } => {
                error!("Установка из Open потеряла свой request в общем strong-open слоте");
                self.set_startup_error(local_open_failure_message(
                    &target.path,
                    PlayerInstallFailureReason::InternalError,
                ));
                true
            }
        }
    }

    /// Идёт ли установка из Open (для «Файл ещё открывается» и планировщика опроса).
    pub(super) const fn is_local_open_installing(&self) -> bool {
        self.local_open_install.is_installing()
    }

    /// Принадлежит ли request установке из Open.
    pub(super) fn local_open_install_owns_request(&self, request_id: MediaOpenRequestId) -> bool {
        self.local_open_install.owns_request(request_id)
    }

    /// Действия после `Installed` в прежнем порядке: источник, затем соседние файлы и автозапуск.
    fn finish_installed_local_open(
        &mut self,
        installed: &InstalledSingleMediaOpen,
        target: LocalOpenInstallTarget,
        playlist_runtime: &mut PlaylistRuntime,
    ) {
        // Публикует источник и снимает «Открываем «…»…»/старую ошибку.
        self.record_installed_media(installed);
        if let Err(error) = playlist_runtime.start_sibling_discovery_then_play_from_beginning(
            target.path,
            target.media_kind,
            target.desired_intent,
        ) {
            warn!(error = %error, "Target установлен, но sibling discovery не запущен");
        }
    }

    /// Сообщает пользователю, почему подготовленный локальный файл не установился.
    ///
    /// Причина берётся из типизированного исхода (сессия 03); отмена — не ошибка, «занято» —
    /// временное уведомление. Старое воспроизведение при отказе до commit barrier-а не
    /// затронуто, а ошибки после barrier-а уже обработала compensation strong-open-а.
    fn report_prepared_local_install_failure(
        &mut self,
        path: &std::path::Path,
        error: &StrongMediaOpenError,
    ) {
        // Имя файла — только в тексте для пользователя, в лог его не пишем.
        match error.user_outcome() {
            StrongMediaOpenUserOutcome::Cancelled => {
                info!(error = %error, "Установка локального файла отменена");
                // Например, Next/Stop во время открытия: строка «Открываем…» больше не нужна.
                self.finish_local_open_progress();
            }
            StrongMediaOpenUserOutcome::Busy => {
                info!(error = %error, "Coordinator занят другим открытием");
                self.finish_local_open_progress();
                self.notify_open_still_in_progress();
            }
            StrongMediaOpenUserOutcome::Failed(reason) => {
                warn!(error = %error, ?reason, "Не удалось установить подготовленный файл");
                self.set_startup_error(local_open_failure_message(path, reason));
            }
        }
    }

    /// Убирает строку прогресса открытия без сообщения об ошибке.
    fn finish_local_open_progress(&mut self) {
        self.startup_pending = None;
        self.mark_pending_worker_redraw();
    }
}

#[cfg(test)]
#[path = "local_open_install/tests.rs"]
mod tests;
