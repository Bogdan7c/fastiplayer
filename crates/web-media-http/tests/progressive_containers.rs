//! S22 real-container evidence для neutral progressive HTTP vertical slice.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::Engine as _;
use demux_api::{
    CompositeAvDemuxer, CompositeAvTrackSelection, CompositeComponentLeadPolicy, DemuxContainerId,
    DemuxHints, DemuxInput, DemuxRegistry, DemuxSniffBudget, DemuxSourceExtension,
    ProgressiveDemuxBufferLimits, ProgressiveDemuxer,
};
use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekability, Demuxer, Packet, TrackId, TrackKind,
};
use rustiplayer_config::NetworkConfig;
use source_core::{
    CancellationToken, HttpPathScope, HttpRequestTarget, LocalFileSource, SourceRuntimeConfig,
};
use symphonia_demux::{DemuxerOptions, SymphoniaDemuxFactory};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
    SourceIdentity,
};
use web_media_http::WebMediaHttpProvider;
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration, TransportInput,
    TransportOpenRequest, TransportProvider, TransportRegistry,
};

/// Tiny H.264 video-only MP4, generated once by FFmpeg 6.2 for hermetic tests.
const VIDEO_MP4_BASE64: &str = "AAAAIGZ0eXBpc29tAAACAGlzb21pc28yYXZjMW1wNDEAAAMNbW9vdgAAAGxtdmhkAAAAAAAAAAAAAAAAAAAD6AAAA+gAAQAAAQAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAgAAAjd0cmFrAAAAXHRraGQAAAADAAAAAAAAAAAAAAABAAAAAAAAA+gAAAAAAAAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAABAAAAAABAAAAAQAAAAAAAkZWR0cwAAABxlbHN0AAAAAAAAAAEAAAPoAAAAAAABAAAAAAGvbWRpYQAAACBtZGhkAAAAAAAAAAAAAAAAAABAAAAAQABVxAAAAAAALWhkbHIAAAAAAAAAAHZpZGUAAAAAAAAAAAAAAABWaWRlb0hhbmRsZXIAAAABWm1pbmYAAAAUdm1oZAAAAAEAAAAAAAAAAAAAACRkaW5mAAAAHGRyZWYAAAAAAAAAAQAAAAx1cmwgAAAAAQAAARpzdGJsAAAAtnN0c2QAAAAAAAAAAQAAAKZhdmMxAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAAAABAAEABIAAAASAAAAAAAAAABFUxhdmM2Mi4yOC4xMDIgbGlieDI2NAAAAAAAAAAAAAAAGP//AAAALGF2Y0MBQsAK/+EAFWdCwAraewEQAAADABAAAAMAKPEiagEABGjOD8gAAAAQcGFzcAAAAAEAAAABAAAAFGJ0cnQAAAAAAAATKAAAAAAAAAAYc3R0cwAAAAAAAAABAAAAAQAAQAAAAAAcc3RzYwAAAAAAAAABAAAAAQAAAAEAAAABAAAAFHN0c3oAAAAAAAACZQAAAAEAAAAUc3RjbwAAAAAAAAABAAADPQAAAGJ1ZHRhAAAAWm1ldGEAAAAAAAAAIWhkbHIAAAAAAAAAAG1kaXJhcHBsAAAAAAAAAAAAAAAALWlsc3QAAAAlqXRvbwAAAB1kYXRhAAAAAQAAAABMYXZmNjIuMTIuMTAyAAAACGZyZWUAAAJtbWRhdAAAAlMGBf//T9xF6b3m2Ui3lizYINkj7u94MjY0IC0gY29yZSAxNjUgcjMyMjIgYjM1NjA1YSAtIEguMjY0L01QRUctNCBBVkMgY29kZWMgLSBDb3B5bGVmdCAyMDAzLTIwMjUgLSBodHRwOi8vd3d3LnZpZGVvbGFuLm9yZy94MjY0Lmh0bWwgLSBvcHRpb25zOiBjYWJhYz0wIHJlZj0xIGRlYmxvY2s9MDowOjAgYW5hbHlzZT0wOjAgbWU9ZGlhIHN1Ym1lPTAgcHN5PTEgcHN5X3JkPTEuMDA6MC4wMiBtaXhlZF9yZWY9MCBtZV9yYW5nZT0xNiBjaHJvbWFfbWU9MSB0cmVsbGlzPTAgOHg4ZGN0PTAgY3FtPTAgZGVhZHpvbmU9MjEsMTEgZmFzdF9wc2tpcD0xIGNocm9tYV9xcF9vZmZzZXQ9MCB0aHJlYWRzPTEgbG9va2FoZWFkX3RocmVhZHM9MSBzbGljZWRfdGhyZWFkcz0wIG5yPTAgZGVjaW1hdGU9MSBpbnRlcmxhY2VkPTAgYmx1cmF5X2NvbXBhdD0wIGNvbnN0cmFpbmVkX2ludHJhPTAgYmZyYW1lcz0wIHdlaWdodHA9MCBrZXlpbnQ9MjUwIGtleWludF9taW49MSBzY2VuZWN1dD0wIGludHJhX3JlZnJlc2g9MCByYz1jcmYgbWJ0cmVlPTAgY3JmPTIzLjAgcWNvbXA9MC42MCBxcG1pbj0wIHFwbWF4PTY5IHFwc3RlcD00IGlwX3JhdGlvPTEuNDAgYXE9MACAAAAACmWIhDomKAAJAuA=";

