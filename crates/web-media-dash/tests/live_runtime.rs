//! Hermetic S42 evidence: public dynamic DASH boundary доходит до production demux packet-а.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::num::{NonZeroU8, NonZeroU64, NonZeroUsize};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{DashContainer, DashMediaKind, DashMpdLimits, DashUtcTimestamp};
use demux_api::{
    CompositeComponentLeadPolicy, DemuxRegistry, DemuxSniffBudget, ProgressiveAsyncSeekLimits,
    ProgressiveDemuxBufferLimits,
};
use media_core::{
    DemuxReadEvent, DemuxRetryHint, Demuxer, DynamicMediaTimelineEpoch,
    DynamicMediaTimelinePortGeneration, TrackKind,
};
use rustiplayer_config::NetworkConfig;
use source_core::{
    CancellationToken, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig, ValidatedHttpHeaders,
};
use symphonia_demux::{DemuxerOptions, SymphoniaDemuxFactory};
use web_media_adaptive::{AdaptiveHttpContext, AdaptiveRetryPolicy, AdaptiveTransportLimits};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
    SourceIdentity,
};
use web_media_dash::{
    DashEndpointRefreshError, DashEndpointRefreshPort, DashEndpointRefreshReply,
    DashEndpointRefreshRequest, DashLiveOpenRequest, DashLiveOpenResult, DashManifestInput,
    DashPresentationSelection, DashRepresentationEvidence, DashVodOpenPolicy, DashWallClock,
    prepare_dash_live,
};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration,
    TransportOpenRequest, TransportProviderId,
};

/// Общий deadline ограничивает только ожидание worker-а в test thread-е.
const TEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Выполняет public open в worker-е, чтобы self-deadlock давал bounded test failure.
fn prepare_dash_live_with_deadline(
    request: DashLiveOpenRequest,
    cancellation: &CancellationToken,
) -> DashLiveOpenResult {
    // Bounded channel переносит ровно один authoritative open result.
    let (result_sender, result_receiver) = mpsc::sync_channel(1);
    // Named worker облегчает диагностику возможного provider deadlock-а.
    let open_worker = thread::Builder::new()
        .name("dash-live-s42-open".to_owned())
        .spawn(move || {
            // Send failure означает только то, что test thread уже завершился.
            let _ = result_sender.send(prepare_dash_live(request));
        })
        .expect("spawn bounded DASH live open worker");
    // Timeout ловит regression, при которой prepare удерживает re-entrant mutex guard.
    let preparation_result = match result_receiver.recv_timeout(TEST_TIMEOUT) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Cancellation освобождает I/O workers; deadlocked open thread будет detached до exit.
            cancellation.cancel();
            drop(open_worker);
            panic!("production DASH live preparation превысила bounded deadline");
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            // Join публикует worker panic вместо маскировки channel disconnect-ом.
            open_worker
                .join()
                .expect("DASH live open worker не должен паниковать");
            panic!("DASH live open worker завершился без authoritative result");
        }
    };
    // Success path не оставляет test-owned worker после получения результата.
    open_worker
        .join()
        .expect("DASH live open worker не должен паниковать");
    // Production error остаётся typed test failure после bounded orchestration.
    preparation_result.expect("production DASH live preparation succeeds")
}

/// Secret-free журнал завершённых loopback responses и refresh rendezvous.
#[derive(Default)]
struct ServedRequestLog {
    /// Path-ы записываются только после успешной отправки fixture response.
    paths: Vec<String>,
    /// Счётчик отличает реально отданный newer MPD от equal initial refresh-а.
    refreshed_manifest_responses: usize,
}

/// Newer MPD, который test включает только после чтения initial packet-а.
struct RefreshManifestResponse {
    /// Exact loopback path не допускает подмену другого fixture route.
    path: &'static str,
    /// Immutable refreshed manifest body принадлежит fixture server-у.
    body: Vec<u8>,
}

