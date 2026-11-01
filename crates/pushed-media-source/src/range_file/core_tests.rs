//! Тесты ядра с потоками: ожидания, будильник, seal, отмена, ошибки страницы.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use source_core::{ByteSource, CancellationToken, SourceError};

use super::core::FileCore;
use super::error::{PushedFileError, PushedFileFailureClass};
use super::ids::{RangeRequest, RangeRequestIds, RangeRequestNotifier};
use super::opener::FileOpenProgress;
use super::policy::PushedFilePolicy;
use super::pushed_file;
use super::retry::RangeRetryPolicy;
use super::source::RangeFileSource;
use super::writer::{
    PushedFileRequests, PushedFileWriter, RangeFailure, RangeOutcome, ResourceSizeReport,
};

/// Сколько тест ждёт того, что должно случиться «сразу».
const PROMPT: Duration = Duration::from_secs(5);

/// Будильник, который сам забирает запросы — как главный цикл приложения.
///
/// Если бы ядро звало его под замком, `take_requests` повисло бы на том же замке:
/// тест тогда не завершился бы.
#[derive(Default)]
struct DrainingNotifier {
    requests: OnceLock<PushedFileRequests>,
    seen: Mutex<Vec<RangeRequest>>,
    calls: AtomicUsize,
}

impl DrainingNotifier {
    fn attach(&self, requests: PushedFileRequests) {
        self.requests.set(requests).ok();
    }

    fn drained(&self) -> Vec<RangeRequest> {
        self.seen.lock().expect("seen").clone()
    }

    /// Ждёт, пока будильник заберёт хотя бы `count` запросов.
    fn wait_for(&self, count: usize) -> Vec<RangeRequest> {
        let deadline = Instant::now() + PROMPT;
        loop {
            let seen = self.drained();
            if seen.len() >= count {
                return seen;
            }
            assert!(
                Instant::now() < deadline,
                "expected {count} requests, saw {seen:?}"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
}

impl RangeRequestNotifier for DrainingNotifier {
    fn range_requested(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(requests) = self.requests.get() {
            self.seen
                .lock()
                .expect("seen")
                .extend(requests.take_requests());
        }
    }
}

/// Политика с запросом в 64 байта: читатель теста ходит маленькими порциями.
fn small_policy(retry: RangeRetryPolicy) -> PushedFilePolicy {
    let prefetch = media_prefetch::PrefetchConfig::new(16, 64, 256).expect("prefetch");
    PushedFilePolicy::new(prefetch, retry).expect("policy")
}

/// Ядро с писателем и подслушивающим будильником.
fn setup(retry: RangeRetryPolicy) -> (Arc<FileCore>, PushedFileWriter, Arc<DrainingNotifier>) {
    let notifier = Arc::new(DrainingNotifier::default());
    let policy = small_policy(retry);
    let (writer, _opener, requests) = pushed_file(
        policy,
        RangeRequestIds::new(),
        Arc::clone(&notifier) as Arc<dyn RangeRequestNotifier>,
    );
    // Ядро нужно тестам напрямую: берём его из очереди запросов.
    let core = requests.core_for_tests();
    notifier.attach(requests);
    (core, writer, notifier)
}

/// Читатель в отдельном потоке: возвращает всё прочитанное до конца или ошибки.
fn spawn_reader(
    core: &Arc<FileCore>,
    content_length: Option<u64>,
    cancellation: CancellationToken,
) -> thread::JoinHandle<Result<Vec<u8>, SourceError>> {
    let mut source = RangeFileSource::new(Arc::clone(core), content_length);
    thread::spawn(move || {
        let mut all = Vec::new();
        let mut buffer = [0_u8; 64];
        loop {
            match source.read(&mut buffer, &cancellation)? {
                0 => return Ok(all),
                count => all.extend_from_slice(&buffer[..count]),
            }
        }
    })
}

fn join_promptly<T>(handle: thread::JoinHandle<T>) -> T {
    let deadline = Instant::now() + PROMPT;
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "the reader hangs");
        thread::sleep(Duration::from_millis(2));
    }
    handle.join().expect("reader thread")
}

#[test]
fn a_blocked_read_wakes_with_the_bytes_the_page_sends_and_each_request_calls_the_notifier() {
    let (core, mut writer, notifier) = setup(RangeRetryPolicy::disabled());
    let reader = spawn_reader(&core, Some(150), CancellationToken::new());
    // Читатель просит файл с нуля порциями; страница отвечает на каждый запрос.
    let file: Vec<u8> = (0..150_u32)
        .map(|index| u8::try_from(index % 200).expect("fits"))
        .collect();
    let mut answered = 0;
    while answered < 3 {
        let requests = notifier.wait_for(answered + 1);
        let request = requests[answered];
        let start = usize::try_from(request.offset).expect("offset");
        let end = start + request.length.get() as usize;
        for (index, frame) in file[start..end].chunks(5).enumerate() {
            writer
                .accept_range_bytes(request.id, request.offset + (index * 5) as u64, frame)
                .expect("frame");
        }
        writer
            .finish_request(
                request.id,
                RangeOutcome::Completed {
                    resource_size: ResourceSizeReport::Known(150),
                },
            )
            .expect("finish");
        answered += 1;
        if end >= 150 {
            break;
        }
    }
    assert_eq!(join_promptly(reader).expect("read to the end"), file);
    // Будильник сработал ровно по разу на каждый запрос (запросов не потеряно).
    let seen = notifier.drained();
    assert_eq!(notifier.calls.load(Ordering::SeqCst), seen.len());
    assert_eq!(seen[0].offset, 0);
}

#[test]
fn seal_wakes_a_blocked_read_with_the_end_of_the_file_after_what_arrived() {
    let (core, mut writer, notifier) = setup(RangeRetryPolicy::disabled());
    let reader = spawn_reader(&core, Some(1000), CancellationToken::new());
    let request = notifier.wait_for(1)[0];
    writer
        .accept_range_bytes(request.id, request.offset, &[3; 20])
        .expect("frame");
    // Страница ушла посреди запроса.
    writer.seal();
    let bytes = join_promptly(reader).expect("clean end of file");
    assert_eq!(bytes, vec![3; 20], "what arrived is still delivered");
    assert_eq!(notifier.drained().len(), 1, "no new requests after seal");
}

#[test]
fn dropping_the_writer_is_a_seal() {
    let (core, writer, notifier) = setup(RangeRetryPolicy::disabled());
    let reader = spawn_reader(&core, Some(1000), CancellationToken::new());
    notifier.wait_for(1);
    drop(writer);
    assert_eq!(join_promptly(reader).expect("clean end"), Vec::<u8>::new());
}

#[test]
fn cancellation_wakes_a_blocked_read() {
    let (core, _writer, notifier) = setup(RangeRetryPolicy::disabled());
    let cancellation = CancellationToken::new();
    let reader = spawn_reader(&core, Some(1000), cancellation.clone());
    notifier.wait_for(1);
    cancellation.cancel();
    assert!(matches!(join_promptly(reader), Err(SourceError::Cancelled)));
}

#[test]
fn a_page_violation_wakes_the_blocked_read_with_the_root_cause() {
    let (core, mut writer, notifier) = setup(RangeRetryPolicy::disabled());
    let reader = spawn_reader(&core, Some(1000), CancellationToken::new());
    let request = notifier.wait_for(1)[0];
    // Рамка не с того смещения.
    let violation = writer
        .accept_range_bytes(request.id, request.offset + 9, b"x")
        .expect_err("violation");
    assert_eq!(
        violation.failure_class(),
        PushedFileFailureClass::InputMisuse
    );
    let error = join_promptly(reader).expect_err("the read fails instead of hanging");
    let SourceError::LocalIo { source, .. } = error else {
        panic!("unexpected error kind: {error:?}");
    };
    let root = source
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<PushedFileError>())
        .expect("the file error stays in the chain");
    assert_eq!(root, &violation);
    assert_eq!(
        writer.failure_class(),
        Some(PushedFileFailureClass::InputMisuse)
    );
}

