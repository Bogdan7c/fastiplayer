//! Hermetic S33 evidence для live refresh, endpoint expiry и detached shutdown.

#[allow(dead_code)]
mod support;

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aes::Aes128;
use cbc::Encryptor;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockModeEncrypt, KeyIvInit};
use demux_api::{
    ProgressiveAsyncSeekLimits, ProgressiveAsyncSeekOutcome, ProgressiveSeekFence,
    ProgressiveSeekRequestId,
};
use media_core::{
    DemuxReadEvent, Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelinePortGeneration,
};
use source_core::{CancellationToken, HttpRequestTarget};
use support::{
    TestQueries, TestServer, adaptive_context, demux_registry, muxed_fmp4, muxed_ts, open_policy,
    response,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsInitialReadinessCapability, HlsLiveOpenRequest,
    HlsMainTrackLayoutIntent, HlsManifestInput, HlsRequestOverrides, HlsRequiredContainer,
    HlsVariantSelectionIntent, HlsVodOpenRequest, prepare_hls_live, prepare_hls_live_receipted,
};
use web_media_transport_api::SourceGeneration;

const TEST_TIMEOUT: Duration = Duration::from_secs(4);

fn muxed_selection() -> HlsVariantSelectionIntent {
    HlsVariantSelectionIntent {
        resolution: None,
        codecs: None,
        audio: HlsAudioLayoutIntent::Muxed,
        main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
    }
}

fn live_request(
    target: HttpRequestTarget,
    cancellation: CancellationToken,
    endpoint_refresh: Arc<dyn HlsEndpointRefreshPort>,
) -> HlsLiveOpenRequest {
    live_request_for_container(
        target,
        cancellation,
        endpoint_refresh,
        HlsRequiredContainer::TransportStream,
    )
}

fn live_request_for_container(
    target: HttpRequestTarget,
    cancellation: CancellationToken,
    endpoint_refresh: Arc<dyn HlsEndpointRefreshPort>,
    container: HlsRequiredContainer,
) -> HlsLiveOpenRequest {
    let generation = SourceGeneration::new(1);
    HlsLiveOpenRequest {
        common: HlsVodOpenRequest {
            http: adaptive_context(&target, cancellation, generation, TestQueries::default()),
            generation,
            manifest: HlsManifestInput::Fetch {
                selected_url: target,
            },
            selection: muxed_selection(),
            overrides: HlsRequestOverrides::new(None),
            containers: HlsComponentContainerIntent {
                main: HlsContainerEvidence::Exact(container),
                alternate_audio: None,
            },
            demux_registry: demux_registry(),
            policy: open_policy(),
        },
        endpoint_refresh,
        timeline_port_generation: DynamicMediaTimelinePortGeneration::new(
            NonZeroU64::new(1).expect("non-zero test port generation"),
        ),
        initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
    }
}

struct SuccessRefreshPort {
    fresh_target: HttpRequestTarget,
    cancellation: CancellationToken,
    endpoint_refreshed: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}

impl HlsEndpointRefreshPort for SuccessRefreshPort {
    fn refresh(
        &self,
        request: HlsEndpointRefreshRequest,
    ) -> Result<HlsEndpointRefreshReply, HlsEndpointRefreshError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let generation = SourceGeneration::new(
            request
                .previous_generation
                .value()
                .checked_add(1)
                .expect("test generation space"),
        );
        self.endpoint_refreshed.store(true, Ordering::SeqCst);
        Ok(HlsEndpointRefreshReply {
            http: adaptive_context(
                &self.fresh_target,
                self.cancellation.clone(),
                generation,
                TestQueries::default(),
            ),
            generation,
            manifest: HlsManifestInput::Fetch {
                selected_url: self.fresh_target.clone(),
            },
            overrides: HlsRequestOverrides::new(None),
        })
    }
}

struct FailingRefreshPort {
    calls: Arc<AtomicUsize>,
    failure: HlsEndpointRefreshError,
}

