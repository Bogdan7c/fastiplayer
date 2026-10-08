//! Замена очереди несколькими локальными файлами одного броска (drag & drop на видео).
//!
//! Владелец очереди — `PlaylistController`; этот модуль — тонкая граница `PlaylistRuntime`:
//! собирает ID-less drafts в порядке броска, коммитит их атомарной controller-границей
//! `commit_import_replace` (та же, что у «Открыть как новый плейлист») и публикует
//! persistence. Подтверждение замены непустой очереди уже получено выше по цепочке:
//! сюда попадает только `AdmittedLocalFilesOpen`. Воспроизведение первого файла запускает
//! вызывающий код обычным Row Play: здесь media не открывается.

use std::path::PathBuf;

use playlist_core::{
    CachedPlaylistMetadata, LocalLocator, MAX_PLAYLIST_ITEMS, PlaylistEntryDraft,
    PlaylistItemDraft, PlaylistItemId, PlaylistMediaKind,
};

use super::PlaylistRuntime;
use super::controller::{
    ControllerImportCommitError, ControllerImportCommitOutcome, ImportReplacementDisposition,
};

/// Итог замены очереди файлами: различает успех и каждую причину отказа.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LocalFilesReplacementOutcome {
    /// Очередь заменена; `first_item` — первый файл броска, который надо играть.
    Replaced {
        /// Первый playable-элемент новой очереди.
        first_item: PlaylistItemId,
        /// Сколько файлов стало очередью.
        file_count: usize,
    },
    /// Список файлов пуст: очередь не тронута.
    NoFilesProvided,
    /// Файлов больше, чем помещается в плейлист: очередь не тронута.
    TooManyFiles,
    /// Состояние плейлиста ещё не загружено: очередь не тронута.
    LoadDecisionPending,
    /// Идёт установка media очередью: замена отложена, очередь не тронута.
    InstallInProgress,
    /// Runtime закрывается или owner отказал по инварианту: очередь не тронута.
    Rejected,
}

impl PlaylistRuntime {
    /// Решено ли, что делать с сохранённой очередью (load decision принят).
    ///
    /// До этого момента committed очереди ещё нет, и замена файлами невозможна.
    pub(crate) fn queue_load_decision_is_pending(&self) -> bool {
        self.controller.as_ref().is_none()
    }

    /// Заменяет очередь файлами в порядке броска без запуска воспроизведения.
    pub(crate) fn replace_queue_with_local_files(
        &mut self,
        paths: Vec<PathBuf>,
    ) -> LocalFilesReplacementOutcome {
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return LocalFilesReplacementOutcome::Rejected;
        }
        if paths.is_empty() {
            return LocalFilesReplacementOutcome::NoFilesProvided;
        }
        if paths.len() > MAX_PLAYLIST_ITEMS {
            return LocalFilesReplacementOutcome::TooManyFiles;
        }
        // Замена очереди отменяет поздние результаты старых add/import/sibling-задач.
        self.supersede_manual_add_queue_generation();
        self.supersede_playlist_import_flow();
        let Some(controller) = self.controller.as_mut() else {
            return LocalFilesReplacementOutcome::LoadDecisionPending;
        };
        let file_count = paths.len();
        let drafts = paths.into_iter().map(local_file_entry_draft).collect();
        let expected_revision = controller.view_snapshot().structural_revision();
        let dirty_before = controller.dirty_revision();
        let commit_result = controller.commit_import_replace(
            expected_revision,
            drafts,
            ImportReplacementDisposition::InteractiveDetached,
        );
        let outcome = match commit_result {
            Ok(ControllerImportCommitOutcome::Committed { allocated, .. }) => {
                match allocated.iter_playable_item_ids().next() {
                    Some(first_item) => LocalFilesReplacementOutcome::Replaced {
                        first_item,
                        file_count,
                    },
                    None => LocalFilesReplacementOutcome::Rejected,
                }
            }
            Ok(ControllerImportCommitOutcome::NoEntriesProvided) => {
                LocalFilesReplacementOutcome::NoFilesProvided
            }
            Err(ControllerImportCommitError::InstallInProgress) => {
                LocalFilesReplacementOutcome::InstallInProgress
            }
            Err(error) => {
                tracing::warn!(?error, "Замена очереди брошенными файлами отклонена");
                LocalFilesReplacementOutcome::Rejected
            }
        };
        self.publish_controller_mutation_if_dirty(dirty_before);
        outcome
    }
}

/// Один брошенный файл → ID-less строка очереди.
///
/// Метаданные — только имя файла как временный заголовок; настоящие заголовок и
/// длительность подтянет обычное обновление видимых строк, как у Manual Add.
/// Этим же черновиком пользуется добавление остальных файлов CLI после первого.
pub(super) fn local_file_entry_draft(path: PathBuf) -> PlaylistEntryDraft {
    let fallback_title = path
        .file_name()
        .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
        .into_owned();
    PlaylistEntryDraft::from(PlaylistItemDraft::local(
        LocalLocator::Native(path),
        None,
        CachedPlaylistMetadata::new(fallback_title, PlaylistMediaKind::Unknown),
    ))
}

#[cfg(test)]
mod tests;
