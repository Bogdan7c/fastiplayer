//! Сквозная проверка очереди запросов ресурсов через публичный API.
//!
//! Движок (как HLS) качает плейлист, init-диапазон и сегмент из нескольких потоков;
//! «приложение» забирает запросы, «страница» раздаёт ресурсы кусками и завершает их
//! в обратном порядке; каждый поток движка получает своё тело байт в байт. Рядом
//! живёт файл по запросам байтов того же сеанса (общий счётчик номеров): переводчик
//! приложения выбирает адресата рамки по `claim_of`, и ни одна сторона не трогает
//! чужое.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

use std::collections::HashMap;
use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use pushed_media_source::{
    ExternalResourceHandle, FileOpenProgress, PushedFilePolicy, RangeBytesVerdict, RangeRequestId,
    RangeRequestIds, RangeRequestNotifier, ResourceByteRange, ResourceExtent, ResourceFetchError,
    ResourceFetchOutcome, ResourceFetchPolicy, ResourceFetchSpec, ResourceFetchWriter,
    ResourceFrameVerdict, ResourceRequest, ResourceRequestClaim, pushed_file, resource_fetch_queue,
};
use source_core::CancellationToken;

/// Сколько тест ждёт того, что должно случиться «сразу».
const PROMPT: Duration = Duration::from_secs(10);

/// Размер куска, которым «страница» шлёт тела (мелко, чтобы кусков было много).
const PAGE_CHUNK: usize = 7;

#[derive(Default)]
struct CountingNotifier {
    calls: AtomicUsize,
}

