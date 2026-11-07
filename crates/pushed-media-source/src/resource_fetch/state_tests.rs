//! Тесты чистой логики очереди: жизнь записи, строгость к странице, учёт.
//!
//! Время подставляется руками (`now`), поэтому сроки молчания проверяются без
//! настоящих пауз.

use std::num::{NonZeroU64, NonZeroUsize};
use std::time::{Duration, Instant};

use super::error::{ResourceFetchError, ResourceFetchViolation};
use super::record::{ChunkStep, RecordState, SlotHold, WakeList};
use super::request::{
    ExternalResourceHandle, ResourceByteRange, ResourceExtent, ResourceFetchSpec, ResourceRequest,
};
use super::requests::ResourceSealCut;
use super::state::{IssueStep, QueueState};
use super::writer::{
    ResourceFetchFailure, ResourceFetchOutcome, ResourceFrameVerdict, ResourceRequestClaim,
};
use crate::range_file::{RangeRequestId, RangeRequestIds};

/// Срок молчания в тестах.
const IDLE: Duration = Duration::from_secs(10);

/// Мест в полёте в тестах.
fn max_in_flight(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("non-zero")
}

/// Просьба «весь ресурс №`handle`, не больше `max_bytes`».
fn whole(handle: u32, max_bytes: u64) -> ResourceFetchSpec {
    ResourceFetchSpec {
        handle: ExternalResourceHandle::new(handle),
        extent: ResourceExtent::Whole,
        max_bytes: NonZeroU64::new(max_bytes).expect("non-zero"),
    }
}

/// Просьба «диапазон ресурса №`handle`».
fn ranged(handle: u32, offset: u64, length: u64, max_bytes: u64) -> ResourceFetchSpec {
    let range = ResourceByteRange::new(offset, NonZeroU64::new(length).expect("non-zero"))
        .expect("range fits");
    ResourceFetchSpec {
        handle: ExternalResourceHandle::new(handle),
        extent: ResourceExtent::Range(range),
        max_bytes: NonZeroU64::new(max_bytes).expect("non-zero"),
    }
}

/// Очередь, счётчик сеанса и «сейчас».
struct Fixture {
    state: QueueState,
    ids: RangeRequestIds,
    now: Instant,
}

impl Fixture {
    fn new() -> Self {
        Self {
            state: QueueState::new(),
            ids: RangeRequestIds::new(),
            now: Instant::now(),
        }
    }

    /// Выдаёт номер (места есть) — как `start` без ожидания.
    fn issue(&mut self, spec: ResourceFetchSpec) -> RangeRequestId {
        match self.try_issue(spec, 8) {
            IssueStep::Issued(id) => id,
            other => panic!("expected an issued request, got {other:?}"),
        }
    }

    fn try_issue(&mut self, spec: ResourceFetchSpec, slots: usize) -> IssueStep {
        let ticket = self.state.join_starters();
        let step = self
            .state
            .try_issue(ticket, spec, &self.ids, max_in_flight(slots));
        if matches!(step, IssueStep::Wait) {
            self.state.leave_starters(ticket);
        }
        step
    }

    /// Забирает запросы на отправку (будильники здесь не нужны).
    fn take(&mut self) -> Vec<ResourceRequest> {
        self.state.take_requests(self.now, &mut WakeList::default())
    }

    /// Выдаёт и сразу «отправляет» запрос.
    fn sent(&mut self, spec: ResourceFetchSpec) -> RangeRequestId {
        let id = self.issue(spec);
        let taken = self.take();
        assert_eq!(
            taken.iter().map(|request| request.id).collect::<Vec<_>>(),
            [id]
        );
        id
    }

    fn bytes(
        &mut self,
        id: RangeRequestId,
        offset: u64,
        payload: &[u8],
    ) -> Result<ResourceFrameVerdict, ResourceFetchViolation> {
        let mut wake = WakeList::default();
        self.state
            .accept_bytes(id, offset, payload, self.now, &self.ids, &mut wake)
    }

    fn finish(
        &mut self,
        id: RangeRequestId,
        outcome: ResourceFetchOutcome,
    ) -> Result<ResourceFrameVerdict, ResourceFetchViolation> {
        let mut wake = WakeList::default();
        self.state.finish(id, outcome, &self.ids, &mut wake)
    }

