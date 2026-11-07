//! Всё изменяемое состояние очереди — чистая логика под одним замком.
//!
//! Здесь нет ожиданий, потоков и вызовов наружу: ядро (`core.rs`) берёт замок, зовёт
//! метод, снимает замок и только потом будит ждущих ([`WakeList`]) и отправителя.
//! Правила:
//! - первое нарушение страницы побеждает: очередь мертва для всех, исходящие
//!   убраны, ждущие разбужены и получают его же;
//! - место «в полёте» считается по записям ([`Record::occupies_slot`]), а не
//!   отдельным счётчиком: так оно не может разойтись с жизнью записей;
//! - ждущие места выдаются строго по очереди прихода (билеты), иначе поздний
//!   `start` мог бы обогнать ранний.

use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroUsize;
use std::task::Waker;
use std::time::{Duration, Instant};

use super::error::{ResourceFetchError, ResourceFetchViolation};
use super::ledger::OwnedRequestIds;
use super::record::{ChunkStep, FrameEffect, Record, RecordRetention, WakeList};
use super::request::{ResourceFetchSpec, ResourceRequest};
use super::requests::ResourceSealCut;
use super::stats::{ResourceFetchStats, bump};
use super::writer::{ResourceFetchOutcome, ResourceFrameVerdict, ResourceRequestClaim};
use crate::range_file::{RangeIdStanding, RangeRequestId, RangeRequestIds};

/// Работает ли сторож молчания (поток для асинхронных ожиданий).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WatchdogStatus {
    /// Не работает: асинхронно никто не ждёт.
    Idle,
    /// Работает.
    Running,
}

/// Надо ли запускать сторожа после регистрации будильника.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WatchdogDemand {
    /// Сторож уже работает (его только разбудить).
    AlreadyRunning,
    /// Сторож не работал: ядро запускает его после снятия замка.
    StartNow,
}

/// Билет ждущего места `start` (порядок прихода).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StarterTicket(u64);

/// Шаг выдачи номера.
#[derive(Debug)]
pub(super) enum IssueStep {
    /// Номер выдан, запрос в исходящих.
    Issued(RangeRequestId),
    /// Ждать места (или своей очереди).
    Wait,
    /// Выдачи не будет.
    Failed(ResourceFetchError),
}

/// Состояние очереди.
#[derive(Debug)]
pub(super) struct QueueState {
    /// Живые записи по номеру.
    records: BTreeMap<u32, Record>,
    /// Номера, которые приложение ещё не забрало, в порядке выдачи.
    outgoing: VecDeque<u32>,
    /// Все номера, выданные этой очередью.
    owned: OwnedRequestIds,
    /// Следующий билет ждущего места.
    next_ticket: u64,
    /// Ждущие места, по порядку прихода.
    waiting_starters: VecDeque<u64>,
    /// Учёт.
    stats: ResourceFetchStats,
    /// Первое нарушение страницы.
    failure: Option<ResourceFetchViolation>,
    /// Источник закрыт.
    sealed: bool,
    /// Печать оборвала хотя бы одно чтение движка (см. [`ResourceSealCut`]).
    seal_cut: ResourceSealCut,
    /// Сторож молчания.
    watchdog: WatchdogStatus,
}

impl QueueState {
    /// Пустая очередь.
    pub(super) fn new() -> Self {
        Self {
            records: BTreeMap::new(),
            outgoing: VecDeque::new(),
            owned: OwnedRequestIds::default(),
            next_ticket: 0,
            waiting_starters: VecDeque::new(),
            stats: ResourceFetchStats::default(),
            failure: None,
            sealed: false,
            seal_cut: ResourceSealCut::NoReadCut,
            watchdog: WatchdogStatus::Idle,
        }
    }

    /// Признаёт мёртвыми запросы, по которым страница молчит дольше срока.
    pub(super) fn sweep_silent(
        &mut self,
        now: Instant,
        idle_timeout: Duration,
        wake: &mut WakeList,
    ) {
        for record in self.records.values_mut() {
            record.expire_if_silent(now, idle_timeout, &mut self.stats, wake);
        }
    }

    // ------------------------------------------------------------ движок: старт

    /// Встаёт в очередь за местом.
    pub(super) fn join_starters(&mut self) -> StarterTicket {
        let ticket = self.next_ticket;
        self.next_ticket = self.next_ticket.saturating_add(1);
        self.waiting_starters.push_back(ticket);
        StarterTicket(ticket)
    }

    /// Уходит из очереди за местом (выдан номер, отмена или отказ).
    pub(super) fn leave_starters(&mut self, ticket: StarterTicket) {
        self.waiting_starters.retain(|waiting| *waiting != ticket.0);
    }

    /// Сколько мест в полёте занято.
    pub(super) fn in_flight(&self) -> usize {
        self.records
            .values()
            .filter(|record| record.occupies_slot())
            .count()
    }