const AUDIO_M4A_BASE64: &str = "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";

/// Tiny muxed VP9+Opus WebM, generated once by FFmpeg 6.2 for hermetic tests.
const MUXED_WEBM_BASE64: &str = "GkXfo59ChoEBQveBAULygQRC84EIQoKEd2VibUKHgQRChYECGFOAZwEAAAAAAAT6EU2bdKtNu4tTq4QVSalmU6yBoU27i1OrhBZUrmtTrIHYTbuMU6uEElTDZ1OsggGL7AEAAAAAAABoAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAVSalmsirXsYMPQkBNgI1MYXZmNjIuMTIuMTAyV0GNTGF2ZjYyLjEyLjEwMkSJiEBqAAAAAAAAFlSua0CtrgEAAAAAAAA/14EBc8WI4RVOQO1IXP2cgQAitZyDdW5kiIEAhoVWX1ZQOYOBASPjg4Q7msoA4JCwgRC6gRCagQJVsIRVuYEBrgEAAAAAAABc14ECc8WILtqFORDWzOacgQAitZyDdW5kiIEAhoZBX09QVVNWqoNjLqBWu4QExLQAg4EC4ZGfgQG1iEDncAAAAAAAYmSBEGOik09wdXNIZWFkAQE4AYC7AAAAAAASVMNnQNpzc6BjwIBnyJpFo4dFTkNPREVSRIeNTGF2ZjYyLjEyLjEwMnNz2mPAi2PFiOEVTkDtSFz9Z8ilRaOHRU5DT0RFUkSHmExhdmM2Mi4yOC4xMDIgbGlidnB4LXZwOWfIoUWjiERVUkFUSU9ORIeTMDA6MDA6MDAuMDAwMDAwMDAwAHNz12PAi2PFiC7ahTkQ1szmZ8iiRaOHRU5DT0RFUkSHlUxhdmM2Mi4yOC4xMDIgbGlib3B1c2fIoUWjiERVUkFUSU9ORIeTMDA6MDA6MDAuMjA4MDAwMDAwAB9DtnVCieeBAKPJggAAgHiCAbdsRyTqAkZv+rYDjIE01uheAesbsV7vyXgY8gqzIUHM0aaf4Yqbhp1rgeoEnutAiitHUCgJ0EZTAHRNtp96w/QA+aO/ggAVgHijP/esmIUDXCYKV5+K8o09bIVMwBPqQbZh53f7dxTMWHWvNRLH9wshBXhb4UZ4svXifY+1ePppKdPLo7mCACmAeJujElFFAKzjUZgqOLSDqFbpMHSeGsZ7FbYcChkq6O3Emtd53XZAdutGpV7d9egrqCh0sAWjt4IAPYB4m6MRtBy/Uih1GdqwJAwsUSZDbA//BI9QgCycHYoxq4Ic+sbnUMzJ9wP0HOjhbWiCb+KjuoIAUYB4m6NfdZz8STO/c/rkeazCRm0L4bmKPIbNcyzW2pEplP1EmVyzZhs/Iy83CL2CaSK296Wi1Quju4IAZYB4m6MXS922lJqWfP0tzgHa+EQUM4ZNzj3/4uyMMFraDmaE1J7E31fJjuaajPCVRFjXb/wpmMQSo7mCAHmAeJujX3Wc/EXvXpTrM9lGArnjG7FsF9NjNymkPGjDrN8mFGnLKUpF54nZ7i7pU2BI16Up08ujuYIAjYB4m6MSU6pITpKco1E0ZuEReGCRPfNQ2rcpm9Q8kk11qrWbS+mLWLrYoq4PwNVcQmSdRgPfi6O5ggChgHiboxJWzh/qBs7RgYcGHJW1jIhm50/ld2IQGsxGsQuWR5+uFiSgUuyGYV8q9fVM5RMJ/qHEo7GCALWASJujX3Wc/EkOxuZCaFcBU85u/OVpN4IrAUNW3avMnEKrO/mPDcjFBe2N0L3WoKehm4IAyQBIBdFHOi7/4JRQMk2r3TOp4vfJLxZY+JuBB3WihADN/mA=";

