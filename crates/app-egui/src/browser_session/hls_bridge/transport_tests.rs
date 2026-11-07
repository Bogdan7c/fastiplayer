//! Переходник «розетка» → очередь на НАСТОЯЩЕЙ очереди ресурсов `pushed-media-source`.
//!
//! «Страница» — поток-заглушка теста: забирает запросы приложения и отвечает по
//! сценарию номерка (тело кусками, отказ, молчание). Так проверяется вся граница:
//! что ушло странице (только номерки, диапазон, потолок), что вернулось движку
//! (байты, `SourceError` с тем же смыслом «повторять ли»).

use std::collections::HashMap;
use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use browser_bridge_protocol::limits::MAX_RESOURCE_FETCH_BYTES;
use pushed_media_source::{
    RangeRequestIds, RangeRequestNotifier, ResourceExtent, ResourceFetchFailure,
    ResourceFetchOutcome, ResourceFetchPolicy, ResourceFetchRequests, ResourceFetchWriter,
    ResourceRequest, resource_fetch_queue,
};
use source_core::{
    CancellationToken, CurrentThreadAsyncExecutor, HttpBoundedByteRange, HttpBoundedFetchKind,
    HttpBoundedFetchRequest, HttpRequestPolicyFailure, HttpRequestTarget, SourceError,
};
use web_media_adaptive::{AdaptiveBufferedHop, AdaptiveResourceTransport, AdaptiveStreamingHop};

use super::BridgeResourceTransport;

/// Как «страница» отвечает на номерок.
#[derive(Clone)]
enum PageAnswer {
    /// Тело кусками по `chunk` байтов, потом «готово».
    Body { bytes: Vec<u8>, chunk: usize },
    /// Отказ.
    Failure(ResourceFetchFailure),
    /// Молчит.
    Silent,
}

/// Будильник, который никого не будит: «страница» сама опрашивает очередь.
struct Silent;
impl RangeRequestNotifier for Silent {
    fn range_requested(&self) {}
}

/// Очередь + транспорт + поток «страницы».
struct Bridge {
    transport: BridgeResourceTransport,
    requests: ResourceFetchRequests,
    /// Что «страница» получила (в порядке отправки).
    seen: Arc<Mutex<Vec<ResourceRequest>>>,
    stop: Arc<AtomicBool>,
    page: Option<JoinHandle<()>>,
}

impl Bridge {
    fn new(answers: impl IntoIterator<Item = (u32, PageAnswer)>) -> Self {
        Self::with_idle_timeout(answers, Duration::from_secs(10))
    }

    fn with_idle_timeout(
        answers: impl IntoIterator<Item = (u32, PageAnswer)>,
        idle_timeout: Duration,
    ) -> Self {
        let policy = ResourceFetchPolicy::new(
            NonZeroUsize::new(8).expect("non-zero"),
            idle_timeout,
            NonZeroU64::new(MAX_RESOURCE_FETCH_BYTES).expect("non-zero"),
        )
        .expect("policy");
        let (writer, client, requests) =
            resource_fetch_queue(policy, RangeRequestIds::new(), Arc::new(Silent));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let page = spawn_page(
            writer,
            requests.clone(),
            answers.into_iter().collect(),
            Arc::clone(&seen),
            Arc::clone(&stop),
        );
        Self {
            transport: BridgeResourceTransport::new(client),
            requests,
            seen,
            stop,
            page: Some(page),
        }
    }

    fn seen(&self) -> Vec<ResourceRequest> {
        self.seen.lock().expect("seen").clone()
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(page) = self.page.take() {
            page.join().expect("page thread");
        }
    }
}

/// Поток «страницы»: забирает запросы и отвечает по сценарию номерка.
fn spawn_page(
    writer: ResourceFetchWriter,
    requests: ResourceFetchRequests,
    answers: HashMap<u32, PageAnswer>,
    seen: Arc<Mutex<Vec<ResourceRequest>>>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            for request in requests.take_requests() {
                seen.lock().expect("seen").push(request);
                let answer =
                    answers
                        .get(&request.handle.get())
                        .cloned()
                        .unwrap_or(PageAnswer::Failure(ResourceFetchFailure::Http {
                            status: 404,
                        }));
                answer_request(&writer, request, answer);
            }
            thread::sleep(Duration::from_millis(1));
        }
    })
}

