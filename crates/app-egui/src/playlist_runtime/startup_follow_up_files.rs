//! Остальные локальные файлы командной строки после установки первого (сессия 13).
//!
//! `fastiplayer a.mkv b.mkv c.mkv` открывается так же, как один CLI-файл: сохранённая
//! очередь остаётся запасной, пока первый файл не дошёл до точного `Installed`. В этот
//! момент startup-замена уже сделала очередь из одного первого файла, и здесь остальные
//! файлы встают за ним в порядке командной строки. Sibling discovery для набора файлов
//! не запускается: пользователь сам выбрал, что открыть.
//!
//! Владелец очереди — `PlaylistController`; модуль — тонкая граница `PlaylistRuntime`
//! над атомарным `commit_import_append` (та же граница, что у добавления импорта).
//! Append не трогает active/current/playback: играет по-прежнему первый файл.

use std::path::PathBuf;

use super::PlaylistRuntime;
use super::controller::{ControllerImportCommitError, ControllerImportCommitOutcome};
use super::identity::ActiveMediaIdentity;
use super::local_files_replacement::local_file_entry_draft;

/// Итог добавления остальных CLI-файлов: успех и каждая причина отказа раздельно.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StartupFollowUpFilesOutcome {
    /// Файлы добавлены в конец очереди после установленного первого файла.
    Appended {
        /// Сколько файлов добавлено.
        file_count: usize,
    },
    /// Добавлять нечего: очередь не тронута.
    NoFilesProvided,
    /// Состояние плейлиста ещё не загружено: очередь не тронута.
    LoadDecisionPending,
    /// Первый файл не закреплён строкой очереди (нечего продолжать): очередь не тронута.
    MissingInstalledTarget,
    /// Идёт другая установка media очередью: очередь не тронута.
    InstallInProgress,
    /// Runtime закрывается или owner отказал по инварианту/лимиту: очередь не тронута.
    Rejected,
}

impl PlaylistRuntime {
    /// Добавляет остальные CLI-файлы после уже установленного первого, сохраняя порядок.
    ///
    /// Вызывается startup owner-ом ровно один раз — сразу после точного `Installed`
    /// первого файла и до применения отложенных (retained) действий пользователя, чтобы
    /// более поздние действия пользователя по-прежнему побеждали.
    pub(crate) fn append_local_files_after_startup_target(
        &mut self,
        paths: Vec<PathBuf>,
    ) -> StartupFollowUpFilesOutcome {
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return StartupFollowUpFilesOutcome::Rejected;
        }
        if paths.is_empty() {
            return StartupFollowUpFilesOutcome::NoFilesProvided;
        }
        let Some(controller) = self.controller.as_mut() else {
            return StartupFollowUpFilesOutcome::LoadDecisionPending;
        };
        // Без закреплённого первого файла «остальные» превратились бы в отдельную очередь
        // без играющего элемента — такое состояние не создаём.
        if controller
            .active_media()
            .and_then(ActiveMediaIdentity::item_id)
            .is_none()
        {
            return StartupFollowUpFilesOutcome::MissingInstalledTarget;
        }
        let file_count = paths.len();
        let drafts = paths.into_iter().map(local_file_entry_draft).collect();
        let expected_revision = controller.view_snapshot().structural_revision();
        let dirty_before = controller.dirty_revision();
        let outcome = match controller.commit_import_append(expected_revision, drafts) {
            Ok(ControllerImportCommitOutcome::Committed { .. }) => {
                StartupFollowUpFilesOutcome::Appended { file_count }
            }
            Ok(ControllerImportCommitOutcome::NoEntriesProvided) => {
                StartupFollowUpFilesOutcome::NoFilesProvided
            }
            Err(ControllerImportCommitError::InstallInProgress) => {
                StartupFollowUpFilesOutcome::InstallInProgress
            }
            Err(error) => {
                tracing::warn!(?error, "Остальные файлы командной строки не добавлены");
                StartupFollowUpFilesOutcome::Rejected
            }
        };
        // Persistence публикуется только если очередь действительно изменилась.
        self.publish_controller_mutation_if_dirty(dirty_before);
        outcome
    }
}

#[cfg(test)]
mod tests;