/// Минимальный loopback origin с управляемым MPD refresh response.
struct HermeticDashServer {
    /// Случайный loopback address исключает конфликт с другими test process-ами.
    address: SocketAddr,
    /// Cooperative stop flag принадлежит test fixture-е.
    stop: Arc<AtomicBool>,
    /// Mutex+Condvar создают happens-before вместо scheduler-dependent sleep-а.
    served_requests: Arc<(Mutex<ServedRequestLog>, Condvar)>,
    /// Test owner включает newer MPD только после завершения initial open/read.
    refreshed_manifest_enabled: Arc<AtomicBool>,
    /// Join handle не позволяет fixture server пережить тест.
    worker: Option<thread::JoinHandle<()>>,
}

impl HermeticDashServer {
    /// Запускает bounded origin с initial routes и одним gated newer MPD.
    fn start_with_refresh(
        routes: HashMap<&'static str, Vec<u8>>,
        refreshed_manifest: RefreshManifestResponse,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind DASH fixture server");
        listener
            .set_nonblocking(true)
            .expect("set DASH fixture listener nonblocking");
        let address = listener.local_addr().expect("read DASH fixture address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let served_requests = Arc::new((Mutex::new(ServedRequestLog::default()), Condvar::new()));
        let worker_served_requests = Arc::clone(&served_requests);
        let refreshed_manifest_enabled = Arc::new(AtomicBool::new(false));
        let worker_refreshed_manifest_enabled = Arc::clone(&refreshed_manifest_enabled);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _peer)) => {
                        let request = read_http_request(&mut stream);
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or_default()
                            .split('?')
                            .next()
                            .unwrap_or_default()
                            .to_owned();
                        let use_refreshed_manifest = path == refreshed_manifest.path
                            && worker_refreshed_manifest_enabled.load(Ordering::Acquire);
                        let response = if use_refreshed_manifest {
                            http_response("200 OK", &refreshed_manifest.body)
                        } else {
                            routes.get(path.as_str()).map_or_else(
                                || http_response("404 Not Found", b"missing DASH fixture route"),
                                |body| http_response("200 OK", body),
                            )
                        };
                        stream
                            .write_all(&response)
                            .expect("write DASH fixture response");
                        let (served_requests_lock, served_requests_changed) =
                            &*worker_served_requests;
                        let mut served_request_log = served_requests_lock
                            .lock()
                            .expect("lock DASH served-request log");
                        served_request_log.paths.push(path);
                        if use_refreshed_manifest {
                            served_request_log.refreshed_manifest_responses += 1;
                        }
                        drop(served_request_log);
                        served_requests_changed.notify_all();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("DASH fixture accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            served_requests,
            refreshed_manifest_enabled,
            worker: Some(worker),
        }
    }

    /// Возвращает exact target внутри собственного loopback origin-а.
    fn target(&self, path: &str) -> HttpRequestTarget {
        HttpRequestTarget::parse_exact(format!("http://{}{path}", self.address))
            .expect("valid DASH fixture target")
    }

    /// Возвращает secret-free snapshot уже обслуженных path-ов.
    fn requested_paths(&self) -> Vec<String> {
        let (served_requests_lock, _) = &*self.served_requests;
        served_requests_lock
            .lock()
            .expect("lock DASH served-request log")
            .paths
            .clone()
    }

    /// Публикует newer MPD только после доказанного initial packet-а.
    fn enable_refreshed_manifest(&self) {
        self.refreshed_manifest_enabled
            .store(true, Ordering::Release);
    }

    /// Ждёт фактической отправки newer MPD через Condvar rendezvous.
    fn wait_for_refreshed_manifest_response(&self) {
        let deadline = Instant::now() + TEST_TIMEOUT;
        let (served_requests_lock, served_requests_changed) = &*self.served_requests;
        let mut served_request_log = served_requests_lock
            .lock()
            .expect("lock DASH served-request log");
        while served_request_log.refreshed_manifest_responses == 0 {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            assert!(
                !remaining.is_zero(),
                "DASH refresh worker не запросил enabled newer MPD"
            );
            let (next_served_request_log, wait_result) = served_requests_changed
                .wait_timeout(served_request_log, remaining)
                .expect("wait for DASH refreshed-manifest response");
            served_request_log = next_served_request_log;
            assert!(
                !wait_result.timed_out() || served_request_log.refreshed_manifest_responses > 0,
                "DASH refresh worker не завершил newer MPD response"
            );
        }
    }
}