#[test]
fn network_failure_is_retried_and_the_file_still_plays() {
    let retry = RangeRetryPolicy::with_budget(Duration::from_secs(5))
        .with_delays(Duration::from_millis(5), Duration::from_millis(5));
    let notifier = Arc::new(DrainingNotifier::default());
    let policy = small_policy(retry);
    let (mut writer, _opener, requests) = pushed_file(
        policy,
        RangeRequestIds::new(),
        Arc::clone(&notifier) as Arc<dyn RangeRequestNotifier>,
    );
    let core = requests.core_for_tests();
    notifier.attach(requests);
    let reader = spawn_reader(&core, Some(10), CancellationToken::new());
    let first = notifier.wait_for(1)[0];
    writer
        .accept_range_bytes(first.id, 0, b"abcd")
        .expect("frame");
    writer
        .finish_request(first.id, RangeOutcome::Failed(RangeFailure::Network))
        .expect("retry scheduled");
    // Повтор — новый запрос с первого недостающего байта.
    let retry_request = notifier.wait_for(2)[1];
    assert_eq!((retry_request.offset, retry_request.length.get()), (4, 60));
    writer
        .accept_range_bytes(retry_request.id, 4, b"efghij")
        .expect("frame");
    writer
        .finish_request(
            retry_request.id,
            RangeOutcome::Completed {
                resource_size: ResourceSizeReport::Known(10),
            },
        )
        .expect("finish");
    assert_eq!(join_promptly(reader).expect("played"), b"abcdefghij");
}

