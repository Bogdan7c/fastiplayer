//! Тесты ядра с потоками: ожидание места, отмена, сроки молчания в блокирующей и
//! асинхронной форме, закрытие, будильник без замков.

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use bytes::Bytes;
use source_core::CancellationToken;

use super::error::{ResourceFetchError, ResourceFetchViolation};
use super::policy::ResourceFetchPolicy;
use super::request::{ExternalResourceHandle, ResourceExtent, ResourceFetchSpec, ResourceRequest};
use super::writer::{ResourceFetchOutcome, ResourceFrameVerdict};
use super::{
    ResourceFetch, ResourceFetchClient, ResourceFetchRequests, ResourceFetchWriter,
    ResourceSealCut, resource_fetch_queue,
};
use crate::range_file::{RangeRequestId, RangeRequestIds, RangeRequestNotifier};

/// Сколько тест ждёт того, что должно случиться «сразу».
const PROMPT: Duration = Duration::from_secs(5);

/// Сколько тест ждёт, чтобы убедиться, что чего-то НЕ случилось.
const QUIET: Duration = Duration::from_millis(150);

/// Будильник, который считает вызовы.
#[derive(Default)]
struct CountingNotifier {
    calls: AtomicUsize,
}

impl RangeRequestNotifier for CountingNotifier {
    fn range_requested(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

/// Будильник, который сам забирает запросы — как главный цикл приложения.
///
/// Если бы ядро звало его под замком, `take_requests` повисло бы на том же замке.
#[derive(Default)]
struct DrainingNotifier {
    requests: OnceLock<ResourceFetchRequests>,
    taken: Mutex<Vec<ResourceRequest>>,
}

impl RangeRequestNotifier for DrainingNotifier {
    fn range_requested(&self) {
        if let Some(requests) = self.requests.get() {
            let fresh = requests.take_requests();
            self.taken.lock().expect("taken").extend(fresh);
        }
    }
}

fn policy(max_in_flight: usize, idle_timeout: Duration) -> ResourceFetchPolicy {
    ResourceFetchPolicy::new(
        NonZeroUsize::new(max_in_flight).expect("non-zero"),
        idle_timeout,
        NonZeroU64::new(1 << 20).expect("non-zero"),
    )
    .expect("policy")
}

fn whole(handle: u32) -> ResourceFetchSpec {
    ResourceFetchSpec {
        handle: ExternalResourceHandle::new(handle),
        extent: ResourceExtent::Whole,
        max_bytes: NonZeroU64::new(1_000).expect("non-zero"),
    }
}

struct Queue {
    writer: ResourceFetchWriter,
    client: ResourceFetchClient,
    requests: ResourceFetchRequests,
    notifier: Arc<CountingNotifier>,
}

fn queue(max_in_flight: usize, idle_timeout: Duration) -> Queue {
    let notifier = Arc::new(CountingNotifier::default());
    let (writer, client, requests) = resource_fetch_queue(
        policy(max_in_flight, idle_timeout),
        RangeRequestIds::new(),
        Arc::clone(&notifier) as Arc<dyn RangeRequestNotifier>,
    );
    Queue {
        writer,
        client,
        requests,
        notifier,
    }
}

fn ids_of(requests: &[ResourceRequest]) -> Vec<u32> {
    requests.iter().map(|request| request.id.get()).collect()
}

/// Читает тело блокирующе до конца.
fn read_body(fetch: &mut ResourceFetch) -> Result<Vec<u8>, ResourceFetchError> {
    let token = CancellationToken::new();
    let mut body = Vec::new();
    while let Some(chunk) = fetch.next_chunk_blocking(&token)? {
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[test]
fn ninth_start_waits_for_a_free_slot_and_gets_the_next_number() {
    let queue = queue(8, PROMPT);
    let token = CancellationToken::new();
    let fetches: Vec<ResourceFetch> = (1..=8)
        .map(|handle| queue.client.start(whole(handle), &token).expect("start"))
        .collect();
    assert_eq!(
        ids_of(&queue.requests.take_requests()),
        (1..=8).collect::<Vec<_>>()
    );

    let (sender, receiver) = mpsc::channel();
    let client = queue.client.clone();
    let waiter = thread::spawn(move || {
        let token = CancellationToken::new();
        let fetch = client.start(whole(9), &token);
        sender.send(fetch).expect("send");
    });
    // Девятый ждёт: мест нет, и отказом это не кончается.
    assert!(receiver.recv_timeout(QUIET).is_err());
    assert!(queue.requests.take_requests().is_empty());

    let third = fetches[2].id();
    queue
        .writer
        .finish(third, ResourceFetchOutcome::Completed { size_bytes: 0 })
        .expect("finish");
    let ninth = receiver
        .recv_timeout(PROMPT)
        .expect("ninth started")
        .expect("ok");
    assert_eq!(ninth.id(), RangeRequestId::new(9));
    let ninth_requests = queue.requests.take_requests();
    assert_eq!(ids_of(&ninth_requests), [9]);
    assert_eq!(ninth_requests[0].handle, ExternalResourceHandle::new(9));
    assert_eq!(queue.notifier.calls.load(Ordering::SeqCst), 9);
    waiter.join().expect("waiter");
    drop(fetches);
}

#[test]
fn cancelled_waiting_start_leaves_the_line_and_the_next_one_proceeds() {
    let queue = queue(1, PROMPT);
    let token = CancellationToken::new();
    let first = queue.client.start(whole(1), &token).expect("start");
    assert!(!queue.requests.take_requests().is_empty());

    let cancel_early = CancellationToken::new();
    let (early_sender, early_receiver) = mpsc::channel();
    let early_client = queue.client.clone();
    let early_token = cancel_early.clone();
    let early = thread::spawn(move || {
        early_sender
            .send(
                early_client
                    .start(whole(2), &early_token)
                    .map(|fetch| fetch.id()),
            )
            .expect("send");
    });
    // Ранний встал в очередь первым.
    thread::sleep(QUIET);
    let (late_sender, late_receiver) = mpsc::channel();
    let late_client = queue.client.clone();
    let late = thread::spawn(move || {
        let token = CancellationToken::new();
        let fetch = late_client.start(whole(3), &token).expect("late start");
        late_sender.send(fetch.id()).expect("send");
        // Хэндл живёт до проверки: иначе место освободилось бы сразу.
        thread::sleep(QUIET);
    });
    thread::sleep(QUIET);

    let cancelled_at = Instant::now();
    cancel_early.cancel();
    assert_eq!(
        early_receiver.recv_timeout(PROMPT).expect("early answered"),
        Err(ResourceFetchError::Cancelled)
    );
    assert!(cancelled_at.elapsed() < Duration::from_secs(1));
    // Отменённый не держит очередь: поздний получает место, как только оно освободится.
    queue
        .writer
        .finish(
            first.id(),
            ResourceFetchOutcome::Completed { size_bytes: 0 },
        )
        .expect("finish");
    assert_eq!(
        late_receiver.recv_timeout(PROMPT).expect("late started"),
        RangeRequestId::new(2)
    );
    early.join().expect("early");
    late.join().expect("late");
    drop(first);
}

#[test]
fn blocking_reader_gets_chunks_written_from_another_thread() {
    let queue = queue(8, PROMPT);
    let token = CancellationToken::new();
    let mut fetch = queue.client.start(whole(1), &token).expect("start");
    let id = fetch.id();
    assert!(!queue.requests.take_requests().is_empty());
    let writer = queue.writer;
    let page = thread::spawn(move || {
        for (offset, chunk) in [(0, &b"seg"[..]), (3, b"ment")] {
            thread::sleep(Duration::from_millis(20));
            assert_eq!(
                writer.accept_bytes(id, offset, chunk),
                Ok(ResourceFrameVerdict::Accepted)
            );
        }
        writer
            .finish(id, ResourceFetchOutcome::Completed { size_bytes: 7 })
            .expect("finish");
        writer
    });
    assert_eq!(read_body(&mut fetch), Ok(b"segment".to_vec()));
    assert_eq!(fetch.received_bytes(), 7);
    drop(page.join().expect("page"));
}

#[test]
fn blocking_reader_fails_with_page_silent_after_the_idle_timeout() {
    let idle = Duration::from_millis(60);
    let queue = queue(8, idle);
    let token = CancellationToken::new();
    let mut fetch = queue.client.start(whole(1), &token).expect("start");
    assert!(!queue.requests.take_requests().is_empty());
    cancel_if_stuck(&token);
    let started = Instant::now();
    assert_eq!(
        fetch.next_chunk_blocking(&token),
        Err(ResourceFetchError::PageSilent { idle_timeout: idle })
    );
    let waited = started.elapsed();
    assert!(waited >= idle, "{waited:?}");
    assert!(waited < Duration::from_secs(2), "{waited:?}");
    assert_eq!(queue.requests.stats().failures.page_silent, 1);
}

#[test]
fn blocking_reader_is_cancelled_promptly() {
    let queue = queue(8, PROMPT);
    let token = CancellationToken::new();
    let mut fetch = queue.client.start(whole(1), &token).expect("start");
    assert!(!queue.requests.take_requests().is_empty());
    let cancel = token.clone();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(30));
        cancel.cancel();
    });
    let started = Instant::now();
    assert_eq!(
        fetch.next_chunk_blocking(&token),
        Err(ResourceFetchError::Cancelled)
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    canceller.join().expect("canceller");
}

/// Будильник ручного исполнителя: будит поток теста и считает побудки.
struct ThreadWaker {
    thread: Thread,
    wakes: AtomicUsize,
}

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.wakes.fetch_add(1, Ordering::SeqCst);
        self.thread.unpark();
    }
}