fn answer_request(writer: &ResourceFetchWriter, request: ResourceRequest, answer: PageAnswer) {
    match answer {
        PageAnswer::Body { bytes, chunk } => {
            let body = match request.extent {
                ResourceExtent::Whole => bytes,
                ResourceExtent::Range(range) => {
                    let start = (range.offset() as usize).min(bytes.len());
                    let end = (start + range.length().get() as usize).min(bytes.len());
                    bytes[start..end].to_vec()
                }
            };
            for (index, piece) in body.chunks(chunk.max(1)).enumerate() {
                let offset = (index * chunk.max(1)) as u64;
                writer
                    .accept_bytes(request.id, offset, piece)
                    .expect("bytes");
            }
            writer
                .finish(
                    request.id,
                    ResourceFetchOutcome::Completed {
                        size_bytes: body.len() as u64,
                    },
                )
                .expect("finish");
        }
        PageAnswer::Failure(failure) => {
            writer
                .finish(request.id, ResourceFetchOutcome::Failed(failure))
                .expect("finish");
        }
        PageAnswer::Silent => {}
    }
}

/// Запрос «весь ресурс по этому адресу, не больше `limit`».
fn whole(address: &str, limit: usize) -> HttpBoundedFetchRequest {
    HttpBoundedFetchRequest::full(
        HttpRequestTarget::parse_exact(address).expect("target"),
        Vec::new(),
        NonZeroUsize::new(limit).expect("non-zero"),
        HttpBoundedFetchKind::Media,
    )
}

/// Запрос диапазона.
fn ranged(address: &str, start: u64, length: usize) -> HttpBoundedFetchRequest {
    HttpBoundedFetchRequest::range(
        HttpRequestTarget::parse_exact(address).expect("target"),
        Vec::new(),
        HttpBoundedByteRange::new(start, NonZeroUsize::new(length).expect("non-zero"))
            .expect("range"),
        HttpBoundedFetchKind::Media,
    )
}

const ADDRESS_7: &str = "https://resource.fastiplayer-bridge.invalid/7";

fn playlist_body() -> Vec<u8> {
    b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-ENDLIST\n".to_vec()
}

fn buffered_bytes(hop: AdaptiveBufferedHop) -> Vec<u8> {
    match hop {
        AdaptiveBufferedHop::Complete(response) => response.into_bytes(),
        AdaptiveBufferedHop::Redirect { .. } => panic!("the bridge never redirects"),
    }
}

/// Блокирующий буферный путь: номерок из адреса, весь ресурс, потолок движка; байты
/// приходят целиком, хоть и кусками.
#[test]
fn a_whole_resource_is_asked_by_its_handle_and_arrives_byte_for_byte() {
    let bridge = Bridge::new([(
        7,
        PageAnswer::Body {
            bytes: playlist_body(),
            chunk: 5,
        },
    )]);
    let hop = bridge
        .transport
        .fetch_buffered_hop_blocking(whole(ADDRESS_7, 4096), &CancellationToken::new())
        .expect("fetched");
    assert_eq!(buffered_bytes(hop), playlist_body());
    let seen = bridge.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].handle.get(), 7);
    assert_eq!(seen[0].extent, ResourceExtent::Whole);
    assert_eq!(seen[0].max_bytes.get(), 4096);
}

/// Диапазон (`EXT-X-BYTERANGE`) уходит куском `offset/length`, приходит ровно он.
#[test]
fn a_byte_range_is_asked_as_a_slice_and_arrives_exactly() {
    let bytes: Vec<u8> = (0..=255).collect();
    let bridge = Bridge::new([(7, PageAnswer::Body { bytes, chunk: 7 })]);
    let hop = bridge
        .transport
        .fetch_buffered_hop_blocking(ranged(ADDRESS_7, 100, 50), &CancellationToken::new())
        .expect("fetched");
    assert_eq!(buffered_bytes(hop), (100..150).collect::<Vec<u8>>());
    let ResourceExtent::Range(range) = bridge.seen()[0].extent else {
        panic!("a byte range must go out as a slice");
    };
    assert_eq!((range.offset(), range.length().get()), (100, 50));
    assert_eq!(bridge.seen()[0].max_bytes.get(), 50);
}

/// Потолок тела — не больше потолка `fetch_resource` протокола, даже если движок готов
/// принять больше (страница ответит `too_large`, а не пришлёт 1 ГиБ).
#[test]
fn the_body_limit_is_capped_by_the_protocol_ceiling() {
    let bridge = Bridge::new([(
        7,
        PageAnswer::Body {
            bytes: playlist_body(),
            chunk: 64,
        },
    )]);
    let engine_limit = usize::try_from(MAX_RESOURCE_FETCH_BYTES).expect("fits") * 4;
    bridge
        .transport
        .fetch_buffered_hop_blocking(whole(ADDRESS_7, engine_limit), &CancellationToken::new())
        .expect("fetched");
    assert_eq!(bridge.seen()[0].max_bytes.get(), MAX_RESOURCE_FETCH_BYTES);
}