impl HlsEndpointRefreshPort for FailingRefreshPort {
    fn refresh(
        &self,
        _request: HlsEndpointRefreshRequest,
    ) -> Result<HlsEndpointRefreshReply, HlsEndpointRefreshError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(self.failure)
    }
}

struct BlockingCancelledRefreshPort {
    cancellation: CancellationToken,
    entered: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
}

impl HlsEndpointRefreshPort for BlockingCancelledRefreshPort {
    fn refresh(
        &self,
        _request: HlsEndpointRefreshRequest,
    ) -> Result<HlsEndpointRefreshReply, HlsEndpointRefreshError> {
        self.entered.store(true, Ordering::SeqCst);
        while !self.cancellation.is_cancelled() {
            std::thread::sleep(Duration::from_millis(1));
        }
        self.finished.store(true, Ordering::SeqCst);
        Err(HlsEndpointRefreshError::Cancelled)
    }
}

fn drive_until_recovered(
    demuxer: &mut dyn Demuxer,
    endpoint_refreshed: &AtomicBool,
) -> Vec<DemuxReadEvent> {
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut events = Vec::new();
    let mut saw_wait = false;
    while Instant::now() < deadline {
        match demuxer.next_event().expect("live runtime stays readable") {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                saw_wait = true;
                std::thread::sleep(Duration::from_millis(2));
            }
            event @ DemuxReadEvent::Packet(_) if endpoint_refreshed.load(Ordering::SeqCst) => {
                events.push(event);
                if saw_wait {
                    return events;
                }
            }
            event => events.push(event),
        }
    }
    panic!("live endpoint recovery exceeded test timeout");
}

fn drive_until_error(demuxer: &mut dyn Demuxer) -> anyhow::Error {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        match demuxer.next_event() {
            Ok(DemuxReadEvent::TemporarilyUnavailable(_)) => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(_) => {}
            Err(error) => return error,
        }
    }
    panic!("live failure did not become observable");
}