/// Ручной исполнитель: опрашивает кусок, пока не готов; спит до побудки.
///
/// Спит не дольше `PROMPT`: если бы никто не будил, тест увидел бы это по времени.
fn poll_until_ready(
    fetch: &mut ResourceFetch,
    waker: &Arc<ThreadWaker>,
) -> Result<Option<Bytes>, ResourceFetchError> {
    let task_waker = Waker::from(Arc::clone(waker));
    let mut context = Context::from_waker(&task_waker);
    // Сломанный механизм должен ронять тест, а не вешать его.
    let give_up_at = Instant::now() + PROMPT * 3;
    loop {
        let poll = fetch.poll_next_chunk(&mut context);
        if let Poll::Ready(ready) = poll {
            return ready;
        }
        assert!(Instant::now() < give_up_at, "the fetch never became ready");
        thread::park_timeout(PROMPT);
    }
}

/// Отменяет `token` через `PROMPT`: зависшее ожидание кончится `Cancelled`, а не
/// повесит тест. Поток отсоединён и безвреден после конца теста.
fn cancel_if_stuck(token: &CancellationToken) {
    let token = token.clone();
    thread::spawn(move || {
        thread::sleep(PROMPT);
        token.cancel();
    });
}

fn thread_waker() -> Arc<ThreadWaker> {
    Arc::new(ThreadWaker {
        thread: thread::current(),
        wakes: AtomicUsize::new(0),
    })
}