    fn sweep_at(&mut self, at: Instant) {
        let mut wake = WakeList::default();
        self.state.sweep_silent(at, IDLE, &mut wake);
    }

    /// Читает всё, что готово: тело и терминальный шаг (или `Pending`).
    fn drain(&mut self, id: RangeRequestId) -> (Vec<u8>, ChunkStep) {
        let mut body = Vec::new();
        loop {
            match self.state.next_chunk(id, IDLE) {
                ChunkStep::Chunk(chunk) => body.extend_from_slice(&chunk),
                terminal => return (body, terminal),
            }
        }
    }
}

fn completed(size_bytes: u64) -> ResourceFetchOutcome {
    ResourceFetchOutcome::Completed { size_bytes }
}

#[test]
fn whole_resource_arrives_byte_for_byte_and_ends_after_completed() {
    let mut fixture = Fixture::new();
    let id = fixture.sent(whole(7, 1_000));
    for (offset, chunk) in [(0, &b"#EXTM3U\n"[..]), (8, b"#EXT-X-"), (15, b"ENDLIST\n")] {
        assert_eq!(
            fixture.bytes(id, offset, chunk),
            Ok(ResourceFrameVerdict::Accepted)
        );
    }
    // До итога тело ещё не кончилось: прочитанное отдаётся, дальше — ждать.
    assert_eq!(
        fixture.drain(id),
        (b"#EXTM3U\n#EXT-X-ENDLIST\n".to_vec(), ChunkStep::Pending)
    );
    assert_eq!(
        fixture.finish(id, completed(23)),
        Ok(ResourceFrameVerdict::Accepted)
    );
    assert_eq!(fixture.drain(id), (Vec::new(), ChunkStep::End));
    // Конец повторяется, а не превращается в ошибку.
    assert_eq!(fixture.state.next_chunk(id, IDLE), ChunkStep::End);

    let stats = fixture.state.stats();
    assert_eq!((stats.issued, stats.sent, stats.completed), (1, 1, 1));
    assert_eq!(stats.bytes_accepted, 23);
    assert_eq!(stats.stale_frames, 0);
    // Итог отпустил место, хотя тело ещё держит хэндл.
    assert_eq!(fixture.state.in_flight(), 0);
}

#[test]
fn full_range_ends_cleanly_and_a_short_range_is_reported_after_its_body() {
    let mut fixture = Fixture::new();
    let full = fixture.sent(ranged(1, 500, 4, 1_000));
    fixture.bytes(full, 0, b"abcd").expect("bytes");
    fixture.finish(full, completed(4)).expect("finish");
    assert_eq!(fixture.drain(full), (b"abcd".to_vec(), ChunkStep::End));

    let short = fixture.sent(ranged(1, 900, 10, 1_000));
    fixture.bytes(short, 0, b"xyz").expect("bytes");
    // Короче диапазона — не нарушение: тело отдаётся, конец честно помечен.
    assert_eq!(
        fixture.finish(short, completed(3)),
        Ok(ResourceFrameVerdict::Accepted)
    );
    assert_eq!(
        fixture.drain(short),
        (
            b"xyz".to_vec(),
            ChunkStep::Failed(ResourceFetchError::ShortRange {
                requested: 10,
                received: 3
            })
        )
    );
    let stats = fixture.state.stats();
    assert_eq!(stats.completed, 2);
    assert_eq!(stats.failures.short_range, 1);
    assert_eq!(stats.failures.page_violations, 0);
}

#[test]
fn requests_finished_in_reverse_order_each_get_their_own_body() {
    let mut fixture = Fixture::new();
    let ids: Vec<RangeRequestId> = (1..=3)
        .map(|handle| fixture.issue(whole(handle, 100)))
        .collect();
    let taken = fixture.take();
    assert_eq!(
        taken.iter().map(|request| request.id).collect::<Vec<_>>(),
        ids
    );
    assert_eq!(
        taken
            .iter()
            .map(|request| request.handle.get())
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    for (index, id) in ids.iter().enumerate().rev() {
        let body = format!("body-{index}");
        fixture.bytes(*id, 0, body.as_bytes()).expect("bytes");
        fixture
            .finish(*id, completed(body.len() as u64))
            .expect("finish");
    }
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(
            fixture.drain(*id),
            (format!("body-{index}").into_bytes(), ChunkStep::End)
        );
    }
}