    /// Пытается выдать номер владельцу билета.
    pub(super) fn try_issue(
        &mut self,
        ticket: StarterTicket,
        spec: ResourceFetchSpec,
        ids: &RangeRequestIds,
        max_in_flight: NonZeroUsize,
    ) -> IssueStep {
        if let Some(violation) = &self.failure {
            let error = ResourceFetchError::PageViolation(violation.clone());
            self.leave_starters(ticket);
            return IssueStep::Failed(error);
        }
        if self.sealed {
            self.leave_starters(ticket);
            // Движок хотел ещё ресурс, а печать его уже не пустит: полученного не хватило.
            self.seal_cut = ResourceSealCut::ReadCutBySeal;
            return IssueStep::Failed(ResourceFetchError::SourceClosed);
        }
        let first_in_line = self.waiting_starters.front() == Some(&ticket.0);
        if !first_in_line || self.in_flight() >= max_in_flight.get() {
            return IssueStep::Wait;
        }
        self.leave_starters(ticket);
        let id = match ids.issue() {
            Ok(id) => id,
            Err(error) => {
                // Счётчик сеанса падает только исчерпанием номеров.
                tracing::warn!(%error, "resource fetch queue could not issue a request number");
                return IssueStep::Failed(ResourceFetchError::RequestIdsExhausted);
            }
        };
        self.owned.record(id.get());
        self.records.insert(id.get(), Record::new(spec));
        self.outgoing.push_back(id.get());
        bump(&mut self.stats.issued);
        IssueStep::Issued(id)
    }

    // -------------------------------------------------------- приложение: отправка

    /// Запросы для отправки в порядке выдачи; с этого мгновения считается молчание.
    pub(super) fn take_requests(
        &mut self,
        now: Instant,
        wake: &mut WakeList,
    ) -> Vec<ResourceRequest> {
        let mut requests = Vec::with_capacity(self.outgoing.len());
        for raw_id in self.outgoing.drain(..) {
            // Брошенные до отправки убираются из исходящих сразу, поэтому записи есть.
            let Some(record) = self.records.get_mut(&raw_id) else {
                continue;
            };
            record.mark_sent(now, wake);
            let spec = *record.spec();
            requests.push(ResourceRequest {
                id: RangeRequestId::new(raw_id),
                handle: spec.handle,
                extent: spec.extent,
                max_bytes: spec.max_bytes,
            });
            bump(&mut self.stats.sent);
        }
        requests
    }

    /// Учёт.
    pub(super) const fn stats(&self) -> ResourceFetchStats {
        self.stats
    }

    // ------------------------------------------------------------ внешняя сторона

    /// Чей номер.
    pub(super) fn claim_of(&self, id: RangeRequestId) -> ResourceRequestClaim {
        if self.owned.owns(id.get()) {
            ResourceRequestClaim::Ours
        } else {
            ResourceRequestClaim::NotOurs
        }
    }

    /// Кусок тела.
    pub(super) fn accept_bytes(
        &mut self,
        id: RangeRequestId,
        offset: u64,
        payload: &[u8],
        now: Instant,
        ids: &RangeRequestIds,
        wake: &mut WakeList,
    ) -> Result<ResourceFrameVerdict, ResourceFetchViolation> {
        self.on_frame(id, ids, wake, |record, stats, wake| {
            record.accept_bytes(id.get(), offset, payload, now, stats, wake)
        })
    }

    /// Итог запроса.
    pub(super) fn finish(
        &mut self,
        id: RangeRequestId,
        outcome: ResourceFetchOutcome,
        ids: &RangeRequestIds,
        wake: &mut WakeList,
    ) -> Result<ResourceFrameVerdict, ResourceFetchViolation> {
        self.on_frame(id, ids, wake, |record, stats, wake| {
            record.finish(id.get(), outcome, stats, wake)
        })
    }

    /// Общая сверка рамки: мертва ли очередь, чей номер, что сказала запись.
    fn on_frame(
        &mut self,
        id: RangeRequestId,
        ids: &RangeRequestIds,
        wake: &mut WakeList,
        apply: impl FnOnce(
            &mut Record,
            &mut ResourceFetchStats,
            &mut WakeList,
        ) -> Result<FrameEffect, ResourceFetchViolation>,
    ) -> Result<ResourceFrameVerdict, ResourceFetchViolation> {
        if let Some(violation) = &self.failure {
            return Err(violation.clone());
        }
        if self.sealed {
            // Поколение кончилось: поздние рамки безвредны.
            return Ok(self.stale());
        }
        let raw_id = id.get();
        let effect = match self.records.get_mut(&raw_id) {
            Some(record) => apply(record, &mut self.stats, wake),
            None => self.standing_without_record(id, ids),
        };
        match effect {
            Ok(FrameEffect::Accepted) => Ok(ResourceFrameVerdict::Accepted),
            Ok(FrameEffect::StaleKeep) => Ok(self.stale()),
            Ok(FrameEffect::StaleForget) => {
                self.records.remove(&raw_id);
                Ok(self.stale())
            }
            Err(violation) => Err(self.fail(violation, wake)),
        }
    }