#[test]
fn async_reader_is_woken_by_the_writer_for_every_chunk_and_the_end() {
    let queue = queue(8, PROMPT);
    let token = CancellationToken::new();
    let mut fetch = queue.client.start(whole(1), &token).expect("start");
    let id = fetch.id();
    assert!(!queue.requests.take_requests().is_empty());
    let waker = thread_waker();
    let task_waker = Waker::from(Arc::clone(&waker));
    let mut context = Context::from_waker(&task_waker);
    assert!(fetch.poll_next_chunk(&mut context).is_pending());

    let writer = queue.writer;
    let page = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        writer.accept_bytes(id, 0, b"key!").expect("bytes");
        thread::sleep(Duration::from_millis(20));
        writer
            .finish(id, ResourceFetchOutcome::Completed { size_bytes: 4 })
            .expect("finish");
        writer
    });
    let started = Instant::now();
    assert_eq!(
        poll_until_ready(&mut fetch, &waker),
        Ok(Some(Bytes::from_static(b"key!")))
    );
    assert_eq!(poll_until_ready(&mut fetch, &waker), Ok(None));
    // Будили писатель, а не таймаут ручного исполнителя.
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(waker.wakes.load(Ordering::SeqCst) >= 2);
    drop(page.join().expect("page"));
}

