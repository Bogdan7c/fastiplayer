//! Состояние файла: активное чтение, выданные запросы, сверка рамок страницы.
//!
//! Это чистая логика под одним замком (замком владеет [`FileCore`](super::core)):
//! здесь нет ни ожиданий, ни потоков, ни обратных вызовов, поэтому каждое правило
//! проверяется модульным тестом без сети.
//!
//! # Модель
//! Источник читает файл **последовательно, одним запросом за раз**. «Активное
//! чтение» — это текущий запрос (а после сетевого сбоя — его повтор с первого
//! недостающего байта) вместе с накопленными, но ещё не прочитанными байтами.
//! Память ограничена одним запросом (≤ 16 МиБ): отменённые запросы (перемотка ушла
//! дальше) буфера не держат, их запоздавшие рамки отбрасываются без сверки.
//!
//! # Недоверие к странице (инвариант 1)
//! Рамки обязаны идти строго подряд внутри запроса, не выходить за его конец и за
//! размер файла; размер файла между запросами не меняется. Нарушение — первая
//! ошибка файла, ждущие чтения и открытие получают её и не висят.

use std::time::{Duration, Instant};

use super::error::PushedFileError;
use super::ids::{RangeRequest, RangeRequestId, RangeRequestIds};
use super::retry::{RangeRetryPolicy, RetryDecision, RetrySchedule};
use super::writer::{RangeBytesVerdict, RangeFailure, RangeOutcome, ResourceSizeReport};

/// Что известно о размере файла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResourceSize {
    /// Ни один запрос ещё не завершился.
    Unset,
    /// Страница сообщила размер.
    Known(u64),
    /// Страница размера не знает (поток без `Content-Length`).
    Unknown,
}

/// Фаза активного чтения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadPhase {
    /// Страница присылает байты запроса.
    Receiving,
    /// Запрос завершён страницей (байты ещё могут ждать чтения).
    Completed,
    /// Сетевой сбой: повторим тот же диапазон новым запросом в `retry_at`.
    WaitingToRetry {
        /// Когда можно слать повтор.
        retry_at: Instant,
    },
}

/// Текущее чтение: запрос, накопленные байты и позиция читателя в них.
#[derive(Debug)]
struct ActiveRead {
    /// Номер запроса, которого сейчас ждём.
    id: RangeRequestId,
    /// Смещение файла, с которого начинается `buffer`.
    buffer_start: u64,
    /// Принятые байты начиная с `buffer_start`.
    buffer: Vec<u8>,
    /// Сколько байтов буфера читатель уже забрал.
    consumed: usize,
    /// Первый байт после запрошенного диапазона (весь запрос, не текущий повтор).
    requested_end: u64,
    /// Длина исходного запроса: по ней просим продолжение, если сбой пришёл после
    /// последнего байта, а размер файла ещё неизвестен.
    span: u64,
    /// Фаза.
    phase: ReadPhase,
}

impl ActiveRead {
    /// Смещение файла сразу за принятыми байтами: отсюда обязана продолжиться рамка.
    fn buffer_end(&self) -> u64 {
        self.buffer_start
            .saturating_add(u64::try_from(self.buffer.len()).unwrap_or(u64::MAX))
    }

    /// Смещение файла, которое читатель получит следующим.
    fn next_read_position(&self) -> u64 {
        self.buffer_start
            .saturating_add(u64::try_from(self.consumed).unwrap_or(u64::MAX))
    }
}

/// Чем номер из рамки является для этого файла.
enum RequestStanding {
    /// Из прошлого (запрос прежнего файла) или отменённый/заменённый: молча отбросить.
    Stale,
    /// Номер, которого источник не выдавал: нарушение.
    Unissued,
    /// Текущий запрос.
    Active,
}

/// Решение читателю, что делать дальше, когда байтов в наличии нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReadPlan {
    /// Файл кончился (или больше байтов не будет): читатель получает `Ok(0)`.
    EndOfFile,
    /// Ждать рамок; `Some` — не дольше срока повтора.
    Wait(Option<Duration>),
    /// Запрос поставлен в очередь: надо будильником позвать отправителя.
    RequestQueued,
}

