//! Открытие файла: первый запрос → размер → проверка раскладки → окно предзагрузки
//! → демуксер. Всё идёт в своём потоке; `poll_open` ничего не блокирует.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use demux_api::{DemuxHints, DemuxInput};
use media_core::Demuxer;
use media_prefetch::PrefetchingByteSource;
use source_core::CancellationToken;

use super::core::{FileCore, OpenInterrupted};
use super::error::PushedFileError;
use super::layout::{LayoutProblem, check_movie_layout};
use super::policy::PushedFilePolicy;
use super::sealed_end::SealAwareDemuxer;
use super::source::RangeFileSource;
use super::state::ResourceSize;
use crate::stream::pushed_demux_registry;

/// Можно ли перематывать файл.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSeekability {
    /// Размер файла известен: перемотка работает (новый запрос с нужного смещения).
    Seekable,
    /// Размер неизвестен: играем только вперёд.
    ForwardOnly,
}

/// Открытый файл: блокирующий демуксер и то, что нужно приложению для обёртки.
///
/// Демуксер читает через окно предзагрузки и **блокируется** на чтении: приложение
/// переносит его в фоновый поток (`ProgressiveDemuxer`) тем же путём, что обычный
/// progressive HTTP, передав [`Self::lifecycle_cancellation`] как токен закрытия.
pub struct OpenedPushedFile {
    /// Открытый блокирующий демуксер.
    pub demuxer: Box<dyn Demuxer + Send>,
    /// Перематывается ли файл.
    pub seekability: FileSeekability,
    /// Размер файла, если страница его сообщила.
    pub content_length: Option<u64>,
    /// Токен жизни файла: когда владелец демуксера его отменяет, ждущее чтение
    /// просыпается ошибкой «источник закрыт», а не висит.
    pub lifecycle_cancellation: CancellationToken,
}

impl std::fmt::Debug for OpenedPushedFile {
    /// Демуксер во внешнюю диагностику не выводится.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenedPushedFile")
            .field("seekability", &self.seekability)
            .field("content_length", &self.content_length)
            .finish_non_exhaustive()
    }
}

/// Ход открытия.
#[derive(Debug)]
pub enum FileOpenProgress {
    /// Ещё открывается (ждём байты страницы).
    Pending,
    /// Файл открыт.
    Ready(OpenedPushedFile),
    /// Открыть не удалось.
    Failed(PushedFileError),
}

/// Слот результата открытия.
enum OpenSlot {
    /// Поток открытия работает.
    Running,
    /// Результат готов, никто не забрал.
    Done(Result<OpenedPushedFile, PushedFileError>),
    /// Результат выдан.
    Taken,
}

/// Сторона владельца файла: спрашивает, открылся ли он.
pub struct PushedFileOpener {
    /// Общее ядро.
    core: Arc<FileCore>,
    /// Политика.
    policy: PushedFilePolicy,
    /// Результат потока открытия.
    slot: Arc<Mutex<OpenSlot>>,
    /// Отмена ожиданий открытия (срабатывает, когда открывальщик пропал).
    open_cancellation: CancellationToken,
    /// Токен жизни файла; уходит в `OpenedPushedFile`.
    lifecycle_cancellation: CancellationToken,
    /// Поток открытия уже запущен.
    started: bool,
}

impl PushedFileOpener {
    /// Открывальщик над ядром.
    pub(super) fn new(core: Arc<FileCore>, policy: PushedFilePolicy) -> Self {
        Self {
            core,
            policy,
            slot: Arc::new(Mutex::new(OpenSlot::Running)),
            open_cancellation: CancellationToken::new(),
            lifecycle_cancellation: CancellationToken::new(),
            started: false,
        }
    }

    /// Двигает открытие, не блокируя вызывающего.
    ///
    /// Первый вызов запускает открытие: ставит первый запрос (смещение 0) — после
    /// него отправитель забирает его через `take_requests`. Приложение зовёт метод
    /// из главного цикла, пока не придёт `Ready` или `Failed`.
    pub fn poll_open(&mut self) -> FileOpenProgress {
        if !self.started {
            self.started = true;
            if let Err(error) = self.start_open_thread() {
                let error = self.record_failure(error);
                *lock_slot(&self.slot) = OpenSlot::Taken;
                return FileOpenProgress::Failed(error);
            }
        }
        let mut slot = lock_slot(&self.slot);
        match std::mem::replace(&mut *slot, OpenSlot::Taken) {
            OpenSlot::Running => {
                *slot = OpenSlot::Running;
                FileOpenProgress::Pending
            }
            OpenSlot::Done(Ok(opened)) => FileOpenProgress::Ready(opened),
            OpenSlot::Done(Err(error)) => FileOpenProgress::Failed(error),
            OpenSlot::Taken => FileOpenProgress::Failed(PushedFileError::AlreadyOpened),
        }
    }

    /// Запоминает ошибку как первую ошибку файла.
    fn record_failure(&self, error: PushedFileError) -> PushedFileError {
        // Ошибку файла хранит ядро: писатель и читатели видят одну и ту же.
        self.core.record_failure(error)
    }