/// Инвариант 2: всё, что не адрес-заглушка номерка, отвергается БЕЗ запроса странице.
#[test]
fn foreign_addresses_are_refused_without_any_request_to_the_page() {
    let bridge = Bridge::new([]);
    for address in [
        "https://example.invalid/x.ts",
        "http://resource.fastiplayer-bridge.invalid/7",
        "https://resource.fastiplayer-bridge.invalid/7?token=1",
        "https://resource.fastiplayer-bridge.invalid/segment.ts",
        "https://resource.fastiplayer-bridge.invalid/007",
        "https://resource.fastiplayer-bridge.invalid:8443/7",
    ] {
        let error = bridge
            .transport
            .fetch_buffered_hop_blocking(whole(address, 1024), &CancellationToken::new())
            .expect_err("a foreign address must be refused");
        assert!(
            matches!(
                error,
                SourceError::HttpRequestPolicyRejected {
                    reason: HttpRequestPolicyFailure::TargetResolution
                }
            ),
            "{address}: {error:?}"
        );
        assert!(!error.is_transient_network_failure(), "{address}");
    }
    // Ни одного номера не выдано, странице ничего не ушло.
    assert_eq!(bridge.requests.stats().issued, 0);
    assert!(bridge.seen().is_empty());
}

/// Отказы страницы → `SourceError` с тем же смыслом «повторять ли», что у HTTP.
#[test]
fn page_failures_keep_their_retry_meaning() {
    let cases = [
        (ResourceFetchFailure::Network, true, "network"),
        (ResourceFetchFailure::Http { status: 503 }, true, "503"),
        (ResourceFetchFailure::Http { status: 403 }, false, "403"),
        (ResourceFetchFailure::Refused, false, "refused"),
        (ResourceFetchFailure::TooLarge, false, "too_large"),
    ];
    for (failure, retryable, label) in cases {
        let bridge = Bridge::new([(7, PageAnswer::Failure(failure))]);
        let error = bridge
            .transport
            .fetch_buffered_hop_blocking(whole(ADDRESS_7, 1024), &CancellationToken::new())
            .expect_err(label);
        assert_eq!(
            error.is_transient_network_failure(),
            retryable,
            "{label}: {error:?}"
        );
        match failure {
            ResourceFetchFailure::Http { status } => assert!(
                matches!(error, SourceError::HttpStatus { status: got, .. } if got.as_u16() == status),
                "{label}: {error:?}"
            ),
            ResourceFetchFailure::Refused => assert!(
                matches!(
                    error,
                    SourceError::HttpRequestPolicyRejected {
                        reason: HttpRequestPolicyFailure::ResourcePolicyRejected
                    }
                ),
                "{label}: {error:?}"
            ),
            ResourceFetchFailure::TooLarge => assert!(
                matches!(error, SourceError::HttpBodyTooLarge { .. }),
                "{label}: {error:?}"
            ),
            ResourceFetchFailure::Network => assert!(
                matches!(error, SourceError::HttpBodyRead { .. }),
                "{label}: {error:?}"
            ),
        }
    }
}

/// Короткий диапазон — `UnexpectedEof` (повторяемо), а не тихо урезанное тело.
#[test]
fn a_short_range_is_an_unexpected_end_and_is_retryable() {
    let bridge = Bridge::new([(
        7,
        PageAnswer::Body {
            bytes: vec![1; 120],
            chunk: 16,
        },
    )]);
    let error = bridge
        .transport
        .fetch_buffered_hop_blocking(ranged(ADDRESS_7, 100, 50), &CancellationToken::new())
        .expect_err("20 bytes instead of 50");
    assert!(
        matches!(
            error,
            SourceError::UnexpectedEof {
                offset: 100,
                expected_bytes: 50,
                actual_bytes: 20
            }
        ),
        "{error:?}"
    );
    assert!(error.is_transient_network_failure());
}

/// Молчащая страница — таймаут (повторяемо), а не вечное ожидание.
#[test]
fn a_silent_page_is_a_retryable_timeout() {
    let bridge = Bridge::with_idle_timeout([(7, PageAnswer::Silent)], Duration::from_millis(60));
    let error = bridge
        .transport
        .fetch_buffered_hop_blocking(whole(ADDRESS_7, 1024), &CancellationToken::new())
        .expect_err("silence");
    assert!(
        matches!(error, SourceError::HttpTimeout { .. }),
        "{error:?}"
    );
    assert!(error.is_transient_network_failure());
}

