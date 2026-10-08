//! Несколько локальных файлов из командной строки при первом запуске (сессия 13).
//!
//! Семантика победителя/запасной очереди — та же, что у одного CLI-файла (S17):
//! - первый файл идёт прежним путём одного файла (target-first open, allocator gate,
//!   stepwise strong install); восстановленная очередь остаётся нетронутой запасной
//!   до точного `Installed` первого файла;
//! - после `Installed` startup-замена уже сделала очередь из первого файла, и остальные
//!   встают за ним в порядке командной строки (`PlaylistRuntime::append_local_files_after_startup_target`);
//! - sibling discovery для набора файлов не запускается (только для одного файла);
//! - ошибка первого файла до барьера — как у одного файла: запасная очередь, остальные
//!   файлы отбрасываются.
//!
//! Модуль хранит только «хвост» набора между разбором аргументов и `Installed`;
//! winner/fallback policy остаётся у `orchestration`, очередь — у `PlaylistRuntime`.

use std::path::PathBuf;

use playlist_discovery::LocalMediaKind;
use tracing::warn;

use super::orchestration::StartupMediaOrchestration;
use super::pending_install::StartupSiblingDiscovery;
use super::{InitialMedia, StartupMediaController};
use crate::playlist_runtime::{PlaylistRuntime, StartupFollowUpFilesOutcome};
use crate::startup_arguments_message::FOLLOW_UP_FILES_NOT_QUEUED_MESSAGE;
use crate::state::AppState;

/// Остальные CLI-файлы, ждущие `Installed` первого. Пусто — обычный запуск с одним файлом.
pub(super) struct CliFollowUpLocalFiles {
    /// Пути в порядке командной строки, байт-в-байт.
    paths: Vec<PathBuf>,
}

impl CliFollowUpLocalFiles {
    /// Пустой хвост: одиночный запуск или набор ещё не разобран.
    pub(super) const fn none() -> Self {
        Self { paths: Vec::new() }
    }
}

impl StartupMediaController {
    /// Разворачивает несколько аргументов в одну цель прежнего startup flow.
    ///
    /// Одиночные цели (`File`/`Playlist`/`Url`) возвращаются без изменений. Для набора:
    /// хвост файлов запоминается до `Installed` первого, уведомление о пропущенных
    /// аргументах показывается информационной плашкой.
    pub(super) fn accept_several_initial_arguments(
        &mut self,
        initial_media: InitialMedia,
        app_state: &mut AppState,
    ) -> InitialMedia {
        let InitialMedia::Several(several) = initial_media else {
            return initial_media;
        };
        let single = several.into_single_target();
        self.orchestration
            .hold_cli_follow_up_files(single.follow_up_files);
        if let Some(notice) = single.skipped_notice {
            app_state.notify_info(notice);
        }
        single.target
    }

    /// Ставит остальные CLI-файлы после установленного первого; сбой — инфо-плашка.
    ///
    /// Первый файл уже играет, поэтому отказ не фатален: пользователь видит, что очередь
    /// неполная, а причина уходит в лог.
    pub(super) fn queue_cli_follow_up_files_after_installed(
        paths: Vec<PathBuf>,
        app_state: &mut AppState,
        playlist_runtime: &mut PlaylistRuntime,
    ) {
        match playlist_runtime.append_local_files_after_startup_target(paths) {
            StartupFollowUpFilesOutcome::Appended { .. }
            | StartupFollowUpFilesOutcome::NoFilesProvided => {}
            outcome @ (StartupFollowUpFilesOutcome::LoadDecisionPending
            | StartupFollowUpFilesOutcome::MissingInstalledTarget
            | StartupFollowUpFilesOutcome::InstallInProgress
            | StartupFollowUpFilesOutcome::Rejected) => {
                warn!(
                    ?outcome,
                    "Остальные CLI-файлы не встали в очередь после первого"
                );
                app_state.notify_info(FOLLOW_UP_FILES_NOT_QUEUED_MESSAGE);
            }
        }
    }
}

impl StartupMediaOrchestration {
    /// Запоминает хвост набора файлов CLI до `Installed` первого.
    pub(super) fn hold_cli_follow_up_files(&mut self, paths: Vec<PathBuf>) {
        self.cli_follow_up_files = CliFollowUpLocalFiles { paths };
    }

    /// Что делать после `Installed` локального файла, установленного startup-ом.
    ///
    /// CLI с одним файлом — sibling discovery (как раньше); CLI с набором — добавить
    /// остальные файлы набора (хвост забирается ровно один раз); восстановленный
    /// элемент очереди — ничего.
    pub(super) fn local_install_follow_up(
        &mut self,
        is_cli: bool,
        media_kind: LocalMediaKind,
    ) -> StartupSiblingDiscovery {
        if !is_cli {
            return StartupSiblingDiscovery::Skip;
        }
        let follow_up_files = std::mem::take(&mut self.cli_follow_up_files.paths);
        if follow_up_files.is_empty() {
            StartupSiblingDiscovery::AfterInstall(media_kind)
        } else {
            StartupSiblingDiscovery::AppendCliFollowUpFiles(follow_up_files)
        }
    }
}

#[cfg(test)]
mod tests;