impl RangeRequestNotifier for CountingNotifier {
    fn range_requested(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

fn queue_policy() -> ResourceFetchPolicy {
    ResourceFetchPolicy::new(
        NonZeroUsize::new(8).expect("non-zero"),
        Duration::from_secs(10),
        NonZeroU64::new(16 << 20).expect("non-zero"),
    )
    .expect("policy")
}

/// Ресурсы «страницы» по номеркам.
fn page_resources() -> HashMap<u32, Vec<u8>> {
    let playlist = b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nhttps://resource.fastiplayer-bridge.invalid/2\n#EXT-X-ENDLIST\n".to_vec();
    let media: Vec<u8> = (0..=255u8).cycle().take(1_000).collect();
    HashMap::from([(1, playlist), (2, media)])
}

/// Тело, которое страница отдаёт на запрос: весь ресурс или диапазон.
fn served_body(resources: &HashMap<u32, Vec<u8>>, request: &ResourceRequest) -> Vec<u8> {
    let resource = &resources[&request.handle.get()];
    match request.extent {
        ResourceExtent::Whole => resource.clone(),
        ResourceExtent::Range(range) => {
            let start = usize::try_from(range.offset()).expect("offset");
            let end = start + usize::try_from(range.length().get()).expect("length");
            resource[start..end.min(resource.len())].to_vec()
        }
    }
}

/// Шлёт тело кусками; итог — отдельно.
fn send_body(writer: &ResourceFetchWriter, id: RangeRequestId, body: &[u8]) {
    for (index, chunk) in body.chunks(PAGE_CHUNK).enumerate() {
        let offset = u64::try_from(index * PAGE_CHUNK).expect("offset");
        assert_eq!(
            writer.accept_bytes(id, offset, chunk),
            Ok(ResourceFrameVerdict::Accepted)
        );
    }
}

/// Ждёт, пока появятся `count` запросов.
fn take_at_least(
    requests: &pushed_media_source::ResourceFetchRequests,
    count: usize,
) -> Vec<ResourceRequest> {
    let deadline = Instant::now() + PROMPT;
    let mut taken = Vec::new();
    while taken.len() < count {
        assert!(Instant::now() < deadline, "requests did not arrive in time");
        taken.extend(requests.take_requests());
        thread::sleep(Duration::from_millis(2));
    }
    taken
}

#[test]
fn engine_threads_receive_their_own_bodies_when_the_page_finishes_in_reverse_order() {
    let notifier = Arc::new(CountingNotifier::default());
    let (writer, client, requests) = resource_fetch_queue(
        queue_policy(),
        RangeRequestIds::new(),
        Arc::clone(&notifier) as Arc<dyn RangeRequestNotifier>,
    );
    let resources = page_resources();
    let init_range =
        ResourceByteRange::new(0, NonZeroU64::new(100).expect("non-zero")).expect("range");
    let specs = [
        (ExternalResourceHandle::new(1), ResourceExtent::Whole),
        (
            ExternalResourceHandle::new(2),
            ResourceExtent::Range(init_range),
        ),
        (ExternalResourceHandle::new(2), ResourceExtent::Whole),
    ];
    let engines: Vec<_> = specs
        .iter()
        .map(|(handle, extent)| {
            let client = client.clone();
            let spec = ResourceFetchSpec {
                handle: *handle,
                extent: *extent,
                max_bytes: NonZeroU64::new(4_096).expect("non-zero"),
            };
            thread::spawn(move || {
                let token = CancellationToken::new();
                let mut fetch = client.start(spec, &token)?;
                let mut body = Vec::new();
                while let Some(chunk) = fetch.next_chunk_blocking(&token)? {
                    body.extend_from_slice(&chunk);
                }
                Ok::<_, ResourceFetchError>((spec, body, fetch.received_bytes()))
            })
        })
        .collect();

    let sent = take_at_least(&requests, specs.len());
    let mut expected_by_spec = Vec::new();
    // Обратный порядок итогов: последний выданный завершается первым.
    for request in sent.iter().rev() {
        let body = served_body(&resources, request);
        send_body(&writer, request.id, &body);
        let size_bytes = u64::try_from(body.len()).expect("size");
        writer
            .finish(request.id, ResourceFetchOutcome::Completed { size_bytes })
            .expect("finish");
        expected_by_spec.push(((request.handle, request.extent), body));
    }
    for engine in engines {
        let (spec, body, received) = engine.join().expect("engine").expect("body");
        let expected = expected_by_spec
            .iter()
            .find(|(key, _)| *key == (spec.handle, spec.extent))
            .map(|(_, body)| body.clone())
            .expect("expected body");
        assert_eq!(body, expected, "{spec:?}");
        assert_eq!(received, u64::try_from(expected.len()).expect("len"));
    }
    let stats = requests.stats();
    assert_eq!((stats.issued, stats.sent, stats.completed), (3, 3, 3));
    let total: usize = expected_by_spec.iter().map(|(_, body)| body.len()).sum();
    assert_eq!(stats.bytes_accepted, u64::try_from(total).expect("total"));
    assert_eq!(notifier.calls.load(Ordering::SeqCst), 3);
}

#[test]
fn queue_and_file_share_request_numbers_without_touching_each_other() {
    let ids = RangeRequestIds::new();
    let file_notifier = Arc::new(CountingNotifier::default());
    let (mut file_writer, mut file_opener, file_requests) = pushed_file(
        PushedFilePolicy::recommended().expect("file policy"),
        ids.clone(),
        Arc::clone(&file_notifier) as Arc<dyn RangeRequestNotifier>,
    );
    let queue_notifier = Arc::new(CountingNotifier::default());
    let (queue_writer, client, queue_requests) = resource_fetch_queue(
        queue_policy(),
        ids,
        Arc::clone(&queue_notifier) as Arc<dyn RangeRequestNotifier>,
    );

    // Файл просит первый диапазон (номер 1).
    assert!(matches!(file_opener.poll_open(), FileOpenProgress::Pending));
    let deadline = Instant::now() + PROMPT;
    let file_request = loop {
        if let Some(request) = file_requests.take_requests().into_iter().next() {
            break request;
        }
        assert!(Instant::now() < deadline, "the file did not request bytes");
        thread::sleep(Duration::from_millis(2));
    };
    // Очередь просит ресурс (номер 2).
    let token = CancellationToken::new();
    let spec = ResourceFetchSpec {
        handle: ExternalResourceHandle::new(5),
        extent: ResourceExtent::Whole,
        max_bytes: NonZeroU64::new(64).expect("non-zero"),
    };
    let mut fetch = client.start(spec, &token).expect("start");
    let queue_request = queue_requests.take_requests().pop().expect("queue request");
    assert!(file_request.id < queue_request.id);
    assert!(
        file_requests.take_requests().is_empty(),
        "the file sees only its own requests"
    );

    // Переводчик приложения: рамка — владельцу номера.
    assert_eq!(
        queue_writer.claim_of(queue_request.id),
        ResourceRequestClaim::Ours
    );
    assert_eq!(
        queue_writer.claim_of(file_request.id),
        ResourceRequestClaim::NotOurs
    );

    // Рамка файла, по ошибке отданная очереди, — устаревшая, а не нарушение.
    assert_eq!(
        queue_writer.accept_bytes(file_request.id, 0, b"file"),
        Ok(ResourceFrameVerdict::StaleDropped)
    );
    // Байты очереди идут в очередь и не трогают файл.
    queue_writer
        .accept_bytes(queue_request.id, 0, b"resource")
        .expect("queue bytes");
    queue_writer
        .finish(
            queue_request.id,
            ResourceFetchOutcome::Completed { size_bytes: 8 },
        )
        .expect("queue finish");
    let mut body = Vec::new();
    while let Some(chunk) = fetch.next_chunk_blocking(&token).expect("chunk") {
        body.extend_from_slice(&chunk);
    }
    assert_eq!(body, b"resource");

    // Файл по-прежнему принимает свои рамки и не знает об очереди.
    assert_eq!(
        file_writer.accept_range_bytes(file_request.id, file_request.offset, b"ftyp"),
        Ok(RangeBytesVerdict::Accepted)
    );
    assert_eq!(file_writer.failure(), None);
    assert!(file_requests.take_requests().is_empty());

    let stats = queue_requests.stats();
    assert_eq!(
        stats.bytes_accepted, 8,
        "the file's bytes are not counted by the queue"
    );
    assert_eq!(stats.stale_frames, 1);
    assert_eq!(stats.failures.page_violations, 0);
    assert_eq!(queue_notifier.calls.load(Ordering::SeqCst), 1);
    file_writer.seal();
}