/// Сценарий нарушения: что сделать странице и какое нарушение ждать.
struct ViolationCase {
    name: &'static str,
    act: fn(&mut Fixture, RangeRequestId) -> Result<ResourceFrameVerdict, ResourceFetchViolation>,
    expected: fn(RangeRequestId) -> ResourceFetchViolation,
}

#[test]
fn every_page_violation_kills_the_queue_for_all_requests() {
    let cases = [
        ViolationCase {
            name: "first chunk not at zero",
            act: |fixture, id| fixture.bytes(id, 5, b"x"),
            expected: |id| ResourceFetchViolation::NonContiguousBytes {
                id: id.get(),
                expected: 0,
                actual: 5,
            },
        },
        ViolationCase {
            name: "gap between chunks",
            act: |fixture, id| {
                fixture.bytes(id, 0, b"ab").expect("first");
                fixture.bytes(id, 3, b"c")
            },
            expected: |id| ResourceFetchViolation::NonContiguousBytes {
                id: id.get(),
                expected: 2,
                actual: 3,
            },
        },
        ViolationCase {
            name: "overlapping chunk",
            act: |fixture, id| {
                fixture.bytes(id, 0, b"ab").expect("first");
                fixture.bytes(id, 1, b"bc")
            },
            expected: |id| ResourceFetchViolation::NonContiguousBytes {
                id: id.get(),
                expected: 2,
                actual: 1,
            },
        },
        ViolationCase {
            name: "body past max_bytes",
            act: |fixture, id| {
                fixture.bytes(id, 0, &[0; 8]).expect("fits exactly");
                fixture.bytes(id, 8, b"!")
            },
            expected: |id| ResourceFetchViolation::BytesBeyondLimit {
                id: id.get(),
                limit: 8,
            },
        },
        ViolationCase {
            name: "completed size differs from received",
            act: |fixture, id| {
                fixture.bytes(id, 0, b"abc").expect("bytes");
                fixture.finish(id, completed(4))
            },
            expected: |id| ResourceFetchViolation::SizeMismatch {
                id: id.get(),
                reported: 4,
                received: 3,
            },
        },
        ViolationCase {
            name: "second outcome",
            act: |fixture, id| {
                fixture.finish(id, completed(0)).expect("first outcome");
                fixture.finish(id, completed(0))
            },
            expected: |id| ResourceFetchViolation::AlreadyFinished { id: id.get() },
        },
        ViolationCase {
            name: "bytes after the outcome",
            act: |fixture, id| {
                fixture
                    .finish(
                        id,
                        ResourceFetchOutcome::Failed(ResourceFetchFailure::Refused),
                    )
                    .expect("outcome");
                fixture.bytes(id, 0, b"late")
            },
            expected: |id| ResourceFetchViolation::AlreadyFinished { id: id.get() },
        },
        ViolationCase {
            name: "bytes after the outcome and the reader left",
            act: |fixture, id| {
                fixture.finish(id, completed(0)).expect("outcome");
                fixture.state.abandon(id, fixture.now);
                fixture.bytes(id, 0, b"late")
            },
            expected: |id| ResourceFetchViolation::RequestNotOpen { id: id.get() },
        },
        ViolationCase {
            name: "number nobody issued",
            act: |fixture, id| fixture.bytes(RangeRequestId::new(id.get() + 50), 0, b"x"),
            expected: |id| ResourceFetchViolation::UnissuedRequest { id: id.get() + 50 },
        },
    ];
    for case in cases {
        let mut fixture = Fixture::new();
        let bystander = fixture.sent(whole(1, 100));
        fixture
            .bytes(bystander, 0, b"kept")
            .expect("bystander bytes");
        let victim = fixture.sent(whole(2, 8));
        let queued = fixture.issue(whole(3, 100));

        assert_eq!(
            (case.act)(&mut fixture, victim),
            Err((case.expected)(victim)),
            "{}",
            case.name
        );
        let violation = ResourceFetchError::PageViolation((case.expected)(victim));
        // Все остальные запросы получают то же нарушение, даже с уже пришедшими байтами.
        for other in [bystander, queued] {
            assert_eq!(
                fixture.state.next_chunk(other, IDLE),
                ChunkStep::Failed(violation.clone()),
                "{}",
                case.name
            );
        }
        // Первое нарушение побеждает: дальше та же ошибка, новые запросы не выдаются.
        assert_eq!(
            fixture.bytes(bystander, 4, b"more"),
            Err((case.expected)(victim)),
            "{}",
            case.name
        );
        assert!(fixture.take().is_empty(), "{}", case.name);
        assert!(
            matches!(
                fixture.try_issue(whole(4, 100), 8),
                IssueStep::Failed(ResourceFetchError::PageViolation(_))
            ),
            "{}",
            case.name
        );
        assert_eq!(
            fixture.state.stats().failures.page_violations,
            1,
            "{}",
            case.name
        );
    }
}