#[test]
fn segment_expiry_waits_for_atomic_endpoint_replacement_then_recovers() {
    let refreshed = Arc::new(AtomicBool::new(false));
    let handler_refreshed = Arc::clone(&refreshed);
    let first = muxed_ts(90_000);
    let second = muxed_ts(180_000);
    let third = muxed_ts(270_000);
    let server = TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /initial.m3u8 ") {
            return response("200 OK", &[], initial_playlist().as_bytes());
        }
        if request.request_line.starts_with("GET /fresh.m3u8 ") {
            return response("200 OK", &[], fresh_playlist().as_bytes());
        }
        if request.request_line.starts_with("GET /a.ts ") {
            return response("200 OK", &[], &first);
        }
        if request.request_line.starts_with("GET /b.ts ") {
            return if handler_refreshed.load(Ordering::SeqCst) {
                response("200 OK", &[], &second)
            } else {
                response("410 Gone", &[], b"")
            };
        }
        if request.request_line.starts_with("GET /c.ts ") {
            return response("200 OK", &[], &third);
        }
        response("404 Not Found", &[], b"")
    });
    let cancellation = CancellationToken::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let port = Arc::new(SuccessRefreshPort {
        fresh_target: server.target("/fresh.m3u8"),
        cancellation: cancellation.clone(),
        endpoint_refreshed: Arc::clone(&refreshed),
        calls: Arc::clone(&calls),
    });
    let opened = prepare_hls_live(live_request(
        server.target("/initial.m3u8"),
        cancellation,
        port,
    ))
    .expect("prepare live TS");
    assert!(matches!(
        opened.initial_readiness(),
        HlsInitialReadinessCapability::AlreadySynchronous
    ));
    let (mut demuxer, _timeline_port, _) = opened.into_parts();

    let events = drive_until_recovered(demuxer.as_mut(), &refreshed);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, DemuxReadEvent::Packet(_)))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn key_expiry_refreshes_same_uri_key_and_none_does_not_reuse_stale_key() {
    let refreshed = Arc::new(AtomicBool::new(false));
    let handler_refreshed = Arc::clone(&refreshed);
    let first = muxed_ts(90_000);
    let second_plain = muxed_ts(180_000);
    let second = encrypt_pkcs7(&second_plain, [0x22; 16], sequence_iv(1));
    let third = muxed_ts(270_000);
    let key_requests = Arc::new(AtomicUsize::new(0));
    let handler_key_requests = Arc::clone(&key_requests);
    let server = TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /initial.m3u8 ") {
            return response("200 OK", &[], initial_key_playlist().as_bytes());
        }
        if request.request_line.starts_with("GET /fresh.m3u8 ") {
            return response("200 OK", &[], fresh_key_playlist().as_bytes());
        }
        if request.request_line.starts_with("GET /a.ts ") {
            return response("200 OK", &[], &first);
        }
        if request.request_line.starts_with("GET /b.ts ") {
            return response("200 OK", &[], &second);
        }
        if request.request_line.starts_with("GET /c.ts ") {
            return response("200 OK", &[], &third);
        }
        if request.request_line.starts_with("GET /key.bin ") {
            handler_key_requests.fetch_add(1, Ordering::SeqCst);
            return if handler_refreshed.load(Ordering::SeqCst) {
                response("200 OK", &[], &[0x22; 16])
            } else {
                response("403 Forbidden", &[], b"")
            };
        }
        response("404 Not Found", &[], b"")
    });
    let cancellation = CancellationToken::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let port = Arc::new(SuccessRefreshPort {
        fresh_target: server.target("/fresh.m3u8"),
        cancellation: cancellation.clone(),
        endpoint_refreshed: Arc::clone(&refreshed),
        calls: Arc::clone(&calls),
    });
    let opened = prepare_hls_live(live_request(
        server.target("/initial.m3u8"),
        cancellation,
        port,
    ))
    .expect("prepare encrypted live TS");
    let (mut demuxer, _timeline_port, _) = opened.into_parts();

    let _ = drive_until_recovered(demuxer.as_mut(), &refreshed);
    for _ in 0..32 {
        match demuxer
            .next_event()
            .expect("METHOD=NONE segment remains readable")
        {
            DemuxReadEvent::Packet(_) => break,
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                std::thread::sleep(Duration::from_millis(2));
            }
            _ => {}
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(key_requests.load(Ordering::SeqCst) >= 2);
}