/// Уникальный номер не позволяет параллельным тестам делить временный путь.
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Режим origin-а явно задаёт transport boundary, который доказывает тест.
#[derive(Clone, Copy)]
enum FixtureOriginMode {
    /// Origin игнорирует `Range` и один раз отдаёт forward-only `200` body.
    FullBody,
    /// Origin обслуживает каждый bounded byte range отдельным `206` response.
    ByteRanges,
}

/// Loopback origin моделирует и Range, и non-Range HTTP без внешней сети.
struct FixtureOrigin {
    /// Loopback listener address.
    address: SocketAddr,
    /// Accept-loop stop flag.
    stop: Arc<AtomicBool>,
    /// Join handle исключает test thread leaks.
    join_handle: Option<JoinHandle<()>>,
}

impl FixtureOrigin {
    /// Запускает origin с одним неизменяемым media body.
    fn spawn(body: Vec<u8>, mode: FixtureOriginMode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture origin");
        listener
            .set_nonblocking(true)
            .expect("set fixture listener nonblocking");
        let address = listener.local_addr().expect("fixture origin address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let join_handle = thread::Builder::new()
            .name("s22-container-origin".to_owned())
            .spawn(move || {
                while !worker_stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => respond(&mut stream, &body, mode),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => return,
                    }
                }
            })
            .expect("spawn fixture origin");
        Self {
            address,
            stop,
            join_handle: Some(join_handle),
        }
    }

    /// Собирает exact component URL.
    fn url(&self, extension: &str) -> String {
        format!("http://{}/component.{extension}", self.address)
    }
}

impl Drop for FixtureOrigin {
    /// Останавливает origin и присоединяет thread.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

/// Guard удаляет только созданный текущим тестом local fixture.
struct TemporaryMediaFile {
    /// Exact test-owned path.
    path: PathBuf,
}

impl TemporaryMediaFile {
    /// Создаёт local fixture с suffix-ом, который не используется как demux hint.
    fn new(body: &[u8]) -> Self {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rustiplayer-s28a1-{}-{sequence}.fixture",
            std::process::id()
        ));
        fs::write(&path, body).expect("write local ISO BMFF fixture");
        Self { path }
    }
}

impl Drop for TemporaryMediaFile {
    /// Cleanup не скрывает test assertion, если файл уже был удалён.
    fn drop(&mut self) {
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove local ISO BMFF fixture: {error}"),
        }
    }
}