    /// Чем номер без живой записи является для очереди: чужой выданный номер —
    /// устаревший (файл или прежнее поколение сеанса), иначе — нарушение.
    fn standing_without_record(
        &self,
        id: RangeRequestId,
        ids: &RangeRequestIds,
    ) -> Result<FrameEffect, ResourceFetchViolation> {
        if self.owned.owns(id.get()) {
            // Наш номер без записи: брошен до отправки или уже закончен страницей.
            return Err(ResourceFetchViolation::RequestNotOpen { id: id.get() });
        }
        match ids.standing_of(id) {
            RangeIdStanding::Issued => Ok(FrameEffect::StaleKeep),
            RangeIdStanding::NeverIssued => {
                Err(ResourceFetchViolation::UnissuedRequest { id: id.get() })
            }
        }
    }

    /// Учитывает устаревшую рамку.
    fn stale(&mut self) -> ResourceFrameVerdict {
        bump(&mut self.stats.stale_frames);
        ResourceFrameVerdict::StaleDropped
    }

    /// Первое нарушение: очередь мертва, все ждущие просыпаются.
    fn fail(
        &mut self,
        violation: ResourceFetchViolation,
        wake: &mut WakeList,
    ) -> ResourceFetchViolation {
        tracing::warn!(%violation, "resource fetch queue: the page violated the contract");
        bump(&mut self.stats.failures.page_violations);
        self.failure = Some(violation.clone());
        self.outgoing.clear();
        for record in self.records.values_mut() {
            record.wake_on_failure(wake);
        }
        violation
    }

    /// Источник закрыт.
    pub(super) fn seal(&mut self, wake: &mut WakeList) {
        if self.sealed {
            return;
        }
        self.sealed = true;
        self.outgoing.clear();
        self.records
            .retain(|_, record| record.seal(wake) != RecordRetention::Forget);
    }

    // ---------------------------------------------------------- движок: чтение

    /// Следующий шаг чтения запроса `id`.
    pub(super) fn next_chunk(&mut self, id: RangeRequestId, idle_timeout: Duration) -> ChunkStep {
        if let Some(violation) = &self.failure {
            return ChunkStep::Failed(ResourceFetchError::PageViolation(violation.clone()));
        }
        let step = match self.records.get_mut(&id.get()) {
            Some(record) => record.next_chunk(idle_timeout),
            // Запись живёт, пока жив потребитель; честный отказ вместо паники.
            None => ChunkStep::Failed(ResourceFetchError::SourceClosed),
        };
        // «Закрыто» после печати — тело оборвано ею: всё пришедшее уже отдано, а
        // остального не будет никогда. Дочитанные до печати тела сюда не попадают.
        if self.sealed && step == ChunkStep::Failed(ResourceFetchError::SourceClosed) {
            self.seal_cut = ResourceSealCut::ReadCutBySeal;
        }
        step
    }

    /// Оборвала ли печать чтение движка.
    pub(super) const fn seal_cut(&self) -> ResourceSealCut {
        self.seal_cut
    }

    /// Запоминает будильник асинхронного потребителя и говорит, нужен ли сторож.
    pub(super) fn register_waker(&mut self, id: RangeRequestId, waker: &Waker) -> WatchdogDemand {
        if let Some(record) = self.records.get_mut(&id.get()) {
            record.register_waker(waker);
        }
        match self.watchdog {
            WatchdogStatus::Running => WatchdogDemand::AlreadyRunning,
            WatchdogStatus::Idle => {
                self.watchdog = WatchdogStatus::Running;
                WatchdogDemand::StartNow
            }
        }
    }

    /// Ближайший срок молчания среди асинхронно ждущих; `None` — сторож не нужен,
    /// и тогда он отмечается остановленным (под тем же замком, без гонки с запуском).
    pub(super) fn next_async_deadline_or_stop(
        &mut self,
        idle_timeout: Duration,
    ) -> Option<Instant> {
        let deadline = self
            .records
            .values()
            .filter_map(|record| record.async_deadline(idle_timeout))
            .min();
        if deadline.is_none() {
            self.watchdog = WatchdogStatus::Idle;
        }
        deadline
    }

    /// Сторож не запустился: отметить остановленным, чтобы следующий раз попробовать.
    pub(super) const fn watchdog_failed_to_start(&mut self) {
        self.watchdog = WatchdogStatus::Idle;
    }

    /// Потребитель ушёл.
    pub(super) fn abandon(&mut self, id: RangeRequestId, now: Instant) {
        let raw_id = id.get();
        let Some(record) = self.records.get_mut(&raw_id) else {
            return;
        };
        match record.abandon(now, &mut self.stats) {
            RecordRetention::Keep => {}
            RecordRetention::Forget => {
                self.records.remove(&raw_id);
            }
            RecordRetention::ForgetUnsent => {
                self.records.remove(&raw_id);
                self.outgoing.retain(|queued| *queued != raw_id);
            }
        }
    }

    /// Сколько байтов пришло по запросу.
    pub(super) fn received(&self, id: RangeRequestId) -> u64 {
        self.records.get(&id.get()).map_or(0, Record::received)
    }

    /// Запись запроса (для тестов).
    #[cfg(test)]
    pub(super) fn record(&self, id: RangeRequestId) -> Option<&Record> {
        self.records.get(&id.get())
    }
}
