//! Сбор очереди из брошенных в окно файлов и папок (drag & drop, сессия 12).
//!
//! Владелец: `PlaylistRuntime` (process-lifetime). Модуль делает две вещи:
//!
//! 1. [`collect_dropped_files`] — чистая функция: раскрывает брошенные элементы в плоский
//!    список файлов (папки — рекурсивно через `playlist_discovery::walk_media_folder`,
//!    порядок броска сохраняется, папка раскрывается на своём месте);
//! 2. [`DroppedCollectionWalkOwner`] — фоновый поток с mailbox-ом и wake-портом: UI-поток
//!    никогда не ходит по диску. Результат забирает UI-поток через
//!    `PlaylistRuntime::take_dropped_collection_walk_completion`.
//!
//! # Осознанное исключение из инварианта S14A (решение владельца, 7 октября 2026)
//!
//! S14A требует «никакого I/O до подтверждения замены очереди». Для брошенных ПАПОК
//! владелец выбрал порядок «сначала обход, потом подтверждение», чтобы в вопросе можно было
//! показать число найденных файлов. Исключение узкое: до Confirm выполняется только
//! перечисление каталога (`read_dir`/`file_type`); demux, подготовка media, discovery и
//! любые изменения очереди остаются строго после подтверждения.
//!
//! # Защита от устаревшего результата
//!
//! Результат обхода применяется на UI-потоке не сразу, а через владельцев состояния:
//! очередь и admission замены проверяются в момент применения, а не в момент броска.
//! Дополнительно: shutdown отменяет обход (`CancellationToken`), а отменённый
//! результат превращается в `Cancelled` и молча отбрасывается; если к моменту применения
//! уже идёт другое открытие, применяющий код отбрасывает результат с «Файл ещё открывается».

use std::fmt;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::thread::{self, JoinHandle};

use media_source_open::local_media::has_supported_local_media_extension;
use playlist_discovery::{
    FolderWalkError, FolderWalkLimits, FolderWalkRequest, FolderWalkTruncation, walk_media_folder,
};
use source_core::CancellationToken;

use super::PlaylistRuntime;
use crate::app_wake::{AppWakePort, CompletionPublishError, OwnerMailboxReceiver, owner_mailbox};
use crate::process_shutdown::{
    FinishedThreadJoin, ProcessOwnerShutdownOutcome, ShutdownDeadline, join_finished_thread,
    join_thread_until,
};

#[cfg(test)]
mod tests;

/// Один элемент брошенного набора в порядке броска.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum DroppedCollectionEntry {
    /// Файл: берётся как есть (тип проверит обычный путь открытия).
    File(PathBuf),
    /// Папка: раскрывается рекурсивно в media-файлы.
    Folder(PathBuf),
}

impl fmt::Debug for DroppedCollectionEntry {
    /// Пути в логи не попадают: показывается только вид элемента.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(_) => formatter.write_str("DroppedCollectionEntry::File(<redacted>)"),
            Self::Folder(_) => formatter.write_str("DroppedCollectionEntry::Folder(<redacted>)"),
        }
    }
}

/// Что сделать с найденными файлами (решает место броска).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DroppedCollectionDestination {
    /// Новая очередь из файлов (бросок на видео).
    ReplaceQueue,
    /// Добавить в конец очереди (бросок на панель плейлиста).
    AppendToQueue,
}

/// Бюджеты обхода, снятые с committed config в момент броска.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DroppedCollectionLimits {
    /// Сколько файлов максимум берётся из всего броска.
    pub(crate) max_files: NonZeroUsize,
    /// Глубина подпапок (`0` — только сама брошенная папка).
    pub(crate) max_subfolder_depth: usize,
}

/// Откуда набор: нужно только для безопасной подписи подтверждения (имя папки без пути).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DroppedCollectionSource {
    /// Имя первой брошенной папки (только последний компонент пути).
    pub(crate) first_folder_name: String,
    /// Сколько ещё элементов брошено кроме первой папки.
    pub(crate) other_item_count: usize,
}