/// Результат transport open удерживает origin на время чтения byte source.
struct OpenedHttpFixture {
    /// Concrete demuxer над transport-owned source.
    demuxer: Box<dyn Demuxer + Send>,
    /// Cancellation token передаётся progressive worker-у без подмены.
    cancellation: CancellationToken,
    /// Origin живёт не меньше demuxer-а.
    _origin: FixtureOrigin,
}

/// Local open удерживает fixture path для понятного lifecycle-а теста.
struct OpenedLocalFixture {
    /// Concrete demuxer над seekable local source.
    demuxer: Box<dyn Demuxer + Send>,
    /// Fixture удаляется после закрытия source.
    _fixture: TemporaryMediaFile,
}

/// Выбирает full-body или byte-range HTTP response.
fn respond(stream: &mut TcpStream, body: &[u8], mode: FixtureOriginMode) {
    match mode {
        FixtureOriginMode::FullBody => respond_full_body(stream, body),
        FixtureOriginMode::ByteRanges => respond_byte_range(stream, body),
    }
}

/// Читает request headers и возвращает exact fixture body.
fn respond_full_body(stream: &mut TcpStream, body: &[u8]) {
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("set fixture read timeout");
    let mut request = [0_u8; 4096];
    let _ = stream.read(&mut request).expect("read fixture request");
    let headers = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
    stream
        .write_all(headers.as_bytes())
        .expect("write fixture headers");
    stream.write_all(body).expect("write fixture body");
}

/// Обслуживает один exact `Range: bytes=start-end` request.
fn respond_byte_range(stream: &mut TcpStream, body: &[u8]) {
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("set fixture read timeout");
    let mut request_bytes = [0_u8; 4096];
    let request_length = stream
        .read(&mut request_bytes)
        .expect("read fixture range request");
    let request = String::from_utf8_lossy(&request_bytes[..request_length]);
    let (range_start, requested_end) = request
        .lines()
        .find_map(parse_range_header)
        .expect("HTTP provider должен прислать bounded Range header");
    assert!(range_start < body.len(), "range start выходит за fixture");
    let range_end = requested_end.min(body.len() - 1);
    let response_body = &body[range_start..=range_end];
    let headers = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {range_start}-{range_end}/{}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
        response_body.len(),
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .expect("write fixture range headers");
    stream
        .write_all(response_body)
        .expect("write fixture range body");
}

/// Извлекает inclusive byte range из одного HTTP header line.
fn parse_range_header(header_line: &str) -> Option<(usize, usize)> {
    let (name, value) = header_line.split_once(':')?;
    if !name.eq_ignore_ascii_case("range") {
        return None;
    }
    let range = value.trim().strip_prefix("bytes=")?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?))
}

/// Декодирует checked-in hermetic fixture.
fn decode_fixture(encoded: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("checked-in base64 fixture")
}