impl Drop for HermeticDashServer {
    /// Завершает accept loop и обязательно присоединяет fixture thread.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join DASH fixture server");
        }
    }
}

/// Читает только HTTP headers; fixture origin не принимает request body.
fn read_http_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(TEST_TIMEOUT))
        .expect("set DASH fixture read timeout");
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1_024];
    loop {
        let read = stream.read(&mut chunk).expect("read DASH fixture request");
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(request).expect("DASH fixture request is UTF-8 HTTP")
}

/// Формирует closing HTTP/1.1 response с exact body length.
fn http_response(status: &str, body: &[u8]) -> Vec<u8> {
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut response = headers.into_bytes();
    response.extend_from_slice(body);
    response
}

/// Детерминированный local clock синхронизируется direct UTCTiming без system time.
struct FixedWallClock {
    /// Exact локальный timestamp одного runtime generation.
    now: DashUtcTimestamp,
}

impl DashWallClock for FixedWallClock {
    /// Возвращает immutable fixture timestamp.
    fn now_utc(&self) -> DashUtcTimestamp {
        self.now
    }
}

/// Refresh boundary не должен вызываться до cancellation этого короткого fixture-а.
struct RejectingEndpointRefresh {
    /// Счётчик доказывает отсутствие скрытой re-extraction.
    calls: AtomicUsize,
}

impl RejectingEndpointRefresh {
    /// Создаёт endpoint boundary без разрешённых refresh попыток.
    const fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    /// Возвращает количество ошибочных обращений к endpoint owner-у.
    fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
}

impl DashEndpointRefreshPort for RejectingEndpointRefresh {
    /// Любой вызов считается ошибкой тестовой lifecycle-модели.
    fn refresh(
        &self,
        _request: DashEndpointRefreshRequest,
    ) -> Result<DashEndpointRefreshReply, DashEndpointRefreshError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        Err(DashEndpointRefreshError::OwnerDisconnected)
    }
}

/// Public live open проходит manifest fetch/parser/planner/HTTP source/Symphonia demux.
#[test]
fn prepares_local_dynamic_mpd_until_audio_packet_and_cooperative_shutdown() {
    let initial_manifest = dynamic_audio_manifest(DynamicAudioManifestFixture {
        publish_time_seconds: 1,
        minimum_update_period: "PT0.25S",
        period_duration: "PT0.4S",
        segment_repeat: 0,
    });
    let refreshed_manifest = dynamic_audio_manifest(DynamicAudioManifestFixture {
        publish_time_seconds: 2,
        minimum_update_period: "PT60S",
        period_duration: "PT0.4S",
        segment_repeat: 1,
    });
    let server = HermeticDashServer::start_with_refresh(
        HashMap::from([
            ("/live.mpd", initial_manifest),
            (
                "/init.webm",
                decode_base64(include_str!("fixtures/audio-webm-init.base64")),
            ),
            (
                "/0.webm",
                decode_base64(include_str!("fixtures/audio-webm-one.base64")),
            ),
            (
                "/200.webm",
                decode_base64(include_str!("fixtures/audio-webm-two.base64")),
            ),
        ]),
        RefreshManifestResponse {
            path: "/live.mpd",
            body: refreshed_manifest,
        },
    );
    let manifest_target = server.target("/live.mpd");
    let generation = SourceGeneration::new(1);
    let cancellation = CancellationToken::new();
    let endpoint_refresh = Arc::new(RejectingEndpointRefresh::new());
    let opened = prepare_dash_live_with_deadline(
        DashLiveOpenRequest {
            http: Box::new(adaptive_context(
                &manifest_target,
                cancellation.clone(),
                generation,
            )),
            generation,
            manifest: manifest_input(manifest_target),
            selection: audio_selection(),
            demux_registry: demux_registry(),
            policy: open_policy(),
            wall_clock: Arc::new(FixedWallClock {
                now: DashUtcTimestamp::from_unix_nanoseconds(2_000_000_000),
            }),
            timeline_port_generation: DynamicMediaTimelinePortGeneration::new(
                NonZeroU64::new(1).expect("DASH timeline port generation"),
            ),
            initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
            endpoint_refresh: endpoint_refresh.clone(),
        },
        &cancellation,
    );

    let (mut demuxer, seek_handle, timeline_port) = opened.into_parts();
    assert!(
        seek_handle.is_some(),
        "DASH live runtime must publish a receipted seek handle"
    );
    assert_eq!(
        timeline_port.port_generation().get().get(),
        1,
        "timeline port keeps caller-owned generation"
    );
    assert!(
        demuxer
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Audio && track.codec_id == "A_OPUS"),
        "production WebM demux must publish the Opus track"
    );

    let media_packet = next_packet(&mut demuxer);
    assert_eq!(media_packet.kind, TrackKind::Audio);
    assert!(
        !server
            .requested_paths()
            .iter()
            .any(|path| path == "/200.webm"),
        "initial one-segment MPD must not expose the refreshed segment"
    );

    server.enable_refreshed_manifest();
    server.wait_for_refreshed_manifest_response();
    let refreshed_packet = next_packet_at_or_after(&mut demuxer, Duration::from_millis(200));
    assert_eq!(refreshed_packet.kind, TrackKind::Audio);
    assert!(
        server
            .requested_paths()
            .iter()
            .any(|path| path == "/200.webm"),
        "accepted newer MPD must replace the exhausted initial live plan"
    );

    let requested_paths = server.requested_paths();
    assert!(requested_paths.iter().any(|path| path == "/live.mpd"));
    assert!(requested_paths.iter().any(|path| path == "/init.webm"));
    assert!(
        requested_paths.iter().any(|path| path == "/0.webm"),
        "initial live seek/read must traverse the first media segment"
    );
    assert_eq!(
        endpoint_refresh.calls(),
        0,
        "fresh local resources must not trigger app endpoint recovery"
    );

    cancellation.cancel();
    drop(demuxer);
    drop(seek_handle);
    drop(timeline_port);
    wait_for_refresh_shutdown(&endpoint_refresh);
    wait_for_request_quiescence(&server);
    assert!(cancellation.is_cancelled());
    assert_eq!(
        endpoint_refresh.calls(),
        0,
        "cancellation must stop refresh before endpoint re-extraction"
    );
}