#[test]
fn async_reader_is_woken_by_the_silence_watchdog() {
    let idle = Duration::from_millis(60);
    let queue = queue(8, idle);
    let token = CancellationToken::new();
    let mut fetch = queue.client.start(whole(1), &token).expect("start");
    assert!(!queue.requests.take_requests().is_empty());
    let waker = thread_waker();
    let started = Instant::now();
    assert_eq!(
        poll_until_ready(&mut fetch, &waker),
        Err(ResourceFetchError::PageSilent { idle_timeout: idle })
    );
    let waited = started.elapsed();
    assert!(waited >= idle, "{waited:?}");
    // Без сторожа ручной исполнитель проспал бы `PROMPT` (5 с).
    assert!(waited < Duration::from_secs(2), "{waited:?}");
    assert!(waker.wakes.load(Ordering::SeqCst) >= 1);
}

#[test]
fn async_reader_that_polled_before_the_send_is_still_woken_by_the_silence_watchdog() {
    // Порядок из жизни: движок опрашивает тело раньше, чем главный цикл забрал
    // запрос. Тогда срока у записи ещё нет, и сторож останавливается сразу.
    let idle = Duration::from_millis(60);
    let queue = queue(8, idle);
    let token = CancellationToken::new();
    cancel_if_stuck(&token);
    let mut fetch = queue.client.start(whole(1), &token).expect("start");
    let waker = thread_waker();
    let task_waker = Waker::from(Arc::clone(&waker));
    let mut context = Context::from_waker(&task_waker);
    assert!(fetch.poll_next_chunk(&mut context).is_pending());
    // Сторож успевает увидеть «ждать некого» и закончиться.
    thread::sleep(idle / 2);
    assert_eq!(queue.requests.take_requests().len(), 1);

    // Настоящий исполнитель НЕ опрашивает снова без побудки: ждём только её.
    let started = Instant::now();
    while waker.wakes.load(Ordering::SeqCst) == 0 && started.elapsed() < PROMPT / 5 {
        thread::park_timeout(idle);
    }
    assert!(
        waker.wakes.load(Ordering::SeqCst) >= 1,
        "the reader that polled before the send was never woken"
    );
    assert_eq!(
        poll_until_ready(&mut fetch, &waker),
        Err(ResourceFetchError::PageSilent { idle_timeout: idle })
    );
    // Молчание признал сторож, а не отмена `cancel_if_stuck` (через `PROMPT`).
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn watchdog_handles_several_async_waiters_with_different_deadlines() {
    let idle = Duration::from_millis(80);
    let queue = queue(8, idle);
    let token = CancellationToken::new();
    let mut early = queue.client.start(whole(1), &token).expect("start");
    assert!(!queue.requests.take_requests().is_empty());
    thread::sleep(Duration::from_millis(40));
    let mut late = queue.client.start(whole(2), &token).expect("start");
    assert!(!queue.requests.take_requests().is_empty());
    let waker = thread_waker();
    // Сначала регистрируется поздний, потом ранний (с более ранним сроком).
    let task_waker = Waker::from(Arc::clone(&waker));
    let mut context = Context::from_waker(&task_waker);
    assert!(late.poll_next_chunk(&mut context).is_pending());
    let started = Instant::now();
    assert_eq!(
        poll_until_ready(&mut early, &waker),
        Err(ResourceFetchError::PageSilent { idle_timeout: idle })
    );
    assert_eq!(
        poll_until_ready(&mut late, &waker),
        Err(ResourceFetchError::PageSilent { idle_timeout: idle })
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(queue.requests.stats().failures.page_silent, 2);
}

#[test]
fn seal_wakes_blocked_readers_and_waiting_starts_with_source_closed() {
    let queue = queue(1, PROMPT);
    let token = CancellationToken::new();
    let mut fetch = queue.client.start(whole(1), &token).expect("start");
    let id = fetch.id();
    assert!(!queue.requests.take_requests().is_empty());
    queue.writer.accept_bytes(id, 0, b"got").expect("bytes");

    let (sender, receiver) = mpsc::channel();
    let client = queue.client.clone();
    let waiter = thread::spawn(move || {
        let token = CancellationToken::new();
        sender
            .send(client.start(whole(2), &token).map(|fetch| fetch.id()))
            .expect("send");
    });
    assert!(receiver.recv_timeout(QUIET).is_err());

    // Уничтожение писателя = seal.
    drop(queue.writer);
    assert_eq!(
        receiver.recv_timeout(PROMPT).expect("waiter answered"),
        Err(ResourceFetchError::SourceClosed)
    );
    // Пришедшее до закрытия отдаётся, дальше — «закрыто».
    assert_eq!(
        fetch.next_chunk_blocking(&token),
        Ok(Some(Bytes::from_static(b"got")))
    );
    assert_eq!(
        fetch.next_chunk_blocking(&token),
        Err(ResourceFetchError::SourceClosed)
    );
    assert!(matches!(
        queue.client.start(whole(3), &token),
        Err(ResourceFetchError::SourceClosed)
    ));
    waiter.join().expect("waiter");
    // Сторона приложения видит обрыв печатью: обёртка демуксера доиграет полученное.
    assert_eq!(queue.requests.seal_cut(), ResourceSealCut::ReadCutBySeal);
}

#[test]
fn page_violation_wakes_every_blocked_reader_with_the_same_error() {
    let queue = queue(8, PROMPT);
    let token = CancellationToken::new();
    let mut bystander = queue.client.start(whole(1), &token).expect("start");
    let victim = queue.client.start(whole(2), &token).expect("start");
    assert!(!queue.requests.take_requests().is_empty());
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let token = CancellationToken::new();
        sender
            .send(bystander.next_chunk_blocking(&token))
            .expect("send");
    });
    assert!(receiver.recv_timeout(QUIET).is_err());
    let violation = ResourceFetchViolation::NonContiguousBytes {
        id: victim.id().get(),
        expected: 0,
        actual: 1,
    };
    assert_eq!(
        queue.writer.accept_bytes(victim.id(), 1, b"x"),
        Err(violation.clone())
    );
    assert_eq!(
        receiver.recv_timeout(PROMPT).expect("reader answered"),
        Err(ResourceFetchError::PageViolation(violation))
    );
    reader.join().expect("reader");
}

