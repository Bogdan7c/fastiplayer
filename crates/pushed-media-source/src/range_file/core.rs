//! Общее ядро файла: состояние под замком, ожидания и будильники.
//!
//! Ядро делят четыре стороны: писатель (страница несёт байты), источник байтов
//! (читает демуксер в потоке предзагрузки), открытие (узнаёт размер файла) и
//! отправитель запросов (приложение). Правила, которые нельзя нарушать:
//! - **ничего не ждём и никого не зовём под замком**: ожидание — `Condvar`
//!   (замок отпускается), будильник отправителя вызывается после снятия замка;
//! - все ожидания отменяемы `CancellationToken` (опрос раз в 10 мс) и `seal`;
//! - ошибка файла — первая и общая для всех ждущих.

use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use source_core::{CancellationToken, SourceError, SourceResult};

use super::error::PushedFileError;
use super::ids::{RangeRequest, RangeRequestId, RangeRequestIds, RangeRequestNotifier};
use super::retry::RangeRetryPolicy;
use super::state::{FileState, InitialStep, ReadPlan, ResourceSize};
use super::writer::{RangeBytesVerdict, RangeOutcome};

/// Шаг опроса отмены во время ожидания: закрытие прерывает ожидание за ≤ 10 мс.
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Почему ожидание открытия прервано.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OpenInterrupted {
    /// Открытие отменено владельцем (открывальщик уничтожен).
    Cancelled,
    /// Файл мёртв: ошибка страницы или конца байтов.
    Failed(PushedFileError),
}

/// Ядро файла.
pub(super) struct FileCore {
    /// Состояние.
    state: Mutex<FileState>,
    /// Будит ждущих при каждом изменении состояния.
    changed: Condvar,
    /// Выдаватель номеров сеанса.
    ids: RangeRequestIds,
    /// Будильник отправителя запросов.
    notifier: Arc<dyn RangeRequestNotifier>,
    /// Самый длинный запрос, байт.
    max_request_bytes: u32,
}

impl FileCore {
    /// Ядро нового файла.
    pub(super) fn new(
        ids: RangeRequestIds,
        notifier: Arc<dyn RangeRequestNotifier>,
        max_request_bytes: u32,
        retry: RangeRetryPolicy,
    ) -> Arc<Self> {
        let first_id = ids.next_unissued();
        Arc::new(Self {
            state: Mutex::new(FileState::new(first_id, retry)),
            changed: Condvar::new(),
            ids,
            notifier,
            max_request_bytes,
        })
    }