/// Отмена до побочного эффекта: номер не выдан, странице ничего не ушло.
#[test]
fn a_cancelled_request_issues_nothing() {
    let bridge = Bridge::new([]);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = bridge
        .transport
        .fetch_buffered_hop_blocking(whole(ADDRESS_7, 1024), &cancellation)
        .expect_err("cancelled");
    assert!(matches!(error, SourceError::Cancelled), "{error:?}");
    assert_eq!(bridge.requests.stats().issued, 0);
}

/// Прерываемый буферный путь (манифесты) — те же байты, что у блокирующего.
#[test]
fn the_abortable_buffered_path_delivers_the_same_bytes() {
    let bridge = Bridge::new([(
        7,
        PageAnswer::Body {
            bytes: playlist_body(),
            chunk: 3,
        },
    )]);
    let executor = CurrentThreadAsyncExecutor::new().expect("executor");
    let cancellation = CancellationToken::new();
    let hop = executor
        .block_on(
            bridge
                .transport
                .fetch_buffered_hop_abortable(whole(ADDRESS_7, 4096), &cancellation),
        )
        .expect("fetched");
    assert_eq!(buffered_bytes(hop), playlist_body());
}

/// Потоковый путь: открытие ждёт начала ответа, тело читается кусками до честного
/// конца, учёт байтов совпадает; отказ страницы виден уже при открытии.
#[test]
fn the_streaming_path_reads_the_body_in_chunks_and_fails_at_open() {
    let segment: Vec<u8> = (0..200).map(|value| (value % 251) as u8).collect();
    let bridge = Bridge::new([
        (
            7,
            PageAnswer::Body {
                bytes: segment.clone(),
                chunk: 64,
            },
        ),
        (
            8,
            PageAnswer::Failure(ResourceFetchFailure::Http { status: 403 }),
        ),
    ]);
    let executor = CurrentThreadAsyncExecutor::new().expect("executor");
    let cancellation = CancellationToken::new();
    let hop = executor
        .block_on(
            bridge
                .transport
                .open_streaming_hop(whole(ADDRESS_7, 4096), &cancellation),
        )
        .expect("opened");
    let AdaptiveStreamingHop::Body(mut body) = hop else {
        panic!("the bridge never redirects");
    };
    let mut received = Vec::new();
    while let Some(chunk) = executor
        .block_on(body.next_chunk(&cancellation))
        .expect("chunk")
    {
        received.extend_from_slice(&chunk);
    }
    assert_eq!(received, segment);
    assert_eq!(body.received_body_bytes(), segment.len());
    // После конца — снова конец.
    assert_eq!(
        executor
            .block_on(body.next_chunk(&cancellation))
            .expect("end"),
        None
    );

    let refused = executor.block_on(bridge.transport.open_streaming_hop(
        whole("https://resource.fastiplayer-bridge.invalid/8", 4096),
        &cancellation,
    ));
    assert!(
        matches!(refused, Err(SourceError::HttpStatus { status, .. }) if status.as_u16() == 403),
        "{refused:?}"
    );
}

/// Брошенное потоковое тело — брошенный запрос: очередь это видит (учёт `abandoned`).
#[test]
fn dropping_a_streaming_body_abandons_its_request() {
    // Страница пришлёт только первый кусок и замолчит: запрос остаётся «в пути».
    let policy = ResourceFetchPolicy::new(
        NonZeroUsize::new(8).expect("non-zero"),
        Duration::from_secs(10),
        NonZeroU64::new(MAX_RESOURCE_FETCH_BYTES).expect("non-zero"),
    )
    .expect("policy");
    let (writer, client, requests) =
        resource_fetch_queue(policy, RangeRequestIds::new(), Arc::new(Silent));
    let transport = BridgeResourceTransport::new(client);
    let page_requests = requests.clone();
    let page = thread::spawn(move || {
        loop {
            if let Some(request) = page_requests.take_requests().first() {
                writer.accept_bytes(request.id, 0, b"first").expect("bytes");
                return writer;
            }
            thread::sleep(Duration::from_millis(1));
        }
    });
    let executor = CurrentThreadAsyncExecutor::new().expect("executor");
    let cancellation = CancellationToken::new();
    let hop = executor
        .block_on(transport.open_streaming_hop(whole(ADDRESS_7, 4096), &cancellation))
        .expect("opened");
    let _writer = page.join().expect("page");
    assert_eq!(requests.stats().abandoned, 0);
    drop(hop);
    assert_eq!(requests.stats().abandoned, 1);
}