#[test]
fn manifest_expiry_uses_same_bounded_replacement_and_reaches_new_segment() {
    let refreshed = Arc::new(AtomicBool::new(false));
    let manifest_requests = Arc::new(AtomicUsize::new(0));
    let handler_manifest_requests = Arc::clone(&manifest_requests);
    let first = muxed_ts(90_000);
    let second = muxed_ts(180_000);
    let third = muxed_ts(270_000);
    let server = TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /initial.m3u8 ") {
            let count = handler_manifest_requests.fetch_add(1, Ordering::SeqCst);
            return if count == 0 {
                response("200 OK", &[], initial_playlist().as_bytes())
            } else {
                response("410 Gone", &[], b"")
            };
        }
        if request.request_line.starts_with("GET /fresh.m3u8 ") {
            return response("200 OK", &[], fresh_playlist().as_bytes());
        }
        if request.request_line.starts_with("GET /a.ts ") {
            return response("200 OK", &[], &first);
        }
        if request.request_line.starts_with("GET /b.ts ") {
            return response("200 OK", &[], &second);
        }
        if request.request_line.starts_with("GET /c.ts ") {
            return response("200 OK", &[], &third);
        }
        response("404 Not Found", &[], b"")
    });
    let cancellation = CancellationToken::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let port = Arc::new(SuccessRefreshPort {
        fresh_target: server.target("/fresh.m3u8"),
        cancellation: cancellation.clone(),
        endpoint_refreshed: Arc::clone(&refreshed),
        calls: Arc::clone(&calls),
    });
    let opened = prepare_hls_live(live_request(
        server.target("/initial.m3u8"),
        cancellation,
        port,
    ))
    .expect("prepare live manifest expiry");
    let (mut demuxer, _timeline_port, _) = opened.into_parts();

    let _ = drive_until_recovered(demuxer.as_mut(), &refreshed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn endpoint_rematch_failure_is_typed_and_secret_safe() {
    let first = muxed_ts(90_000);
    let server = TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /initial.m3u8 ") {
            return response("200 OK", &[], initial_playlist().as_bytes());
        }
        if request.request_line.starts_with("GET /a.ts ") {
            return response("200 OK", &[], &first);
        }
        if request.request_line.starts_with("GET /b.ts ") {
            return response("410 Gone", &[], b"");
        }
        response("404 Not Found", &[], b"")
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let port = Arc::new(FailingRefreshPort {
        calls: Arc::clone(&calls),
        failure: HlsEndpointRefreshError::SemanticRematchFailed,
    });
    let opened = prepare_hls_live(live_request(
        server.target("/initial.m3u8"),
        CancellationToken::new(),
        port,
    ))
    .expect("prepare live rematch failure");
    let (mut demuxer, _timeline_port, _) = opened.into_parts();

    let error = drive_until_error(demuxer.as_mut());
    let message = error.to_string();
    assert!(message.contains("semantic candidate match"));
    assert!(!message.contains("http://"));
    assert!(!message.contains("/b.ts"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn drop_does_not_wait_for_blocked_endpoint_refresh_and_cancels_it() {
    let first = muxed_ts(90_000);
    let server = TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /initial.m3u8 ") {
            return response("200 OK", &[], initial_playlist().as_bytes());
        }
        if request.request_line.starts_with("GET /a.ts ") {
            return response("200 OK", &[], &first);
        }
        if request.request_line.starts_with("GET /b.ts ") {
            return response("410 Gone", &[], b"");
        }
        response("404 Not Found", &[], b"")
    });
    let cancellation = CancellationToken::new();
    let entered = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let port = Arc::new(BlockingCancelledRefreshPort {
        cancellation: cancellation.clone(),
        entered: Arc::clone(&entered),
        finished: Arc::clone(&finished),
    });
    let opened = prepare_hls_live(live_request(
        server.target("/initial.m3u8"),
        cancellation,
        port,
    ))
    .expect("prepare live cancellation");
    let (mut demuxer, _timeline_port, _) = opened.into_parts();
    let deadline = Instant::now() + TEST_TIMEOUT;
    while !entered.load(Ordering::SeqCst) && Instant::now() < deadline {
        let _ = demuxer.next_event();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(entered.load(Ordering::SeqCst));

    let drop_started = Instant::now();
    drop(demuxer);
    assert!(drop_started.elapsed() < Duration::from_millis(100));
    while !finished.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(finished.load(Ordering::SeqCst));
}

#[test]
fn manifest_refresh_cancellation_is_typed_secret_safe_and_worker_bounded() {
    // Origin нужен только для успешного initial open до явной отмены reload-а.
    let first = muxed_ts(90_000);
    let server = TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /initial.m3u8 ") {
            return response("200 OK", &[], initial_playlist().as_bytes());
        }
        if request.request_line.starts_with("GET /a.ts ") {
            return response("200 OK", &[], &first);
        }
        response("404 Not Found", &[], b"")
    });
    // Test owner сохраняет token, чтобы отменить transport без drop demuxer-а.
    let cancellation = CancellationToken::new();
    // Endpoint recovery не должен участвовать в обычной cooperative cancellation.
    let endpoint_calls = Arc::new(AtomicUsize::new(0));
    let port = Arc::new(FailingRefreshPort {
        calls: Arc::clone(&endpoint_calls),
        failure: HlsEndpointRefreshError::AttemptsExhausted,
    });
    let opened = prepare_hls_live(live_request(
        server.target("/initial.m3u8"),
        cancellation.clone(),
        port.clone(),
    ))
    .expect("prepare live manifest refresh cancellation");
    let (mut demuxer, _timeline_port, _) = opened.into_parts();

    // Demuxer остаётся жив: control shutdown не маскирует manifest reload cancellation.
    cancellation.cancel();
    // Worker-owned request держит единственный дополнительный strong Arc endpoint-port-а.
    let worker_deadline = Instant::now() + TEST_TIMEOUT;
    while Arc::strong_count(&port) != 1 && Instant::now() < worker_deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    // Единственный test-owned Arc доказывает полный выход detached refresh worker-а.
    assert_eq!(
        Arc::strong_count(&port),
        1,
        "cancelled HLS manifest refresh worker must release its request"
    );

    // После worker barrier fatal читается без scheduler race и без inner I/O.
    let error = demuxer
        .next_event()
        .expect_err("cancelled HLS manifest refresh must become terminal");
    // Exact public text одновременно закрепляет typed смысл и отсутствие locator/secrets.
    assert_eq!(error.to_string(), "live refresh cancelled");
    assert_eq!(
        endpoint_calls.load(Ordering::SeqCst),
        0,
        "cooperative manifest cancellation must not trigger endpoint recovery"
    );
}