/// Poll-ит neutral readiness contract с bounded ожиданием только в test thread-е.
fn next_packet(demuxer: &mut dyn Demuxer) -> media_core::Packet {
    next_packet_at_or_after(demuxer, Duration::ZERO)
}

/// Дожидается packet-а из exact presentation suffix после accepted refresh.
fn next_packet_at_or_after(demuxer: &mut dyn Demuxer, minimum_pts: Duration) -> media_core::Packet {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        assert!(
            Instant::now() < deadline,
            "DASH live worker не достиг required presentation suffix"
        );
        let call_started = Instant::now();
        match demuxer.next_event().expect("DASH live demux event") {
            DemuxReadEvent::Packet(packet) if packet.pts >= minimum_pts => return packet,
            DemuxReadEvent::Packet(_) => {}
            DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(
                    call_started.elapsed() < Duration::from_millis(50),
                    "DASH live player-owner poll must stay nonblocking"
                );
                thread::sleep(Duration::from_millis(2));
            }
            DemuxReadEvent::EndOfStream => {
                panic!("DASH live runtime ended before an encoded packet")
            }
        }
    }
}

/// Ждёт фактического выхода detached refresh owner-а до завершения test process.
fn wait_for_refresh_shutdown(endpoint_refresh: &Arc<RejectingEndpointRefresh>) {
    // Локальный Arc остаётся единственным владельцем только после выхода refresh
    // closure и progressive worker, освободившего `DashLiveShared`.
    let deadline = Instant::now() + TEST_TIMEOUT;
    while Arc::strong_count(endpoint_refresh) != 1 && Instant::now() < deadline {
        // Короткая пауза не создаёт busy-spin и остаётся только в test owner-е.
        thread::sleep(Duration::from_millis(2));
    }
    // Незавершённый worker нельзя выдавать за cooperative shutdown: помимо
    // lifecycle leak он повреждает LLVM profile при завершении test process.
    assert_eq!(
        Arc::strong_count(endpoint_refresh),
        1,
        "DASH refresh owner must release every request/shared reference before test exit"
    );
}