#[test]
fn range_length_caps_the_body_even_when_max_bytes_is_larger() {
    let mut fixture = Fixture::new();
    let id = fixture.sent(ranged(1, 0, 4, 1_000));
    assert_eq!(
        fixture.bytes(id, 0, b"abcde"),
        Err(ResourceFetchViolation::BytesBeyondLimit {
            id: id.get(),
            limit: 4
        })
    );
}

#[test]
fn bytes_for_a_request_not_yet_sent_are_a_violation() {
    let mut fixture = Fixture::new();
    let queued = fixture.issue(whole(1, 100));
    assert_eq!(
        fixture.bytes(queued, 0, b"x"),
        Err(ResourceFetchViolation::RequestNotOpen { id: queued.get() })
    );
}

#[test]
fn page_reported_failures_reach_the_engine_and_the_queue_keeps_working() {
    let cases = [
        (
            ResourceFetchFailure::Http { status: 403 },
            ResourceFetchError::Http { status: 403 },
        ),
        (ResourceFetchFailure::Refused, ResourceFetchError::Refused),
        (ResourceFetchFailure::TooLarge, ResourceFetchError::TooLarge),
        (ResourceFetchFailure::Network, ResourceFetchError::Network),
    ];
    let mut fixture = Fixture::new();
    for (failure, expected) in cases {
        let id = fixture.sent(whole(1, 100));
        assert_eq!(
            fixture.finish(id, ResourceFetchOutcome::Failed(failure)),
            Ok(ResourceFrameVerdict::Accepted)
        );
        assert_eq!(fixture.drain(id), (Vec::new(), ChunkStep::Failed(expected)));
        fixture.state.abandon(id, fixture.now);
        // Следующий запрос работает: отказ ресурса очередь не убивает.
        let next = fixture.sent(whole(2, 100));
        fixture.bytes(next, 0, b"ok").expect("bytes");
        fixture.finish(next, completed(2)).expect("finish");
        assert_eq!(fixture.drain(next), (b"ok".to_vec(), ChunkStep::End));
        fixture.state.abandon(next, fixture.now);
    }
    let failures = fixture.state.stats().failures;
    assert_eq!(
        (
            failures.http,
            failures.refused,
            failures.too_large,
            failures.network
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(failures.page_violations, 0);
    assert_eq!(fixture.state.stats().completed, 4);
}

#[test]
fn chunks_received_before_a_failure_are_delivered_before_the_error() {
    let mut fixture = Fixture::new();
    let id = fixture.sent(whole(1, 100));
    fixture.bytes(id, 0, b"part").expect("bytes");
    fixture
        .finish(
            id,
            ResourceFetchOutcome::Failed(ResourceFetchFailure::Network),
        )
        .expect("outcome");
    assert_eq!(
        fixture.drain(id),
        (
            b"part".to_vec(),
            ChunkStep::Failed(ResourceFetchError::Network)
        )
    );
}

#[test]
fn abandoned_unsent_request_disappears_and_frees_its_slot() {
    let mut fixture = Fixture::new();
    let dropped = fixture.issue(whole(1, 100));
    let kept = fixture.issue(whole(2, 100));
    assert_eq!(fixture.state.in_flight(), 2);
    fixture.state.abandon(dropped, fixture.now);
    assert_eq!(fixture.state.in_flight(), 1);
    let taken = fixture.take();
    assert_eq!(
        taken.iter().map(|request| request.id).collect::<Vec<_>>(),
        [kept]
    );
    assert_eq!(fixture.state.stats().abandoned, 1);
    assert_eq!(fixture.state.stats().sent, 1);
}

#[test]
fn abandoned_sent_request_drops_late_frames_and_holds_its_slot_until_the_page_finishes() {
    let mut fixture = Fixture::new();
    let id = fixture.sent(whole(1, 4));
    fixture.bytes(id, 0, b"ab").expect("bytes");
    fixture.state.abandon(id, fixture.now);
    // Страница ещё качает: место занято, иначе у неё было бы больше запросов, чем можно.
    assert_eq!(fixture.state.in_flight(), 1);
    assert!(matches!(fixture.try_issue(whole(2, 4), 1), IssueStep::Wait));
    // Поздние куски не сверяются (даже «неправильные») и не ломают очередь.
    assert_eq!(
        fixture.bytes(id, 99, b"garbage"),
        Ok(ResourceFrameVerdict::StaleDropped)
    );
    assert_eq!(
        fixture.finish(id, completed(5)),
        Ok(ResourceFrameVerdict::StaleDropped)
    );
    assert_eq!(fixture.state.in_flight(), 0);
    assert!(fixture.state.record(id).is_none());
    assert!(matches!(
        fixture.try_issue(whole(2, 4), 1),
        IssueStep::Issued(_)
    ));
    let stats = fixture.state.stats();
    assert_eq!((stats.abandoned, stats.stale_frames), (1, 2));
    assert_eq!(stats.bytes_accepted, 2);
    assert_eq!(stats.failures.page_violations, 0);
}

#[test]
fn silence_past_the_idle_timeout_expires_a_request_and_frees_its_slot() {
    let mut fixture = Fixture::new();
    let sent_at = fixture.now;
    let id = fixture.sent(whole(1, 100));
    // Кусок на середине срока продлевает жизнь: молчание считается от последнего куска.
    fixture.now = sent_at + IDLE / 2;
    fixture.bytes(id, 0, b"ab").expect("bytes");
    fixture.sweep_at(sent_at + IDLE);
    assert!(matches!(
        fixture.state.record(id).map(super::record::Record::state),
        Some(RecordState::Receiving { .. })
    ));
    fixture.sweep_at(sent_at + IDLE / 2 + IDLE);
    assert_eq!(fixture.state.in_flight(), 0);
    // Пришедшее до молчания отдаётся, потом — ошибка молчания (повторяемая).
    let (body, terminal) = fixture.drain(id);
    assert_eq!(body, b"ab");
    let ChunkStep::Failed(error) = terminal else {
        panic!("expected a failure, got {terminal:?}");
    };
    assert_eq!(error, ResourceFetchError::PageSilent { idle_timeout: IDLE });
    assert_eq!(
        error.failure_class(),
        super::error::ResourceFetchFailureClass::Transient
    );
    // Страница проснулась поздно: её рамки устарели, нарушения нет.
    assert_eq!(
        fixture.bytes(id, 2, b"cd"),
        Ok(ResourceFrameVerdict::StaleDropped)
    );
    assert_eq!(
        fixture.finish(id, completed(4)),
        Ok(ResourceFrameVerdict::StaleDropped)
    );
    assert_eq!(fixture.state.stats().failures.page_silent, 1);
}

#[test]
fn queued_requests_do_not_time_out_before_the_application_sends_them() {
    let mut fixture = Fixture::new();
    let id = fixture.issue(whole(1, 100));
    fixture.sweep_at(fixture.now + IDLE * 3);
    assert_eq!(
        fixture.state.record(id).map(super::record::Record::state),
        Some(RecordState::Queued)
    );
    assert_eq!(fixture.state.stats().failures.page_silent, 0);
}

#[test]
fn abandoned_request_releases_its_slot_when_the_page_stays_silent() {
    let mut fixture = Fixture::new();
    let id = fixture.sent(whole(1, 100));
    fixture.state.abandon(id, fixture.now);
    fixture.sweep_at(fixture.now + IDLE);
    assert_eq!(
        fixture.state.record(id).map(super::record::Record::state),
        Some(RecordState::Abandoned {
            slot: SlotHold::Released,
            last_activity: fixture.now
        })
    );
    assert_eq!(fixture.state.in_flight(), 0);
    // Брошенный потребителем запрос не считается «страница молчала» у движка.
    assert_eq!(fixture.state.stats().failures.page_silent, 0);
}

#[test]
fn seal_closes_waiting_requests_but_finished_bodies_and_received_chunks_are_read_out() {
    let mut fixture = Fixture::new();
    let finished = fixture.sent(whole(1, 100));
    fixture.bytes(finished, 0, b"done").expect("bytes");
    fixture.finish(finished, completed(4)).expect("finish");
    let receiving = fixture.sent(whole(2, 100));
    fixture.bytes(receiving, 0, b"half").expect("bytes");
    let abandoned = fixture.sent(whole(3, 100));
    fixture.state.abandon(abandoned, fixture.now);
    let queued = fixture.issue(whole(4, 100));

    let mut wake = WakeList::default();
    fixture.state.seal(&mut wake);

    assert_eq!(fixture.drain(finished), (b"done".to_vec(), ChunkStep::End));
    assert_eq!(
        fixture.drain(receiving),
        (
            b"half".to_vec(),
            ChunkStep::Failed(ResourceFetchError::SourceClosed)
        )
    );
    assert_eq!(
        fixture.drain(queued),
        (
            Vec::new(),
            ChunkStep::Failed(ResourceFetchError::SourceClosed)
        )
    );
    assert!(fixture.state.record(abandoned).is_none());
    assert!(fixture.take().is_empty());
    assert!(matches!(
        fixture.try_issue(whole(5, 100), 8),
        IssueStep::Failed(ResourceFetchError::SourceClosed)
    ));
    // Поздние рамки ушедшей вкладки безвредны.
    assert_eq!(
        fixture.bytes(receiving, 4, b"x"),
        Ok(ResourceFrameVerdict::StaleDropped)
    );
    assert_eq!(fixture.state.in_flight(), 0);
}

/// Печать сама по себе не «обрезает»: тела, пришедшие до неё целиком, дочитываются, и
/// признак остаётся `NoReadCut`. Обрезано — только когда движку после печати чего-то
/// не хватило (недочитанное тело).
#[test]
fn seal_cut_is_reported_only_when_a_read_after_the_seal_found_the_body_missing() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.state.seal_cut(), ResourceSealCut::NoReadCut);
    let finished = fixture.sent(whole(1, 100));
    fixture.bytes(finished, 0, b"done").expect("bytes");
    fixture.finish(finished, completed(4)).expect("finish");
    let receiving = fixture.sent(whole(2, 100));
    fixture.bytes(receiving, 0, b"half").expect("bytes");

    let mut wake = WakeList::default();
    fixture.state.seal(&mut wake);
    assert_eq!(
        fixture.state.seal_cut(),
        ResourceSealCut::NoReadCut,
        "the seal alone cuts nothing"
    );
    assert_eq!(fixture.drain(finished), (b"done".to_vec(), ChunkStep::End));
    assert_eq!(
        fixture.state.seal_cut(),
        ResourceSealCut::NoReadCut,
        "a body that arrived whole before the seal is not cut"
    );

    // Недочитанное тело: пришедшее отдано, дальше «закрыто» — это обрыв печатью.
    let (body, terminal) = fixture.drain(receiving);
    assert_eq!(body, b"half".to_vec());
    assert_eq!(
        terminal,
        ChunkStep::Failed(ResourceFetchError::SourceClosed)
    );
    assert_eq!(fixture.state.seal_cut(), ResourceSealCut::ReadCutBySeal);
}