#[test]
fn endlist_refresh_drains_retained_segments_then_returns_eos_with_unknown_duration() {
    let manifest_requests = Arc::new(AtomicUsize::new(0));
    let handler_manifest_requests = Arc::clone(&manifest_requests);
    let first = muxed_ts(90_000);
    let second = muxed_ts(180_000);
    let server = TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /initial.m3u8 ") {
            let count = handler_manifest_requests.fetch_add(1, Ordering::SeqCst);
            return if count == 0 {
                response("200 OK", &[], initial_playlist().as_bytes())
            } else {
                response("200 OK", &[], ended_playlist().as_bytes())
            };
        }
        if request.request_line.starts_with("GET /a.ts ") {
            return response("200 OK", &[], &first);
        }
        if request.request_line.starts_with("GET /b.ts ") {
            return response("200 OK", &[], &second);
        }
        response("404 Not Found", &[], b"")
    });
    let endpoint_calls = Arc::new(AtomicUsize::new(0));
    let port = Arc::new(FailingRefreshPort {
        calls: Arc::clone(&endpoint_calls),
        failure: HlsEndpointRefreshError::AttemptsExhausted,
    });
    let opened = prepare_hls_live(live_request(
        server.target("/initial.m3u8"),
        CancellationToken::new(),
        port,
    ))
    .expect("prepare live ENDLIST transition");
    let (mut demuxer, _timeline_port, _) = opened.into_parts();
    assert_eq!(demuxer.duration(), None);

    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut packet_count = 0;
    loop {
        assert!(Instant::now() < deadline, "ENDLIST drain exceeded timeout");
        match demuxer
            .next_event()
            .expect("ENDLIST transition stays readable")
        {
            DemuxReadEvent::Packet(_) => packet_count += 1,
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                std::thread::sleep(Duration::from_millis(2));
            }
            DemuxReadEvent::EndOfStream => break,
            _ => {}
        }
    }
    assert!(packet_count > 0);
    assert_eq!(demuxer.duration(), None);
    assert_eq!(endpoint_calls.load(Ordering::SeqCst), 0);
    assert!(manifest_requests.load(Ordering::SeqCst) >= 2);
}