#[test]
fn seal_before_the_file_opened_ends_the_open_with_an_unavailable_error() {
    let notifier = Arc::new(DrainingNotifier::default());
    let (mut writer, mut opener, requests) = pushed_file(
        PushedFilePolicy::recommended().expect("policy"),
        RangeRequestIds::new(),
        Arc::clone(&notifier) as Arc<dyn RangeRequestNotifier>,
    );
    notifier.attach(requests);
    assert!(matches!(opener.poll_open(), FileOpenProgress::Pending));
    notifier.wait_for(1);
    writer.seal();
    let deadline = Instant::now() + PROMPT;
    let error = loop {
        match opener.poll_open() {
            FileOpenProgress::Failed(error) => break error,
            FileOpenProgress::Pending => {
                assert!(Instant::now() < deadline, "the open waits forever");
                thread::sleep(Duration::from_millis(2));
            }
            FileOpenProgress::Ready(_) => panic!("nothing to open"),
        }
    };
    assert_eq!(error, PushedFileError::SealedBeforeOpen);
    assert_eq!(
        error.failure_class(),
        PushedFileFailureClass::SourceUnavailable
    );
    assert!(matches!(
        opener.poll_open(),
        FileOpenProgress::Failed(PushedFileError::AlreadyOpened)
    ));
}

#[test]
fn a_violation_during_open_ends_the_open_with_input_misuse() {
    let notifier = Arc::new(DrainingNotifier::default());
    let (mut writer, mut opener, requests) = pushed_file(
        PushedFilePolicy::recommended().expect("policy"),
        RangeRequestIds::new(),
        Arc::clone(&notifier) as Arc<dyn RangeRequestNotifier>,
    );
    notifier.attach(requests);
    let _ = opener.poll_open();
    let request = notifier.wait_for(1)[0];
    writer
        .accept_range_bytes(request.id, 100, b"wrong offset")
        .expect_err("violation");
    let deadline = Instant::now() + PROMPT;
    loop {
        match opener.poll_open() {
            FileOpenProgress::Failed(error) => {
                assert_eq!(error.failure_class(), PushedFileFailureClass::InputMisuse);
                break;
            }
            FileOpenProgress::Pending => {
                assert!(Instant::now() < deadline, "the open waits forever");
                thread::sleep(Duration::from_millis(2));
            }
            FileOpenProgress::Ready(_) => panic!("garbage cannot open"),
        }
    }
}

#[test]
fn the_source_reports_size_seekability_and_refuses_outside_seeks_without_a_size() {
    let (core, _writer, _notifier) = setup(RangeRetryPolicy::disabled());
    let mut known = RangeFileSource::new(Arc::clone(&core), Some(500));
    assert_eq!(known.content_length(), Some(500));
    assert!(known.seekability().is_seekable());
    known.seek(300).expect("seek");
    assert_eq!(known.position(), 300);

    let mut unknown = RangeFileSource::new(core, None);
    assert_eq!(unknown.content_length(), None);
    assert!(!unknown.seekability().is_seekable());
    assert!(matches!(
        unknown.seek(5),
        Err(SourceError::NotSeekable { .. })
    ));
    assert_eq!(unknown.position(), 0, "a refused seek moves nothing");
}

/// Решение 150 для `<video src>`: пока запрос к странице в пути, файл «ждёт живую
/// страницу», и счётчик принесённого растёт с каждой рамкой (решение 151); законченный
/// запрос и `seal` — уже не ожидание страницы.
#[test]
fn an_outstanding_range_request_is_a_live_page_wait_with_growing_progress() {
    use media_core::{DemuxDataWait, LiveProducerProgress};

    let (core, mut writer, notifier) = setup(RangeRetryPolicy::disabled());
    assert_eq!(
        core.data_wait(),
        DemuxDataWait::Unattributed,
        "nothing requested yet"
    );
    let reader = spawn_reader(&core, Some(1000), CancellationToken::new());
    let request = notifier.wait_for(1)[0];
    let live = |bytes| DemuxDataWait::LiveProducer {
        progress: LiveProducerProgress::from_delivered_bytes(bytes),
    };
    assert_eq!(core.data_wait(), live(0));
    let length = u64::from(request.length.get());
    writer
        .accept_range_bytes(request.id, request.offset, &[7_u8; 5])
        .expect("frame");
    assert_eq!(core.data_wait(), live(5));
    let rest = vec![7_u8; usize::try_from(length - 5).expect("length")];
    writer
        .accept_range_bytes(request.id, request.offset + 5, &rest)
        .expect("the rest of the answer");
    assert_eq!(core.data_wait(), live(length));
    writer
        .finish_request(
            request.id,
            RangeOutcome::Completed {
                resource_size: ResourceSizeReport::Known(1000),
            },
        )
        .expect("finish");
    // Читатель забрал байты и попросил продолжение — снова ждём страницу.
    let next = notifier.wait_for(2)[1];
    assert_eq!(core.data_wait(), live(length));
    writer
        .accept_range_bytes(next.id, next.offset, &[7_u8; 3])
        .expect("frame");
    assert_eq!(core.data_wait(), live(length + 3));
    writer.seal();
    assert_eq!(
        core.data_wait(),
        DemuxDataWait::Unattributed,
        "the tab is gone"
    );
    drop(join_promptly(reader));
}