/// Шаг ожидания первого запроса (открытие узнаёт размер файла).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InitialStep {
    /// Первый запрос завершён: размер известен (или известно, что неизвестен).
    Completed(ResourceSize),
    /// Ждать.
    Wait(Option<Duration>),
    /// Повтор поставлен в очередь: позвать отправителя.
    RequestQueued,
}

/// Всё изменяемое состояние файла.
#[derive(Debug)]
pub(super) struct FileState {
    /// Первый номер этого файла: всё, что ниже, — запросы прежних файлов.
    stream_first_id: u64,
    /// Последний выданный этим файлом номер.
    last_issued: Option<u32>,
    /// Размер файла.
    size: ResourceSize,
    /// Где кончился последний завершённый запрос (для 416 у самого конца файла).
    last_completed_end: Option<u64>,
    /// Смещение конца файла, когда оно известно (размер или короткий ответ).
    end_of_file: Option<u64>,
    /// Запросы, которые ещё не забрал отправитель.
    outgoing: Vec<RangeRequest>,
    /// Текущее чтение.
    active: Option<ActiveRead>,
    /// Серия повторов.
    retry: RetrySchedule,
    /// Больше байтов не будет никогда.
    sealed: bool,
    /// Читатель (источник байтов демуксера) уничтожен.
    reader_closed: bool,
    /// Первая ошибка файла.
    failure: Option<PushedFileError>,
    /// Хотя бы одно чтение оборвано `seal`: читатель дошёл до границы полученного
    /// раньше настоящего конца файла. Не «seal вообще был», а именно «файл кончился
    /// не там, где кончается на самом деле» — по этому факту демуксер, споткнувшийся
    /// об обрыв, считает его концом потока ([`super::sealed_end`]).
    read_cut_by_seal: bool,
}

impl FileState {
    /// Состояние нового файла; `stream_first_id` — следующий номер выдавателя.
    pub(super) fn new(stream_first_id: u64, retry: RangeRetryPolicy) -> Self {
        Self {
            stream_first_id,
            last_issued: None,
            size: ResourceSize::Unset,
            end_of_file: None,
            last_completed_end: None,
            outgoing: Vec::new(),
            active: None,
            retry: RetrySchedule::new(retry),
            sealed: false,
            reader_closed: false,
            failure: None,
            read_cut_by_seal: false,
        }
    }

    /// Было ли чтение оборвано `seal` раньше настоящего конца файла.
    pub(super) const fn read_cut_by_seal(&self) -> bool {
        self.read_cut_by_seal
    }

    /// Первая ошибка файла, если он уже мёртв.
    pub(super) fn failure(&self) -> Option<&PushedFileError> {
        self.failure.as_ref()
    }

    /// Запоминает ошибку, если она первая, и возвращает действующую (первую).
    pub(super) fn fail(&mut self, error: PushedFileError) -> PushedFileError {
        self.failure.get_or_insert(error).clone()
    }

    /// Больше байтов не будет.
    pub(super) fn seal(&mut self) {
        self.sealed = true;
    }

    /// Закрыта ли запись.
    pub(super) const fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// Читатель уничтожен: буфер отпускается, новые рамки — устаревшие.
    pub(super) fn close_reader(&mut self) {
        self.reader_closed = true;
        self.abandon_active();
    }

    /// Запросы, которые надо отправить странице, в порядке выдачи.
    pub(super) fn take_requests(&mut self) -> Vec<RangeRequest> {
        std::mem::take(&mut self.outgoing)
    }

    // ---------------------------------------------------------------- запросы