/// Открывает real component через provider и S21 registry.
fn open_component(
    body: Vec<u8>,
    extension: &str,
    container: &str,
    role: MediaComponentRole,
    source_identity: u64,
    origin_mode: FixtureOriginMode,
) -> OpenedHttpFixture {
    let server = FixtureOrigin::spawn(body, origin_mode);
    let source_config = SourceRuntimeConfig::from_network_config(&NetworkConfig::default())
        .expect("default source config");
    let provider =
        WebMediaHttpProvider::new(source_config, media_prefetch::PrefetchConfig::default())
            .expect("HTTP provider");
    let provider_id = provider.descriptor().provider_id().clone();
    let mut transport_registry = TransportRegistry::new();
    transport_registry
        .register(Box::new(provider))
        .expect("register HTTP provider");
    let cancellation = CancellationToken::new();
    let target = HttpRequestTarget::parse_exact(server.url(extension)).expect("fixture target");
    let source = SourceIdentity::new(source_identity);
    let component = MediaComponentIdentity::new(
        CandidateIdentity::new(
            source,
            ExtractionGeneration::new(1),
            CandidateFormatIdentity::new(extension).expect("fixture format identity"),
        ),
        SemanticIdentity::new(source, extension).expect("fixture semantic identity"),
        role,
    )
    .expect("fixture component identity");
    let scope = SecretRequestScope::from_target(&target, HttpPathScope::from_target_path(&target));
    let request = TransportOpenRequest::new(
        provider_id,
        component,
        target,
        MediaPresentation::Vod,
        SourceGeneration::new(1),
        SecretRequestContext::builder(scope).build(),
        RedirectPolicy::same_origin(RedirectHopLimit::new(2).expect("redirect limit")),
        cancellation.clone(),
    )
    .expect("fixture transport request");
    let opened = transport_registry
        .open(request)
        .expect("open fixture transport");
    let input = match (origin_mode, opened.into_input()) {
        (FixtureOriginMode::FullBody, TransportInput::Streaming(source)) => {
            DemuxInput::streaming_source(source, cancellation.clone())
        }
        (FixtureOriginMode::ByteRanges, TransportInput::Seekable(source)) => {
            DemuxInput::byte_source(source)
        }
        (FixtureOriginMode::FullBody, TransportInput::Seekable(_)) => {
            panic!("full-body fixture не должен стать seekable")
        }
        (FixtureOriginMode::ByteRanges, TransportInput::Streaming(_)) => {
            panic!("Range fixture должен стать seekable")
        }
    };

    let mut demux_registry = DemuxRegistry::new();
    demux_registry
        .register(Box::new(
            SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("Symphonia factory"),
        ))
        .expect("register Symphonia factory");
    let hints = DemuxHints::none()
        .with_extension(DemuxSourceExtension::new(extension).expect("extension hint"))
        .with_container(DemuxContainerId::new(container).expect("container hint"));
    let sniff_budget = DemuxSniffBudget::new(
        NonZeroUsize::new(4096).expect("sniff bytes"),
        NonZeroUsize::MIN,
        Duration::from_secs(1),
    )
    .expect("sniff budget");
    let demuxer = demux_registry
        .open(input, hints, sniff_budget, cancellation.clone())
        .expect("open real container");

    OpenedHttpFixture {
        demuxer,
        cancellation,
        _origin: server,
    }
}

/// Открывает тот же corpus через seekable local byte-source и S21 registry.
fn open_local_component(body: &[u8], hints: DemuxHints) -> OpenedLocalFixture {
    let fixture = TemporaryMediaFile::new(body);
    let source = LocalFileSource::open(&fixture.path).expect("open local ISO BMFF fixture");
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("Symphonia factory"),
        ))
        .expect("register Symphonia factory");
    let sniff_budget = DemuxSniffBudget::new(
        NonZeroUsize::new(4096).expect("sniff bytes"),
        NonZeroUsize::MIN,
        Duration::from_secs(1),
    )
    .expect("sniff budget");
    let demuxer = registry
        .open(
            DemuxInput::byte_source(Box::new(source)),
            hints,
            sniff_budget,
            CancellationToken::never_cancelled(),
        )
        .expect("open local ISO BMFF fixture");
    OpenedLocalFixture {
        demuxer,
        _fixture: fixture,
    }
}

/// Переводит blocking concrete demuxer в player-facing readiness contract.
fn progressive(
    demuxer: Box<dyn Demuxer + Send>,
    cancellation: CancellationToken,
) -> Box<dyn Demuxer + Send> {
    let limits = ProgressiveDemuxBufferLimits::new(
        NonZeroUsize::new(8).expect("event capacity"),
        NonZeroUsize::new(1024 * 1024).expect("encoded byte capacity"),
    );
    let retry_hint =
        DemuxRetryHint::new(DemuxRetryHint::MIN_RETRY_AFTER).expect("minimum retry hint");
    Box::new(
        ProgressiveDemuxer::new(demuxer, cancellation, limits, retry_hint)
            .expect("progressive demux worker"),
    )
}

/// Ждёт event только на test owner-е; production scheduling принадлежит S21W.
fn next_non_readiness_event(demuxer: &mut dyn Demuxer) -> anyhow::Result<DemuxReadEvent> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match demuxer.next_event()? {
            DemuxReadEvent::TemporarilyUnavailable(_) if Instant::now() < deadline => {
                thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
            }
            event => return Ok(event),
        }
    }
}