impl DroppedCollectionSource {
    /// Строит описание по брошенным элементам; `None`, если папок среди них нет.
    pub(crate) fn from_entries(entries: &[DroppedCollectionEntry]) -> Option<Self> {
        let first_folder = entries.iter().find_map(|entry| match entry {
            DroppedCollectionEntry::Folder(path) => Some(path),
            DroppedCollectionEntry::File(_) => None,
        })?;
        let first_folder_name = first_folder.file_name().map_or_else(
            || "папка".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        Some(Self {
            first_folder_name,
            other_item_count: entries.len() - 1,
        })
    }
}

/// Почему результат неполный.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DroppedCollectionTruncation {
    /// Достигнут лимит файлов: оставлены первые по порядку.
    FileLimit,
    /// Подпапки глубже лимита не обходились.
    DepthLimit,
}

/// Итог сбора: файлы в порядке броска и всё, что нужно для сообщений пользователю.
pub(crate) struct CollectedDroppedFiles {
    /// Что сделать с файлами.
    pub(crate) destination: DroppedCollectionDestination,
    /// Файлы в итоговом порядке.
    pub(crate) files: Vec<PathBuf>,
    /// Причина неполноты, если она есть.
    pub(crate) truncation: Option<DroppedCollectionTruncation>,
    /// Сколько папок/записей пропущено из-за ошибок чтения.
    pub(crate) unreadable_count: usize,
    /// Описание папок для подписи подтверждения; `None`, если папок не было.
    pub(crate) source: Option<DroppedCollectionSource>,
}

impl fmt::Debug for CollectedDroppedFiles {
    /// Печатает только счётчики: пути и имена папок в лог не уходят.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CollectedDroppedFiles")
            .field("destination", &self.destination)
            .field("file_count", &self.files.len())
            .field("truncation", &self.truncation)
            .field("unreadable_count", &self.unreadable_count)
            .finish()
    }
}

/// Обход отменён владельцем (shutdown): частичный результат не нужен.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DroppedCollectionCancelled;

/// Раскрывает брошенные элементы в плоский список файлов (чистая, отменяемая функция).
///
/// Порядок: ровно порядок броска, папка раскрывается на месте (внутри — файлы раньше
/// подпапок, natural-порядок). Лимит файлов общий на весь бросок. Нечитаемая папка не
/// роняет обход: она пропускается и считается в `unreadable_count`.
pub(crate) fn collect_dropped_files(
    entries: &[DroppedCollectionEntry],
    destination: DroppedCollectionDestination,
    limits: DroppedCollectionLimits,
    cancellation: &CancellationToken,
) -> Result<CollectedDroppedFiles, DroppedCollectionCancelled> {
    let max_files = limits.max_files.get();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut truncation: Option<DroppedCollectionTruncation> = None;
    let mut unreadable_count = 0usize;

    for entry in entries {
        if cancellation.is_cancelled() {
            return Err(DroppedCollectionCancelled);
        }
        match entry {
            DroppedCollectionEntry::File(path) => {
                if files.len() >= max_files {
                    truncation = Some(DroppedCollectionTruncation::FileLimit);
                    break;
                }
                files.push(path.clone());
            }
            DroppedCollectionEntry::Folder(root) => {
                let remaining_budget = max_files - files.len();
                // Walker требует бюджет >= 1. Если бюджет уже исчерпан, обходим с бюджетом 1
                // как «щуп»: найденный файл означает, что лимит реально отсёк содержимое.
                let walk_budget =
                    NonZeroUsize::new(remaining_budget.max(1)).unwrap_or(NonZeroUsize::MIN);
                let walk_limits = FolderWalkLimits::new(walk_budget, limits.max_subfolder_depth);
                let request = FolderWalkRequest::new(
                    root,
                    walk_limits,
                    cancellation,
                    has_supported_local_media_extension,
                );
                match walk_media_folder(request) {
                    Ok(outcome) => {
                        unreadable_count +=
                            outcome.unreadable_directories + outcome.unreadable_entries;
                        if remaining_budget == 0 {
                            if !outcome.files.is_empty() {
                                truncation = Some(DroppedCollectionTruncation::FileLimit);
                                break;
                            }
                            continue;
                        }
                        files.extend(outcome.files);
                        match outcome.truncation {
                            Some(FolderWalkTruncation::FileLimit) => {
                                truncation = Some(DroppedCollectionTruncation::FileLimit);
                                break;
                            }
                            Some(FolderWalkTruncation::DepthLimit) => {
                                truncation.get_or_insert(DroppedCollectionTruncation::DepthLimit);
                            }
                            None => {}
                        }
                    }
                    Err(FolderWalkError::Cancelled) => return Err(DroppedCollectionCancelled),
                    Err(FolderWalkError::ReadRoot(kind)) => {
                        // Путь в лог не пишем: только вид ошибки.
                        tracing::debug!(?kind, "Брошенная папка не читается, пропущена");
                        unreadable_count += 1;
                    }
                }
            }
        }
    }

    Ok(CollectedDroppedFiles {
        destination,
        files,
        truncation,
        unreadable_count,
        source: DroppedCollectionSource::from_entries(entries),
    })
}