    /// Запускает поток открытия.
    fn start_open_thread(&self) -> Result<(), PushedFileError> {
        let job = OpenJob {
            core: Arc::clone(&self.core),
            policy: self.policy,
            open_cancellation: self.open_cancellation.clone(),
            lifecycle_cancellation: self.lifecycle_cancellation.clone(),
        };
        let slot = Arc::clone(&self.slot);
        thread::Builder::new()
            .name("pushed-file-open".to_owned())
            .spawn(move || {
                let result = job.run();
                *lock_slot(&slot) = OpenSlot::Done(result);
            })
            .map(|_| ())
            .map_err(|source| PushedFileError::OpenerStart {
                reason: source.to_string(),
            })
    }
}

impl Drop for PushedFileOpener {
    /// Открывальщик пропал: незавершённое открытие прекращается, а готовый файл
    /// (уже выданный приложению) продолжает жить.
    fn drop(&mut self) {
        self.open_cancellation.cancel();
        if matches!(*lock_slot(&self.slot), OpenSlot::Running) {
            self.lifecycle_cancellation.cancel();
        }
    }
}

impl std::fmt::Debug for PushedFileOpener {
    /// Содержимое не выводится.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PushedFileOpener")
            .finish_non_exhaustive()
    }
}

/// Захватывает слот; отравленный замок не мешает.
fn lock_slot(slot: &Mutex<OpenSlot>) -> MutexGuard<'_, OpenSlot> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Работа потока открытия.
struct OpenJob {
    /// Общее ядро.
    core: Arc<FileCore>,
    /// Политика.
    policy: PushedFilePolicy,
    /// Отмена ожиданий (открывальщик пропал).
    open_cancellation: CancellationToken,
    /// Токен жизни файла.
    lifecycle_cancellation: CancellationToken,
}

impl OpenJob {
    /// Открывает файл; любую ошибку делает ошибкой файла (первая побеждает).
    fn run(self) -> Result<OpenedPushedFile, PushedFileError> {
        self.open().map_err(|error| self.core.record_failure(error))
    }

    /// Шаги открытия.
    fn open(&self) -> Result<OpenedPushedFile, PushedFileError> {
        let registry = pushed_demux_registry().map_err(|error| PushedFileError::OpenerStart {
            reason: error.to_string(),
        })?;
        let sniff_budget =
            self.policy
                .sniff_budget()
                .map_err(|error| PushedFileError::OpenerStart {
                    reason: error.to_string(),
                })?;

        // 1. Первый запрос с начала файла: из его итога узнаём размер.
        self.core
            .start_initial_request(self.policy.initial_request_bytes())?;
        let size = self
            .core
            .await_initial_completion(&self.open_cancellation)
            .map_err(open_interrupted_to_error)?;
        let content_length = match size {
            ResourceSize::Known(length) => Some(length),
            ResourceSize::Unknown | ResourceSize::Unset => None,
        };
        let mut source = RangeFileSource::new(Arc::clone(&self.core), content_length);

        // 2. Оглавление MP4: в конце без размера — отказ; шифрование — отказ.
        check_movie_layout(
            &mut source,
            size,
            self.policy.max_movie_bytes(),
            &self.open_cancellation,
        )
        .map_err(|problem| self.layout_problem_to_error(problem))?;
        source.move_to(0);

        // 3. Окно предзагрузки и демуксер — как у обычного progressive HTTP.
        let prefetch = PrefetchingByteSource::new(Box::new(source), self.policy.prefetch())
            .map_err(|error| PushedFileError::OpenerStart {
                reason: error.to_string(),
            })?
            .with_lifecycle_cancellation(self.lifecycle_cancellation.clone());
        let demuxer = registry
            .open(
                DemuxInput::byte_source(Box::new(prefetch)),
                DemuxHints::none(),
                sniff_budget,
                self.lifecycle_cancellation.clone(),
            )
            .map_err(|error| self.unparsable_or_root_cause(error.to_string()))?;
        Ok(OpenedPushedFile {
            demuxer: Box::new(SealAwareDemuxer::new(demuxer, Arc::clone(&self.core))),
            seekability: if content_length.is_some() {
                FileSeekability::Seekable
            } else {
                FileSeekability::ForwardOnly
            },
            content_length,
            lifecycle_cancellation: self.lifecycle_cancellation.clone(),
        })
    }

    /// Ошибка разбора: если корень — файл (страница, конец байтов), называем корень.
    fn unparsable_or_root_cause(&self, reason: String) -> PushedFileError {
        if let Some(root) = self.core.failure() {
            return root;
        }
        if self.core.is_sealed() {
            return PushedFileError::SealedBeforeOpen;
        }
        PushedFileError::UnparsableMedia { reason }
    }

    /// Проблема проверки раскладки в ошибку файла.
    fn layout_problem_to_error(&self, problem: LayoutProblem) -> PushedFileError {
        match problem {
            LayoutProblem::Verdict(error) => error,
            LayoutProblem::Read(error) => self.unparsable_or_root_cause(error.to_string()),
            LayoutProblem::EndedEarly => {
                self.unparsable_or_root_cause("the file ended inside an MP4 box".to_owned())
            }
        }
    }
}

/// Прерванное ожидание открытия в ошибку файла.
fn open_interrupted_to_error(interrupted: OpenInterrupted) -> PushedFileError {
    match interrupted {
        OpenInterrupted::Failed(error) => error,
        OpenInterrupted::Cancelled => PushedFileError::OpenerStart {
            reason: "the file opener was dropped".to_owned(),
        },
    }
}