/// Движок хочет новый ресурс после печати: его не будет — полученного не хватило.
#[test]
fn a_new_request_after_the_seal_is_a_read_cut_by_the_seal() {
    let mut fixture = Fixture::new();
    let mut wake = WakeList::default();
    fixture.state.seal(&mut wake);
    assert_eq!(fixture.state.seal_cut(), ResourceSealCut::NoReadCut);

    assert!(matches!(
        fixture.try_issue(whole(9, 100), 8),
        IssueStep::Failed(ResourceFetchError::SourceClosed)
    ));
    assert_eq!(fixture.state.seal_cut(), ResourceSealCut::ReadCutBySeal);
}

/// Без печати отказы ресурса (сеть, HTTP) обрывом печатью не считаются: обёртка
/// демуксера не должна прятать настоящие ошибки.
#[test]
fn failures_without_a_seal_are_never_a_seal_cut() {
    let mut fixture = Fixture::new();
    let failed = fixture.sent(whole(3, 100));
    fixture
        .finish(
            failed,
            ResourceFetchOutcome::Failed(ResourceFetchFailure::Network),
        )
        .expect("finish");
    assert_eq!(
        fixture.drain(failed),
        (Vec::new(), ChunkStep::Failed(ResourceFetchError::Network))
    );
    assert_eq!(fixture.state.seal_cut(), ResourceSealCut::NoReadCut);
}