/// Проверяет, что после cooperative cancellation origin больше не получает запросов.
fn wait_for_request_quiescence(server: &HermeticDashServer) {
    let stable_count = server.requested_paths().len();
    let deadline = Instant::now() + Duration::from_millis(100);
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
        assert_eq!(
            server.requested_paths().len(),
            stable_count,
            "cancelled DASH workers must not start another HTTP request"
        );
    }
}

/// Собирает live-scoped transport request без headers/cookies/query secrets.
fn adaptive_context(
    target: &HttpRequestTarget,
    cancellation: CancellationToken,
    generation: SourceGeneration,
) -> AdaptiveHttpContext {
    let source = SourceIdentity::new(42);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(generation.value()),
        CandidateFormatIdentity::new("dash-live-runtime-test")
            .expect("DASH candidate format identity"),
    );
    let semantic = SemanticIdentity::new(source, "dash-live-runtime-test")
        .expect("DASH candidate semantic identity");
    let component = MediaComponentIdentity::new(exact, semantic, MediaComponentRole::Audio)
        .expect("DASH audio component identity");
    let scope =
        SecretRequestScope::from_target(target, HttpPathScope::new("/").expect("root path scope"));
    let secrets = SecretRequestContext::builder(scope)
        .with_headers(ValidatedHttpHeaders::new(Vec::new()).expect("empty DASH fixture headers"))
        .build();
    let request = TransportOpenRequest::new(
        TransportProviderId::new("dash-live-runtime-test").expect("DASH provider identity"),
        component,
        target.clone(),
        MediaPresentation::Live,
        generation,
        secrets,
        RedirectPolicy::same_origin(
            RedirectHopLimit::new(2).expect("DASH fixture redirect hop limit"),
        ),
        cancellation,
    )
    .expect("DASH transport request");
    AdaptiveHttpContext::new(
        request,
        &SourceRuntimeConfig::from_network_config(&NetworkConfig::default())
            .expect("DASH fixture source config"),
        AdaptiveTransportLimits::new(non_zero(64 * 1_024), non_zero(256 * 1_024), non_zero(8)),
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(2).expect("DASH retry attempts"),
            Duration::from_millis(2),
            Duration::from_millis(5),
        )
        .expect("DASH retry policy"),
    )
    .expect("adaptive DASH context")
}

/// Exact selected audio/WebM Representation evidence.
fn audio_selection() -> DashPresentationSelection {
    DashPresentationSelection::Single {
        main: DashRepresentationEvidence {
            media_kind: DashMediaKind::Audio,
            container: DashContainer::WebM,
            representation_id: Some("audio".to_owned()),
            codecs: Some("opus".to_owned()),
            bandwidth: None,
            dimensions: None,
        },
    }
}

/// Регистрирует production Symphonia factory без fake demuxer-а.
fn demux_registry() -> Arc<DemuxRegistry> {
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("Symphonia demux factory"),
        ))
        .expect("register Symphonia demux factory");
    Arc::new(registry)
}

/// Устанавливает явные XML/schema bounds на untrusted MPD.
fn manifest_input(target: HttpRequestTarget) -> DashManifestInput {
    DashManifestInput {
        target,
        xml_budgets: XmlBudgets::builder()
            .maximum_document_bytes(64 * 1_024)
            .maximum_depth(16)
            .maximum_tokens(512)
            .maximum_attributes_per_element(16)
            .maximum_attribute_count(128)
            .maximum_attribute_bytes(8 * 1_024)
            .maximum_namespace_declarations_per_element(4)
            .maximum_namespace_declaration_count(16)
            .maximum_namespace_bytes(1_024)
            .maximum_text_bytes(8 * 1_024)
            .build()
            .expect("DASH XML budgets"),
        mpd_limits: DashMpdLimits {
            maximum_periods: 2,
            maximum_adaptation_sets_per_period: 2,
            maximum_representations_per_adaptation_set: 2,
            maximum_segments_per_list: 4,
            maximum_timeline_entries: 4,
            maximum_schema_string_bytes: 1_024,
        },
    }
}