/// Возвращает первый track exact kind-а.
fn selected_track(demuxer: &dyn Demuxer, kind: TrackKind) -> TrackId {
    demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == kind)
        .map(|track| track.id)
        .expect("fixture contains selected track")
}

/// Читает первый packet, пропуская только штатные lifecycle events.
fn next_packet(demuxer: &mut dyn Demuxer) -> Packet {
    for _ in 0..8 {
        match demuxer.next_event().expect("read ISO BMFF event") {
            DemuxReadEvent::Packet(packet) => return packet,
            DemuxReadEvent::MediaMetadataChanged(_) | DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                panic!("concrete local/Range demuxer не должен публиковать readiness")
            }
            DemuxReadEvent::EndOfStream => panic!("fixture должен содержать packet"),
        }
    }
    panic!("слишком много lifecycle events до первого packet")
}

/// Проверяет общий classic ISO BMFF contract без привязки к codec backend-у.
fn assert_classic_iso_bmff_contract(demuxer: &mut dyn Demuxer, track_kind: TrackKind) {
    assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
    assert!(
        demuxer
            .duration()
            .is_some_and(|duration| !duration.is_zero())
    );
    let track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == track_kind)
        .expect("classic ISO BMFF track");
    assert!(
        track
            .codec_private
            .as_ref()
            .is_some_and(|codec_private| !codec_private.is_empty()),
        "codec configuration должна остаться на track boundary"
    );
    assert!(track.duration.is_some_and(|duration| !duration.is_zero()));
    let selected_track_id = track.id;

    let packet = next_packet(demuxer);
    assert_eq!(packet.track_id, selected_track_id);
    assert_eq!(packet.kind, track_kind);
    assert!(packet.track_pts.is_some());
    assert!(packet.track_dts.is_some());
    assert!(packet.duration.is_some_and(|duration| !duration.is_zero()));
    assert!(packet.track_duration.is_some());
    assert!(!packet.data.is_empty());

    demuxer
        .seek(Duration::ZERO)
        .expect("classic local/Range ISO BMFF seek to start");
    assert_eq!(next_packet(demuxer).track_id, selected_track_id);
}

/// Переводит весь `ftyp` в одну brand family, не меняя `moov`/`mdat` corpus.
fn with_iso_bmff_brand_family(mut body: Vec<u8>, brand: [u8; 4]) -> Vec<u8> {
    assert_eq!(&body[4..8], b"ftyp");
    let ftyp_size = u32::from_be_bytes(body[0..4].try_into().expect("ftyp size bytes")) as usize;
    assert!(ftyp_size >= 16 && ftyp_size <= body.len());
    body[8..12].copy_from_slice(&brand);
    for compatible_brand in body[16..ftyp_size].chunks_exact_mut(4) {
        compatible_brand.copy_from_slice(&brand);
    }
    body
}

#[test]
fn classic_iso_bmff_local_and_range_preserve_timing_seek_and_codec_private() {
    let video_body = decode_fixture(VIDEO_MP4_BASE64);
    let mut local_video = open_local_component(&video_body, DemuxHints::none());
    assert_classic_iso_bmff_contract(local_video.demuxer.as_mut(), TrackKind::Video);

    let mut range_video = open_component(
        video_body,
        "mp4",
        "iso-bmff",
        MediaComponentRole::Video,
        21,
        FixtureOriginMode::ByteRanges,
    );
    assert_classic_iso_bmff_contract(range_video.demuxer.as_mut(), TrackKind::Video);

    let audio_body = decode_fixture(AUDIO_M4A_BASE64);
    let mut local_audio = open_local_component(&audio_body, DemuxHints::none());
    assert_classic_iso_bmff_contract(local_audio.demuxer.as_mut(), TrackKind::Audio);

    let mut range_audio = open_component(
        audio_body,
        "m4a",
        "iso-bmff",
        MediaComponentRole::Audio,
        22,
        FixtureOriginMode::ByteRanges,
    );
    assert_classic_iso_bmff_contract(range_audio.demuxer.as_mut(), TrackKind::Audio);
}