#[test]
fn numbers_issued_to_someone_else_in_the_session_are_stale_and_not_ours() {
    let mut fixture = Fixture::new();
    // Номер 1 выдан «файлом» того же сеанса (тот же счётчик), номер 2 — очередью.
    let foreign = fixture.ids.issue().expect("file id");
    let ours = fixture.sent(whole(1, 100));
    assert_eq!(
        fixture.state.claim_of(foreign),
        ResourceRequestClaim::NotOurs
    );
    assert_eq!(fixture.state.claim_of(ours), ResourceRequestClaim::Ours);
    // Номер, выданный «файлом» после очереди, — тоже чужой.
    let later_foreign = fixture.ids.issue().expect("file id");
    assert_eq!(
        fixture.state.claim_of(later_foreign),
        ResourceRequestClaim::NotOurs
    );

    assert_eq!(
        fixture.bytes(foreign, 0, b"x"),
        Ok(ResourceFrameVerdict::StaleDropped)
    );
    assert_eq!(
        fixture.finish(later_foreign, completed(0)),
        Ok(ResourceFrameVerdict::StaleDropped)
    );
    // Чужие рамки не трогают свой запрос.
    fixture.bytes(ours, 0, b"mine").expect("own bytes");
    assert_eq!(fixture.state.received(ours), 4);
    assert_eq!(fixture.state.stats().stale_frames, 2);
    assert_eq!(fixture.state.stats().failures.page_violations, 0);
}