    /// Захватывает состояние; отравленный замок не мешает читать.
    fn lock(&self) -> MutexGuard<'_, FileState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ждёт изменения состояния не дольше `limit` и не дольше шага опроса отмены.
    fn wait<'guard>(
        &self,
        guard: MutexGuard<'guard, FileState>,
        limit: Option<Duration>,
    ) -> MutexGuard<'guard, FileState> {
        let step = limit.map_or(CANCELLATION_POLL_INTERVAL, |limit| {
            limit.min(CANCELLATION_POLL_INTERVAL)
        });
        self.changed
            .wait_timeout(guard, step)
            .unwrap_or_else(PoisonError::into_inner)
            .0
    }

    /// Зовёт отправителя запросов; вызывать строго без замка.
    fn announce_request(&self) {
        self.notifier.range_requested();
    }

    /// Снимает замок, зовёт отправителя и берёт замок снова.
    fn announce_and_relock<'guard>(
        &'guard self,
        guard: MutexGuard<'guard, FileState>,
    ) -> MutexGuard<'guard, FileState> {
        drop(guard);
        self.announce_request();
        self.lock()
    }

    // ------------------------------------------------------------ отправитель

    /// Запросы, которые надо отправить странице, в порядке выдачи.
    pub(super) fn take_requests(&self) -> Vec<RangeRequest> {
        self.lock().take_requests()
    }

    // --------------------------------------------------------------- писатель

    /// Рамка байтов от страницы.
    pub(super) fn accept_range_bytes(
        &self,
        id: RangeRequestId,
        offset: u64,
        payload: &[u8],
    ) -> Result<RangeBytesVerdict, PushedFileError> {
        let verdict = self.lock().accept_bytes(id, offset, payload);
        self.changed.notify_all();
        verdict
    }

    /// Итог запроса от страницы.
    pub(super) fn finish_request(
        &self,
        id: RangeRequestId,
        outcome: RangeOutcome,
    ) -> Result<(), PushedFileError> {
        let result = self.lock().finish_request(id, outcome, Instant::now());
        self.changed.notify_all();
        result
    }

    /// «Больше байтов не будет»: ждущие просыпаются.
    pub(super) fn seal(&self) {
        self.lock().seal();
        self.changed.notify_all();
    }

    /// Первая ошибка файла.
    pub(super) fn failure(&self) -> Option<PushedFileError> {
        self.lock().failure().cloned()
    }

    /// Читатель уничтожен: буфер отпущен.
    pub(super) fn close_reader(&self) {
        self.lock().close_reader();
        self.changed.notify_all();
    }

    // ---------------------------------------------------------------- открытие

    /// Ставит первый запрос открытия (смещение 0).
    pub(super) fn start_initial_request(&self, length: u32) -> Result<(), PushedFileError> {
        let result = self.lock().begin_initial_read(&self.ids, length);
        if result.is_ok() {
            self.announce_request();
        }
        result
    }

    /// Ждёт завершения первого запроса и возвращает, что известно о размере файла.
    pub(super) fn await_initial_completion(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<ResourceSize, OpenInterrupted> {
        let mut state = self.lock();
        loop {
            if cancellation.is_cancelled() {
                return Err(OpenInterrupted::Cancelled);
            }
            if let Some(failure) = state.failure() {
                return Err(OpenInterrupted::Failed(failure.clone()));
            }
            match state.step_initial(&self.ids, Instant::now()) {
                Err(error) => {
                    let error = state.fail(error);
                    return Err(OpenInterrupted::Failed(error));
                }
                Ok(InitialStep::Completed(size)) => return Ok(size),
                Ok(InitialStep::Wait(limit)) => state = self.wait(state, limit),
                Ok(InitialStep::RequestQueued) => state = self.announce_and_relock(state),
            }
        }
    }

    // ------------------------------------------------------------------ чтение

    /// Читает файл с `position`: блокирует, пока рамки не придут, и возвращает `Ok(0)`
    /// на конце файла (или когда страница ушла и дальше байтов нет).
    ///
    /// Если часть байтов уже скопирована, а дальше ждать пришлось бы нового запроса,
    /// читатель получает то, что есть: ошибка или ожидание — при следующем вызове.
    pub(super) fn read_at(
        &self,
        position: u64,
        output: &mut [u8],
        cancellation: &CancellationToken,
    ) -> SourceResult<usize> {
        let mut state = self.lock();
        let mut filled = 0;
        loop {
            filled += state.copy_available(position + filled as u64, &mut output[filled..]);
            let next_position = position + filled as u64;
            if filled == output.len() {
                return Ok(filled);
            }
            if filled > 0 && !state.more_bytes_expected_at(next_position) {
                return Ok(filled);
            }
            if cancellation.is_cancelled() {
                return partial_or(filled, SourceError::Cancelled);
            }
            if let Some(failure) = state.failure() {
                return partial_or(filled, file_error_to_source_error(failure));
            }
            let plan = state.plan_read(
                &self.ids,
                next_position,
                output.len() - filled,
                self.max_request_bytes,
                Instant::now(),
            );
            match plan {
                Err(error) => {
                    let error = state.fail(error);
                    return partial_or(filled, file_error_to_source_error(&error));
                }
                Ok(ReadPlan::EndOfFile) => return Ok(filled),
                Ok(ReadPlan::Wait(limit)) => state = self.wait(state, limit),
                Ok(ReadPlan::RequestQueued) => state = self.announce_and_relock(state),
            }
        }
    }
}

/// Уже скопированные байты важнее ошибки: ошибка придёт со следующим чтением.
fn partial_or(filled: usize, error: SourceError) -> SourceResult<usize> {
    if filled > 0 { Ok(filled) } else { Err(error) }
}

/// Ошибка файла в словаре источников байтов: исходная ошибка остаётся в цепочке.
pub(super) fn file_error_to_source_error(error: &PushedFileError) -> SourceError {
    SourceError::LocalIo {
        context: "pushed range file",
        source: io::Error::other(error.clone()),
    }
}

impl FileCore {
    /// Запоминает ошибку как первую ошибку файла; возвращает действующую (первую).
    pub(super) fn record_failure(&self, error: PushedFileError) -> PushedFileError {
        let effective = self.lock().fail(error);
        self.changed.notify_all();
        effective
    }

    /// Закрыта ли запись («больше байтов не будет»).
    pub(super) fn is_sealed(&self) -> bool {
        self.lock().is_sealed()
    }
}

impl FileCore {
    /// Ждёт ли файл байтов живой страницы (правило — `FileState::data_wait`).
    pub(super) fn data_wait(&self) -> media_core::DemuxDataWait {
        self.lock().data_wait()
    }

    /// Было ли чтение оборвано `seal` раньше настоящего конца файла.
    pub(super) fn read_cut_by_seal(&self) -> bool {
        self.lock().read_cut_by_seal()
    }
}