#[test]
fn mov_and_3gp_brands_open_by_signature_without_extension() {
    for brand in [*b"qt  ", *b"3gp6"] {
        let body = with_iso_bmff_brand_family(decode_fixture(VIDEO_MP4_BASE64), brand);
        let opened = open_local_component(&body, DemuxHints::none());
        assert!(
            opened
                .demuxer
                .tracks()
                .iter()
                .any(|track| track.kind == TrackKind::Video)
        );
    }
}

#[test]
fn iso_bmff_signature_overrides_conflicting_wave_hint() {
    let conflicting_hints = DemuxHints::none()
        .with_extension(DemuxSourceExtension::new("wav").expect("conflicting extension"))
        .with_container(DemuxContainerId::new("wave").expect("conflicting container"));
    let opened = open_local_component(&decode_fixture(VIDEO_MP4_BASE64), conflicting_hints);
    assert!(
        opened
            .demuxer
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Video)
    );
}

#[test]
fn progressive_mp4_m4a_and_webm_open_with_real_hints_and_non_range_input() {
    let video = open_component(
        decode_fixture(VIDEO_MP4_BASE64),
        "mp4",
        "iso-bmff",
        MediaComponentRole::Video,
        1,
        FixtureOriginMode::FullBody,
    );
    assert!(
        video
            .demuxer
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Video)
    );
    assert!(matches!(
        video.demuxer.seekability(),
        DemuxSeekability::NotSeekable { .. }
    ));
    drop(progressive(video.demuxer, video.cancellation));

    let audio = open_component(
        decode_fixture(AUDIO_M4A_BASE64),
        "m4a",
        "iso-bmff",
        MediaComponentRole::Audio,
        2,
        FixtureOriginMode::FullBody,
    );
    assert!(
        audio
            .demuxer
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Audio)
    );
    assert!(matches!(
        audio.demuxer.seekability(),
        DemuxSeekability::NotSeekable { .. }
    ));
    drop(progressive(audio.demuxer, audio.cancellation));

    let muxed = open_component(
        decode_fixture(MUXED_WEBM_BASE64),
        "webm",
        "webm",
        MediaComponentRole::Muxed,
        3,
        FixtureOriginMode::FullBody,
    );
    assert!(
        muxed
            .demuxer
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Video)
    );
    assert!(
        muxed
            .demuxer
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Audio)
    );
    drop(progressive(muxed.demuxer, muxed.cancellation));
}

#[test]
fn separate_progressive_mp4_and_m4a_compose_through_neutral_av_demuxer() {
    let video = open_component(
        decode_fixture(VIDEO_MP4_BASE64),
        "mp4",
        "iso-bmff",
        MediaComponentRole::Video,
        11,
        FixtureOriginMode::FullBody,
    );
    let video = progressive(video.demuxer, video.cancellation);
    let video_track = selected_track(video.as_ref(), TrackKind::Video);
    let audio = open_component(
        decode_fixture(AUDIO_M4A_BASE64),
        "m4a",
        "iso-bmff",
        MediaComponentRole::Audio,
        12,
        FixtureOriginMode::FullBody,
    );
    let audio = progressive(audio.demuxer, audio.cancellation);
    let audio_track = selected_track(audio.as_ref(), TrackKind::Audio);
    let lead_policy = CompositeComponentLeadPolicy::single_pending_packet(
        Duration::from_millis(500),
        NonZeroUsize::new(1024 * 1024).expect("composite byte limit"),
    )
    .expect("composite lead policy");
    let mut composite = CompositeAvDemuxer::new(
        video,
        audio,
        CompositeAvTrackSelection::new(video_track, audio_track),
        lead_policy,
    )
    .expect("neutral A/V composite");

    assert!(
        composite
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Video)
    );
    assert!(
        composite
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Audio)
    );
    let event = next_non_readiness_event(&mut composite).expect("composite progress event");
    assert!(matches!(
        event,
        DemuxReadEvent::Packet(_) | DemuxReadEvent::EndOfStream
    ));
}