#[test]
fn sliding_refresh_evicts_old_rap_and_early_seek_is_typed_expired() {
    let manifest_requests = Arc::new(AtomicUsize::new(0));
    let handler_manifest_requests = Arc::clone(&manifest_requests);
    let first = muxed_ts(90_000);
    let second = muxed_ts(180_000);
    let third = muxed_ts(270_000);
    let server = TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /initial.m3u8 ") {
            let count = handler_manifest_requests.fetch_add(1, Ordering::SeqCst);
            return if count == 0 {
                response("200 OK", &[], initial_playlist().as_bytes())
            } else {
                response("200 OK", &[], fresh_playlist().as_bytes())
            };
        }
        if request.request_line.starts_with("GET /a.ts ") {
            return response("200 OK", &[], &first);
        }
        if request.request_line.starts_with("GET /b.ts ") {
            return response("200 OK", &[], &second);
        }
        if request.request_line.starts_with("GET /c.ts ") {
            return response("200 OK", &[], &third);
        }
        response("404 Not Found", &[], b"")
    });
    let endpoint_calls = Arc::new(AtomicUsize::new(0));
    let port = Arc::new(FailingRefreshPort {
        calls: Arc::clone(&endpoint_calls),
        failure: HlsEndpointRefreshError::AttemptsExhausted,
    });
    let opened = prepare_hls_live_receipted(
        live_request(
            server.target("/initial.m3u8"),
            CancellationToken::new(),
            port,
        ),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("prepare sliding live");
    assert!(matches!(
        opened.initial_readiness(),
        HlsInitialReadinessCapability::Progressive(_)
    ));
    let seek_handle = opened.async_seek_handle().expect("live seek handle");
    let (mut demuxer, timeline_port, _) = opened.into_parts();
    let deadline = Instant::now() + TEST_TIMEOUT;
    while timeline_port
        .observe()
        .snapshot
        .state
        .seekable_range()
        .is_none_or(|range| range.start.as_duration() == Duration::ZERO)
        && Instant::now() < deadline
    {
        let _ = demuxer.next_event().expect("sliding live stays readable");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(manifest_requests.load(Ordering::SeqCst) >= 2);

    let fence = ProgressiveSeekFence {
        runtime_generation: seek_handle.runtime_generation(),
        request_id: ProgressiveSeekRequestId::new(1),
    };
    seek_handle
        .enqueue(
            fence,
            media_core::DemuxSeekRequest::accurate(Duration::ZERO),
        )
        .expect("enqueue expired live seek");
    let deadline = Instant::now() + TEST_TIMEOUT;
    let receipt = loop {
        if let Some(receipt) = seek_handle.poll_receipt() {
            break receipt;
        }
        assert!(Instant::now() < deadline, "live seek receipt timed out");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(receipt.outcome, ProgressiveAsyncSeekOutcome::Failed);
    let observed = timeline_port.observe();
    assert!(
        observed
            .snapshot
            .state
            .seekable_range()
            .is_none_or(|range| range.start.as_duration() > Duration::ZERO)
    );
    assert_eq!(endpoint_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn fmp4_map_discontinuity_emits_one_config_change_with_stable_ids_and_no_duration() {
    let (initialization, first, _) = muxed_fmp4();
    let second = first.clone();
    let mut changed_initialization = initialization.clone();
    let avcc = changed_initialization
        .windows(4)
        .position(|window| window == b"avcC")
        .expect("fixture contains avcC");
    changed_initialization[avcc + 5] ^= 0x01;
    let server = TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /live.m3u8 ") {
            return response("200 OK", &[], fmp4_discontinuity_playlist().as_bytes());
        }
        if request.request_line.starts_with("GET /init-a.mp4 ") {
            return response("200 OK", &[], &initialization);
        }
        if request.request_line.starts_with("GET /init-b.mp4 ") {
            return response("200 OK", &[], &changed_initialization);
        }
        if request.request_line.starts_with("GET /a.m4s ") {
            return response("200 OK", &[], &first);
        }
        if request.request_line.starts_with("GET /b.m4s ") {
            return response("200 OK", &[], &second);
        }
        response("404 Not Found", &[], b"")
    });
    let endpoint_calls = Arc::new(AtomicUsize::new(0));
    let port = Arc::new(FailingRefreshPort {
        calls: Arc::clone(&endpoint_calls),
        failure: HlsEndpointRefreshError::AttemptsExhausted,
    });
    let opened = prepare_hls_live(live_request_for_container(
        server.target("/live.m3u8"),
        CancellationToken::new(),
        port,
        HlsRequiredContainer::FragmentedMp4,
    ))
    .expect("prepare fMP4 live discontinuity");
    let (mut demuxer, _timeline_port, _) = opened.into_parts();
    let initial_ids = demuxer
        .tracks()
        .iter()
        .map(|track| track.id)
        .collect::<Vec<_>>();
    let mut changes = Vec::new();
    let mut packets = 0;
    for _ in 0..128 {
        match demuxer.next_event().expect("fMP4 live remains readable") {
            DemuxReadEvent::Packet(_) => packets += 1,
            DemuxReadEvent::TracksChanged(update) => changes.push(update),
            DemuxReadEvent::TemporarilyUnavailable(_) if packets > 0 => break,
            _ => {}
        }
    }
    assert!(packets > 0);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].duration, None);
    assert_eq!(
        changes[0]
            .tracks
            .iter()
            .map(|track| track.id)
            .collect::<Vec<_>>(),
        initial_ids
    );
    assert_eq!(demuxer.duration(), None);
    assert_eq!(endpoint_calls.load(Ordering::SeqCst), 0);
}