/// Собирает compact explicit runtime policy без default-магии.
fn open_policy() -> DashVodOpenPolicy {
    DashVodOpenPolicy {
        maximum_manifest_bytes: non_zero(64 * 1_024),
        maximum_fragment_bytes: non_zero(256 * 1_024),
        maximum_range_read_bytes: non_zero(16 * 1_024),
        maximum_planned_segments: non_zero(4),
        demux_sniff_budget: DemuxSniffBudget::new(
            non_zero(64 * 1_024),
            non_zero(4),
            Duration::from_secs(1),
        )
        .expect("DASH demux sniff budget"),
        progressive_limits: ProgressiveDemuxBufferLimits::new(non_zero(16), non_zero(512 * 1_024)),
        asynchronous_seek_limits: ProgressiveAsyncSeekLimits::new(non_zero(4)),
        retry_hint: DemuxRetryHint::new(Duration::from_millis(2)).expect("DASH demux retry hint"),
        composite_lead_policy: CompositeComponentLeadPolicy::single_pending_packet(
            Duration::from_secs(1),
            non_zero(256 * 1_024),
        )
        .expect("DASH composite lead policy"),
        maximum_seek_scan_events: non_zero(1_024),
        maximum_seek_scan_bytes: non_zero(2 * 1_024 * 1_024),
    }
}

/// Возвращает non-zero bound с понятным fixture failure.
fn non_zero(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("DASH fixture bound is non-zero")
}

/// Named параметры отделяют initial revision от accepted refreshed revision.
struct DynamicAudioManifestFixture {
    /// Strictly monotonic publish time управляет authoritative refresh commit.
    publish_time_seconds: u8,
    /// Initial revision быстро poll-ится, refreshed revision получает far deadline.
    minimum_update_period: &'static str,
    /// Immutable Period bound заранее вмещает initial и refreshed suffix timeline.
    period_duration: &'static str,
    /// Ноль даёт initial segment, единица добавляет refreshed suffix segment.
    segment_repeat: u8,
}

/// Строит exact dynamic MPD revision с общим stable Representation identity.
fn dynamic_audio_manifest(fixture: DynamicAudioManifestFixture) -> Vec<u8> {
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic"
      availabilityStartTime="1970-01-01T00:00:00Z"
      publishTime="1970-01-01T00:00:{publish_time_seconds:02}Z"
      minimumUpdatePeriod="{minimum_update_period}" suggestedPresentationDelay="PT0.05S">
      <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014"
        value="1970-01-01T00:00:02Z"/>
      <Period id="p0" start="PT0S" duration="{period_duration}">
        <AdaptationSet id="audio-set" contentType="audio"
          mimeType="audio/webm" codecs="opus">
          <Representation id="audio">
            <SegmentTemplate timescale="1000" initialization="init.webm"
              media="$Time$.webm">
              <SegmentTimeline><S t="0" d="200" r="{segment_repeat}"/></SegmentTimeline>
            </SegmentTemplate>
          </Representation>
        </AdaptationSet>
      </Period>
    </MPD>"#,
        publish_time_seconds = fixture.publish_time_seconds,
        minimum_update_period = fixture.minimum_update_period,
        period_duration = fixture.period_duration,
        segment_repeat = fixture.segment_repeat,
    )
    .into_bytes()
}

/// Декодирует checked-in ASCII base64 без новой test dependency.
fn decode_base64(encoded: &str) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(encoded.len() * 3 / 4);
    let mut accumulator = 0_u32;
    let mut accumulated_bits = 0_u8;
    for encoded_byte in encoded.trim().bytes() {
        let value = match encoded_byte {
            b'A'..=b'Z' => encoded_byte - b'A',
            b'a'..=b'z' => encoded_byte - b'a' + 26,
            b'0'..=b'9' => encoded_byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\r' | b'\n' | b' ' | b'\t' => continue,
            _ => panic!("invalid base64 fixture byte"),
        };
        accumulator = (accumulator << 6) | u32::from(value);
        accumulated_bits += 6;
        if accumulated_bits >= 8 {
            accumulated_bits -= 8;
            decoded.push((accumulator >> accumulated_bits) as u8);
            accumulator &= (1_u32 << accumulated_bits).saturating_sub(1);
        }
    }
    decoded
}