/// Terminal result фонового обхода.
#[derive(Debug)]
pub(crate) enum DroppedCollectionWalkCompletion {
    /// Обход завершён (возможно, усечён).
    Collected(CollectedDroppedFiles),
    /// Обход отменён (shutdown): результат не применяется.
    Cancelled,
    /// Рабочий поток упал; очередь не тронута.
    WorkerFailed,
}

/// Почему обход не стартовал.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DroppedCollectionWalkStartError {
    /// Runtime закрывает admission (shutdown).
    RuntimeClosed,
    /// Предыдущий обход ещё не завершён и не забран.
    AlreadyInFlight,
    /// Поток не удалось создать.
    SpawnFailed,
}

/// Один фоновый обход: mailbox, поток и токен отмены.
struct DroppedCollectionWalkJob {
    mailbox: OwnerMailboxReceiver<(), DroppedCollectionWalkCompletion>,
    join_handle: Option<JoinHandle<()>>,
    cancellation: CancellationToken,
}

/// Process-lifetime владелец единственного фонового обхода брошенных папок.
pub(super) struct DroppedCollectionWalkOwner {
    wake_port: AppWakePort,
    job: Option<DroppedCollectionWalkJob>,
}

impl DroppedCollectionWalkOwner {
    pub(super) fn new(wake_port: AppWakePort) -> Self {
        Self {
            wake_port,
            job: None,
        }
    }

    /// Идёт ли обход или его результат ещё не забран UI.
    pub(super) fn is_in_flight(&self) -> bool {
        self.job.is_some()
    }

    /// Помечает идущий обход устаревшим: его результат больше никогда не будет применён.
    ///
    /// Поток не убивается и не ждётся — он сам остановится по токену, а `take_completion`
    /// превратит любой его итог (в том числе уже опубликованный) в `Cancelled`.
    pub(super) fn cancel_active(&mut self) {
        if let Some(job) = self.job.as_ref() {
            job.cancellation.cancel();
        }
    }

    /// Запускает обход в отдельном потоке; второй одновременный обход запрещён.
    fn start(
        &mut self,
        entries: Vec<DroppedCollectionEntry>,
        destination: DroppedCollectionDestination,
        limits: DroppedCollectionLimits,
    ) -> Result<(), DroppedCollectionWalkStartError> {
        if self.job.is_some() {
            return Err(DroppedCollectionWalkStartError::AlreadyInFlight);
        }
        let (publisher, mailbox) = owner_mailbox(self.wake_port.clone());
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let join_handle = thread::Builder::new()
            .name("dropped-folder-walk".to_owned())
            .spawn(move || {
                let completion = match collect_dropped_files(
                    &entries,
                    destination,
                    limits,
                    &worker_cancellation,
                ) {
                    Ok(collected) => DroppedCollectionWalkCompletion::Collected(collected),
                    Err(DroppedCollectionCancelled) => DroppedCollectionWalkCompletion::Cancelled,
                };
                if let Err(CompletionPublishError::AlreadyPublished) =
                    publisher.publish_completion(completion)
                {
                    tracing::error!("Обход брошенных папок опубликовал второй terminal result");
                }
            })
            .map_err(|error| {
                tracing::warn!(%error, "Не удалось запустить поток обхода папок");
                DroppedCollectionWalkStartError::SpawnFailed
            })?;
        self.job = Some(DroppedCollectionWalkJob {
            mailbox,
            join_handle: Some(join_handle),
            cancellation,
        });
        Ok(())
    }