    /// Выдаёт номер и ставит запрос `[start, end)` в очередь отправителя.
    fn issue_request(
        &mut self,
        ids: &RangeRequestIds,
        start: u64,
        end: u64,
    ) -> Result<RangeRequestId, PushedFileError> {
        // Пустой диапазон не просим никогда: подгонять его под 1 байт нельзя — страница
        // ответила бы «0 байт» и чтение крутилось бы по кругу (offset около u64::MAX).
        let length = u32::try_from(end.saturating_sub(start))
            .ok()
            .or_else(|| (end > start).then_some(u32::MAX))
            .and_then(std::num::NonZeroU32::new)
            .ok_or(PushedFileError::EmptyRangeRequest { offset: start })?;
        let id = ids.issue()?;
        self.last_issued = Some(id.get());
        self.outgoing.push(RangeRequest {
            id,
            offset: start,
            length,
        });
        Ok(id)
    }

    /// Бросает текущее чтение: его запоздавшие рамки станут устаревшими, а если
    /// запрос ещё не ушёл странице — он не уйдёт вовсе.
    fn abandon_active(&mut self) {
        if let Some(abandoned) = self.active.take() {
            self.outgoing.retain(|request| request.id != abandoned.id);
        }
    }

    /// Начинает новое чтение `[start, end)`; прежнее брошено.
    fn begin_read(
        &mut self,
        ids: &RangeRequestIds,
        start: u64,
        end: u64,
    ) -> Result<(), PushedFileError> {
        self.abandon_active();
        let id = self.issue_request(ids, start, end)?;
        self.active = Some(ActiveRead {
            id,
            buffer_start: start,
            buffer: Vec::new(),
            consumed: 0,
            requested_end: end,
            span: end - start,
            phase: ReadPhase::Receiving,
        });
        Ok(())
    }

    /// Первый запрос открытия: с начала файла, длиной в начальный кусок.
    pub(super) fn begin_initial_read(
        &mut self,
        ids: &RangeRequestIds,
        length: u32,
    ) -> Result<(), PushedFileError> {
        let result = self.begin_read(ids, 0, u64::from(length));
        result.map_err(|error| self.fail(error))
    }

    /// Повтор того же диапазона новым запросом с первого недостающего байта.
    fn reissue_after_pause(&mut self, ids: &RangeRequestIds) -> Result<(), PushedFileError> {
        let end_of_file = self.end_of_file;
        let Some((start, mut end)) = self
            .active
            .as_ref()
            .map(|active| (active.buffer_end(), active.requested_end))
        else {
            return Ok(());
        };
        if start >= end {
            // Все байты запроса пришли, потерялось только подтверждение, а размера
            // файла мы не знаем: просим продолжение той же длины — его итог скажет
            // размер (или конец файла).
            let span = self.active.as_ref().map_or(0, |active| active.span);
            end = start.saturating_add(span);
            if let Some(file_end) = end_of_file {
                end = end.min(file_end);
            }
        }
        let id = self.issue_request(ids, start, end)?;
        if let Some(active) = self.active.as_mut() {
            active.requested_end = end;
            active.id = id;
            active.phase = ReadPhase::Receiving;
        }
        tracing::debug!(
            attempt = self.retry.failed_attempts(),
            "range file is retrying a failed range request"
        );
        Ok(())
    }

    // ------------------------------------------------------ сверка (страница)

    /// Чем номер из рамки является для этого файла.
    ///
    /// Номер между первым номером файла и последним выданным, которого сейчас нет в
    /// работе, считается устаревшим: это отменённый запрос, повтор или запоздавшая
    /// рамка другого файла сеанса (номера сквозные). Все они одинаково безвредны.
    fn standing_of(&self, id: RangeRequestId) -> RequestStanding {
        let raw = id.get();
        if u64::from(raw) < self.stream_first_id {
            return RequestStanding::Stale;
        }
        if self.last_issued.is_none_or(|last| raw > last) {
            return RequestStanding::Unissued;
        }
        match &self.active {
            Some(active) if active.id == id => RequestStanding::Active,
            // Выдан нами, но отменён, заменён повтором или уже прочитан целиком.
            _ => RequestStanding::Stale,
        }
    }