fn initial_playlist() -> String {
    "#EXTM3U\n\
     #EXT-X-TARGETDURATION:1\n\
     #EXT-X-MEDIA-SEQUENCE:0\n\
     #EXTINF:1,\n\
     a.ts\n\
     #EXTINF:1,\n\
     b.ts\n"
        .to_owned()
}

fn fresh_playlist() -> String {
    "#EXTM3U\n\
     #EXT-X-TARGETDURATION:1\n\
     #EXT-X-MEDIA-SEQUENCE:1\n\
     #EXTINF:1,\n\
     b.ts\n\
     #EXTINF:1,\n\
     c.ts\n"
        .to_owned()
}

fn ended_playlist() -> String {
    format!("{}#EXT-X-ENDLIST\n", initial_playlist())
}

fn fmp4_discontinuity_playlist() -> String {
    "#EXTM3U\n\
     #EXT-X-TARGETDURATION:60\n\
     #EXT-X-MEDIA-SEQUENCE:0\n\
     #EXT-X-MAP:URI=\"init-a.mp4\"\n\
     #EXTINF:1,\n\
     a.m4s\n\
     #EXT-X-DISCONTINUITY\n\
     #EXT-X-MAP:URI=\"init-b.mp4\"\n\
     #EXTINF:1,\n\
     b.m4s\n"
        .to_owned()
}

fn initial_key_playlist() -> String {
    "#EXTM3U\n\
     #EXT-X-TARGETDURATION:1\n\
     #EXT-X-MEDIA-SEQUENCE:0\n\
     #EXTINF:1,\n\
     a.ts\n\
     #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n\
     #EXTINF:1,\n\
     b.ts\n"
        .to_owned()
}

fn fresh_key_playlist() -> String {
    "#EXTM3U\n\
     #EXT-X-TARGETDURATION:1\n\
     #EXT-X-MEDIA-SEQUENCE:1\n\
     #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n\
     #EXTINF:1,\n\
     b.ts\n\
     #EXT-X-KEY:METHOD=NONE\n\
     #EXTINF:1,\n\
     c.ts\n"
        .to_owned()
}

fn encrypt_pkcs7(plaintext: &[u8], key: [u8; 16], iv: [u8; 16]) -> Vec<u8> {
    let mut buffer = vec![0_u8; plaintext.len() + 16];
    buffer[..plaintext.len()].copy_from_slice(plaintext);
    let encrypted_length = Encryptor::<Aes128>::new((&key).into(), (&iv).into())
        .encrypt_padded::<Pkcs7>(&mut buffer, plaintext.len())
        .expect("encrypt HLS fixture")
        .len();
    buffer.truncate(encrypted_length);
    buffer
}

fn sequence_iv(sequence: u64) -> [u8; 16] {
    let mut iv = [0_u8; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
}