#[test]
fn notifier_runs_without_locks_and_may_take_requests_right_away() {
    let notifier = Arc::new(DrainingNotifier::default());
    let (_writer, client, requests) = resource_fetch_queue(
        policy(8, PROMPT),
        RangeRequestIds::new(),
        Arc::clone(&notifier) as Arc<dyn RangeRequestNotifier>,
    );
    assert!(notifier.requests.set(requests.clone()).is_ok());
    let token = CancellationToken::new();
    let first = client.start(whole(11), &token).expect("start");
    let second = client.start(whole(12), &token).expect("start");
    let taken = notifier.taken.lock().expect("taken").clone();
    assert_eq!(ids_of(&taken), [first.id().get(), second.id().get()]);
    assert_eq!(
        taken
            .iter()
            .map(|request| request.handle.get())
            .collect::<Vec<_>>(),
        [11, 12]
    );
    assert_eq!(requests.stats().sent, 2);
}

#[test]
fn start_beyond_the_policy_ceiling_is_refused_without_issuing_a_number() {
    let queue = queue(8, PROMPT);
    let token = CancellationToken::new();
    let oversized = ResourceFetchSpec {
        max_bytes: NonZeroU64::new((1 << 20) + 1).expect("non-zero"),
        ..whole(1)
    };
    assert_eq!(
        queue
            .client
            .start(oversized, &token)
            .map(|fetch| fetch.id()),
        Err(ResourceFetchError::ExceedsResourceCeiling {
            requested: (1 << 20) + 1,
            ceiling: 1 << 20
        })
    );
    assert_eq!(queue.requests.stats().issued, 0);
    assert_eq!(queue.notifier.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn dropping_an_unsent_fetch_removes_it_from_outgoing_and_wakes_a_waiting_start() {
    let queue = queue(1, PROMPT);
    let token = CancellationToken::new();
    let unsent = queue.client.start(whole(1), &token).expect("start");
    let (sender, receiver) = mpsc::channel();
    let client = queue.client.clone();
    let waiter = thread::spawn(move || {
        let token = CancellationToken::new();
        let fetch = client.start(whole(2), &token).expect("start");
        sender.send(fetch.id()).expect("send");
        fetch
    });
    assert!(receiver.recv_timeout(QUIET).is_err());
    drop(unsent);
    assert_eq!(
        receiver.recv_timeout(PROMPT).expect("started"),
        RangeRequestId::new(2)
    );
    assert_eq!(ids_of(&queue.requests.take_requests()), [2]);
    drop(waiter.join().expect("waiter"));
}