    /// Принимает рамку байтов.
    pub(super) fn accept_bytes(
        &mut self,
        id: RangeRequestId,
        offset: u64,
        payload: &[u8],
    ) -> Result<RangeBytesVerdict, PushedFileError> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        if self.sealed {
            return Err(PushedFileError::WriterSealed);
        }
        if self.reader_closed {
            return Ok(RangeBytesVerdict::StaleDropped);
        }
        match self.standing_of(id) {
            RequestStanding::Stale => return Ok(RangeBytesVerdict::StaleDropped),
            RequestStanding::Unissued => {
                return Err(self.fail(PushedFileError::UnissuedRequest { id: id.get() }));
            }
            RequestStanding::Active => {}
        }
        let size = self.size;
        let Some(active) = self.active.as_mut() else {
            return Ok(RangeBytesVerdict::StaleDropped);
        };
        if active.phase != ReadPhase::Receiving {
            let error = PushedFileError::RequestAlreadyFinished { id: id.get() };
            return Err(self.fail(error));
        }
        let expected = active.buffer_end();
        if offset != expected {
            let error = PushedFileError::NonContiguousBytes {
                id: id.get(),
                expected,
                actual: offset,
            };
            return Err(self.fail(error));
        }
        let end = expected.saturating_add(u64::try_from(payload.len()).unwrap_or(u64::MAX));
        if end > active.requested_end {
            let error = PushedFileError::BytesBeyondRequest {
                id: id.get(),
                requested_end: active.requested_end,
            };
            return Err(self.fail(error));
        }
        if let ResourceSize::Known(size) = size
            && end > size
        {
            let error = PushedFileError::BytesBeyondResourceSize { size, end };
            return Err(self.fail(error));
        }
        active.buffer.extend_from_slice(payload);
        if !payload.is_empty() {
            self.retry.record_progress();
        }
        Ok(RangeBytesVerdict::Accepted)
    }

    /// Принимает итог запроса.
    pub(super) fn finish_request(
        &mut self,
        id: RangeRequestId,
        outcome: RangeOutcome,
        now: Instant,
    ) -> Result<(), PushedFileError> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        if self.sealed {
            return Err(PushedFileError::WriterSealed);
        }
        if self.reader_closed {
            return Ok(());
        }
        match self.standing_of(id) {
            RequestStanding::Stale => return Ok(()),
            RequestStanding::Unissued => {
                return Err(self.fail(PushedFileError::UnissuedRequest { id: id.get() }));
            }
            RequestStanding::Active => {}
        }
        let still_receiving = self
            .active
            .as_ref()
            .is_some_and(|active| active.phase == ReadPhase::Receiving);
        if !still_receiving {
            let error = PushedFileError::RequestAlreadyFinished { id: id.get() };
            return Err(self.fail(error));
        }
        let result = match outcome {
            RangeOutcome::Completed { resource_size } => self.complete_active(id, resource_size),
            RangeOutcome::Failed(RangeFailure::Network | RangeFailure::Http) => {
                self.schedule_retry(now)
            }
            RangeOutcome::Failed(RangeFailure::ResourceChanged) => {
                Err(PushedFileError::ResourceChanged)
            }
            RangeOutcome::Failed(RangeFailure::OutOfRange) => self.complete_out_of_range(),
        };
        result.map_err(|error| self.fail(error))
    }

    /// Запрос завершён: сверяем размер файла и недобор.
    fn complete_active(
        &mut self,
        id: RangeRequestId,
        report: ResourceSizeReport,
    ) -> Result<(), PushedFileError> {
        let Some(active) = self.active.as_ref() else {
            return Ok(());
        };
        let received_end = active.buffer_end();
        let requested_end = active.requested_end;
        match (self.size, report) {
            (ResourceSize::Unset, ResourceSizeReport::Known(size)) => {
                if received_end > size {
                    return Err(PushedFileError::BytesBeyondResourceSize {
                        size,
                        end: received_end,
                    });
                }
                self.size = ResourceSize::Known(size);
                self.end_of_file = Some(size);
            }
            (ResourceSize::Unset, ResourceSizeReport::Unknown)
            | (ResourceSize::Unknown, ResourceSizeReport::Unknown) => {
                self.size = ResourceSize::Unknown;
            }
            (ResourceSize::Known(before), ResourceSizeReport::Known(after)) => {
                if before != after {
                    return Err(PushedFileError::ResourceSizeChanged { before, after });
                }
            }
            (ResourceSize::Known(_), ResourceSizeReport::Unknown)
            | (ResourceSize::Unknown, ResourceSizeReport::Known(_)) => {
                return Err(PushedFileError::ResourceSizeKnowledgeChanged);
            }
        }
        if received_end < requested_end {
            // Недобор честен только у конца файла.
            match self.size {
                ResourceSize::Known(size) if received_end == size => {}
                ResourceSize::Unknown => match self.end_of_file {
                    // Конец файла уже известен и не может сдвинуться.
                    Some(known) if known != received_end => {
                        return Err(PushedFileError::ResourceSizeChanged {
                            before: known,
                            after: received_end,
                        });
                    }
                    _ => self.end_of_file = Some(received_end),
                },
                _ => {
                    return Err(PushedFileError::ShortRangeResponse {
                        id: id.get(),
                        ended_at: received_end,
                        requested_end,
                    });
                }
            }
        }
        if let Some(active) = self.active.as_mut() {
            active.phase = ReadPhase::Completed;
        }
        self.retry.record_progress();
        self.last_completed_end = Some(received_end);
        Ok(())
    }

    /// Сетевой сбой или ошибка HTTP: повторить по бюджету либо сдаться.
    fn schedule_retry(&mut self, now: Instant) -> Result<(), PushedFileError> {
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        if active.buffer_end() >= active.requested_end
            && matches!(self.size, ResourceSize::Known(_))
        {
            // Связь оборвалась после последнего байта, размер файла уже известен: все
            // запрошенные байты на месте — это завершённый запрос, а не атака.
            active.phase = ReadPhase::Completed;
            self.retry.record_progress();
            return Ok(());
        }
        // Иначе (размер ещё неизвестен) — обычный повтор; если байтов не хватает нет,
        // повтор просит продолжение (см. `reissue_after_pause`).
        match self.retry.record_failure(now) {
            RetryDecision::After(delay) => {
                active.phase = ReadPhase::WaitingToRetry {
                    retry_at: now + delay,
                };
                Ok(())
            }
            RetryDecision::GiveUp => Err(PushedFileError::RetryBudgetExhausted {
                attempts: self.retry.failed_attempts(),
            }),
        }
    }

    /// «Диапазон вне файла»: за концом файла это просто конец, внутри — ошибка.
    fn complete_out_of_range(&mut self) -> Result<(), PushedFileError> {
        let end_of_file = self.end_of_file;
        let last_completed_end = self.last_completed_end;
        let unknown_size = self.size == ResourceSize::Unknown;
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        let failed_offset = active.buffer_end();
        match end_of_file {
            Some(end) if failed_offset >= end => {
                active.phase = ReadPhase::Completed;
                Ok(())
            }
            // Размер неизвестен, а запрос начинается ровно там, где кончился прошлый:
            // настоящий сервер отвечает 416 на диапазон с самого конца файла. Для файла,
            // длина которого кратна запросу, это чистый конец, а не сбой.
            None if unknown_size && last_completed_end == Some(failed_offset) => {
                active.phase = ReadPhase::Completed;
                self.end_of_file = Some(failed_offset);
                Ok(())
            }
            _ => Err(PushedFileError::RangeRejected {
                offset: failed_offset,
            }),
        }
    }

    // ------------------------------------------------------ чтение (плеер)

    /// Копирует уже принятые байты, если читатель стоит на их начале.
    pub(super) fn copy_available(&mut self, position: u64, output: &mut [u8]) -> usize {
        let Some(active) = self.active.as_mut() else {
            return 0;
        };
        if active.next_read_position() != position {
            return 0;
        }
        let available = active.buffer.len() - active.consumed;
        let count = available.min(output.len());
        output[..count].copy_from_slice(&active.buffer[active.consumed..active.consumed + count]);
        active.consumed += count;
        if active.consumed == active.buffer.len() {
            if active.phase == ReadPhase::Completed {
                // Запрос завершён и прочитан целиком: память отпускаем.
                self.active = None;
            } else {
                // Запрос продолжается: прочитанное не держим (память — один запрос).
                active.buffer_start = active.buffer_end();
                active.buffer.clear();
                active.consumed = 0;
            }
        }
        count
    }

    /// Ждёт ли читатель в `position` ещё байтов того же запроса (рамки идут).
    pub(super) fn more_bytes_expected_at(&self, position: u64) -> bool {
        self.active.as_ref().is_some_and(|active| {
            active.next_read_position() == position && active.phase == ReadPhase::Receiving
        })
    }

    /// Решает, что делать читателю, у которого в наличии байтов нет.
    pub(super) fn plan_read(
        &mut self,
        ids: &RangeRequestIds,
        position: u64,
        wanted_bytes: usize,
        max_request_bytes: u32,
        now: Instant,
    ) -> Result<ReadPlan, PushedFileError> {
        if self.sealed {
            // Страница ушла насовсем: за пределами полученного — конец файла. Если
            // настоящий конец файла дальше (или неизвестен), читатель оборван.
            if self.end_of_file.is_none_or(|end| position < end) {
                self.read_cut_by_seal = true;
            }
            return Ok(ReadPlan::EndOfFile);
        }
        if let Some(active) = &self.active
            && active.next_read_position() == position
        {
            match active.phase {
                ReadPhase::Receiving => return Ok(ReadPlan::Wait(None)),
                ReadPhase::WaitingToRetry { retry_at } => {
                    return self
                        .wait_or_retry(ids, retry_at, now)
                        .map(|step| match step {
                            RetryStep::Wait(delay) => ReadPlan::Wait(Some(delay)),
                            RetryStep::Queued => ReadPlan::RequestQueued,
                        });
                }
                ReadPhase::Completed => {}
            }
        }
        if self.end_of_file.is_some_and(|end| position >= end) {
            return Ok(ReadPlan::EndOfFile);
        }
        let length = u64::try_from(wanted_bytes)
            .unwrap_or(u64::MAX)
            .clamp(1, u64::from(max_request_bytes));
        let mut end = position.saturating_add(length);
        if let Some(file_end) = self.end_of_file {
            end = end.min(file_end);
        }
        if end <= position {
            // Пустой диапазон (позиция у предела u64 или за концом файла): просить нечего.
            return Ok(ReadPlan::EndOfFile);
        }
        self.begin_read(ids, position, end)
            .map(|()| ReadPlan::RequestQueued)
    }

    /// Шаг ожидания первого запроса для открытия.
    pub(super) fn step_initial(
        &mut self,
        ids: &RangeRequestIds,
        now: Instant,
    ) -> Result<InitialStep, PushedFileError> {
        let Some(phase) = self.active.as_ref().map(|active| active.phase) else {
            return Err(PushedFileError::SealedBeforeOpen);
        };
        match phase {
            ReadPhase::Completed => Ok(InitialStep::Completed(self.size)),
            _ if self.sealed => Err(PushedFileError::SealedBeforeOpen),
            ReadPhase::Receiving => Ok(InitialStep::Wait(None)),
            ReadPhase::WaitingToRetry { retry_at } => {
                self.wait_or_retry(ids, retry_at, now)
                    .map(|step| match step {
                        RetryStep::Wait(delay) => InitialStep::Wait(Some(delay)),
                        RetryStep::Queued => InitialStep::RequestQueued,
                    })
            }
        }
    }

    /// Пауза повтора: подождать остаток либо отправить повтор.
    fn wait_or_retry(
        &mut self,
        ids: &RangeRequestIds,
        retry_at: Instant,
        now: Instant,
    ) -> Result<RetryStep, PushedFileError> {
        if now < retry_at {
            return Ok(RetryStep::Wait(retry_at - now));
        }
        self.reissue_after_pause(ids)?;
        Ok(RetryStep::Queued)
    }
}

/// Итог проверки паузы повтора.
enum RetryStep {
    /// Ждать ещё столько.
    Wait(Duration),
    /// Повтор поставлен в очередь.
    Queued,
}