#[test]
fn bytes_for_one_request_do_not_change_another() {
    let mut fixture = Fixture::new();
    let first = fixture.sent(whole(1, 100));
    let second = fixture.sent(whole(2, 100));
    fixture.bytes(first, 0, b"12345").expect("bytes");
    fixture
        .finish(
            first,
            ResourceFetchOutcome::Failed(ResourceFetchFailure::Http { status: 404 }),
        )
        .expect("finish");
    assert_eq!(fixture.state.received(second), 0);
    assert_eq!(fixture.drain(second), (Vec::new(), ChunkStep::Pending));
    assert!(matches!(
        fixture
            .state
            .record(second)
            .map(super::record::Record::state),
        Some(RecordState::Receiving { .. })
    ));
}

#[test]
fn slots_are_given_in_arrival_order_and_only_when_free() {
    let mut fixture = Fixture::new();
    let occupying = fixture.issue(whole(1, 100));
    let early = fixture.state.join_starters();
    let late = fixture.state.join_starters();
    let slots = max_in_flight(2);
    // Место есть, но поздний не обгоняет раннего.
    assert!(matches!(
        fixture
            .state
            .try_issue(late, whole(3, 100), &fixture.ids, slots),
        IssueStep::Wait
    ));
    let IssueStep::Issued(early_id) =
        fixture
            .state
            .try_issue(early, whole(2, 100), &fixture.ids, slots)
    else {
        panic!("the first in line gets the free slot");
    };
    // Мест больше нет: поздний ждёт, пока страница не закончит один запрос.
    assert!(matches!(
        fixture
            .state
            .try_issue(late, whole(3, 100), &fixture.ids, slots),
        IssueStep::Wait
    ));
    fixture.take();
    fixture.finish(occupying, completed(0)).expect("finish");
    let IssueStep::Issued(late_id) =
        fixture
            .state
            .try_issue(late, whole(3, 100), &fixture.ids, slots)
    else {
        panic!("a finished request frees its slot");
    };
    assert!(early_id < late_id);
    assert_eq!(fixture.state.stats().issued, 3);
}