    /// Неблокирующе забирает terminal result (ровно один раз).
    fn take_completion(&mut self) -> Option<DroppedCollectionWalkCompletion> {
        let job = self.job.as_mut()?;
        let published = job.mailbox.drain().completion;
        let completion = match published {
            Some(completion) => {
                // Поток завершается сразу после публикации: join короткий.
                let joined = job.join_handle.take().map(JoinHandle::join).transpose();
                match joined {
                    Ok(_) => completion,
                    Err(_) => DroppedCollectionWalkCompletion::WorkerFailed,
                }
            }
            None => match join_finished_thread(&mut job.join_handle) {
                FinishedThreadJoin::StillRunning => return None,
                // Поток мог опубликовать результат между drain и проверкой завершения.
                FinishedThreadJoin::Joined
                | FinishedThreadJoin::AlreadyJoined
                | FinishedThreadJoin::Panicked => job
                    .mailbox
                    .drain()
                    .completion
                    .unwrap_or(DroppedCollectionWalkCompletion::WorkerFailed),
            },
        };
        let completion = if job.cancellation.is_cancelled() {
            DroppedCollectionWalkCompletion::Cancelled
        } else {
            completion
        };
        self.job = None;
        Some(completion)
    }

    /// Отменяет обход и ждёт поток до общего deadline процесса.
    pub(super) fn shutdown_until(
        &mut self,
        deadline: ShutdownDeadline,
    ) -> ProcessOwnerShutdownOutcome {
        let Some(job) = self.job.as_mut() else {
            return ProcessOwnerShutdownOutcome::Completed;
        };
        job.cancellation.cancel();
        let outcome = match join_thread_until(&mut job.join_handle, deadline) {
            FinishedThreadJoin::AlreadyJoined | FinishedThreadJoin::Joined => {
                ProcessOwnerShutdownOutcome::Completed
            }
            FinishedThreadJoin::StillRunning => {
                ProcessOwnerShutdownOutcome::TimedOut { pending_threads: 1 }
            }
            FinishedThreadJoin::Panicked => ProcessOwnerShutdownOutcome::ThreadPanicked {
                panicked_threads: 1,
                pending_threads: 0,
            },
        };
        if !matches!(outcome, ProcessOwnerShutdownOutcome::TimedOut { .. }) {
            self.job = None;
        }
        outcome
    }
}

impl PlaylistRuntime {
    /// Бюджеты обхода из committed playlist config (на момент вызова).
    pub(crate) fn dropped_collection_limits(&self) -> DroppedCollectionLimits {
        let committed = self.settings.committed();
        DroppedCollectionLimits {
            max_files: NonZeroUsize::new(usize::from(committed.dropped_folder_max_files))
                .unwrap_or(NonZeroUsize::MIN),
            max_subfolder_depth: usize::from(committed.dropped_folder_max_depth),
        }
    }

    /// Настройка «что делать с брошенным файлом плейлиста» из committed playlist config.
    pub(crate) fn dropped_playlist_file_action(
        &self,
    ) -> fastiplayer_config::DroppedPlaylistFileAction {
        self.settings.committed().dropped_playlist_file_action
    }

    /// Явное открытие/импорт вытесняет идущий обход папки: его результат будет отброшен.
    pub(in crate::playlist_runtime) fn supersede_dropped_collection_walk(&mut self) {
        self.dropped_collection_walk.cancel_active();
    }

    /// Идёт ли обход брошенных папок (новое открытие должно подождать).
    pub(crate) fn has_dropped_collection_walk_in_flight(&self) -> bool {
        self.dropped_collection_walk.is_in_flight()
    }

    /// Запускает фоновый обход; очередь и disk I/O UI-потока не затрагиваются.
    pub(crate) fn start_dropped_collection_walk(
        &mut self,
        entries: Vec<DroppedCollectionEntry>,
        destination: DroppedCollectionDestination,
    ) -> Result<(), DroppedCollectionWalkStartError> {
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(DroppedCollectionWalkStartError::RuntimeClosed);
        }
        let limits = self.dropped_collection_limits();
        self.dropped_collection_walk
            .start(entries, destination, limits)
    }

    /// Забирает завершённый обход (неблокирующе); `None`, пока он идёт или не запускался.
    pub(crate) fn take_dropped_collection_walk_completion(
        &mut self,
    ) -> Option<DroppedCollectionWalkCompletion> {
        self.dropped_collection_walk.take_completion()
    }
}
