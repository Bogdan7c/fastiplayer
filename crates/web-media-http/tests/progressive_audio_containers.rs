//! S28C Range/non-Range HTTP proof для current audio-container families.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use demux_api::{
    DemuxContainerId, DemuxHints, DemuxInput, DemuxRegistry, DemuxSniffBudget,
    DemuxSourceExtension, ProgressiveDemuxBufferLimits, ProgressiveDemuxer,
};
use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekRequest, DemuxSeekability, Demuxer, MediaDemuxError,
    Packet, TrackKind,
};
use rustiplayer_config::NetworkConfig;
use source_core::{CancellationToken, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig};
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

#[allow(dead_code)]
#[path = "../../symphonia-demux/src/factory/tests/audio_fixtures.rs"]
mod audio_fixtures;

use audio_fixtures::AudioContainerFixture;

/// Режим origin-а однозначно выбирает seekable Range или forward-only body.
#[derive(Clone, Copy)]
enum OriginMode {
    /// Origin возвращает bounded `206` responses.
    ByteRanges,
    /// Origin возвращает один `200` response и игнорирует initial Range.
    FullBody,
}

/// Loopback HTTP origin хранит exact generated fixture до завершения test-а.
struct FixtureOrigin {
    /// Listener address для exact component URL.
    address: SocketAddr,
    /// Stop flag завершает nonblocking accept loop.
    stop: Arc<AtomicBool>,
    /// Join handle не оставляет фоновые test threads.
    join_handle: Option<JoinHandle<()>>,
}

impl FixtureOrigin {
    /// Запускает hermetic origin без внешней сети.
    fn spawn(body: Vec<u8>, mode: OriginMode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind S28C origin");
        listener
            .set_nonblocking(true)
            .expect("set S28C origin nonblocking");
        let address = listener.local_addr().expect("S28C origin address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let join_handle = thread::Builder::new()
            .name("s28c-audio-origin".to_owned())
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
            .expect("spawn S28C origin");
        Self {
            address,
            stop,
            join_handle: Some(join_handle),
        }
    }

    /// Собирает URL только из loopback address и non-secret fixture extension.
    fn url(&self, extension: &str) -> String {
        format!("http://{}/audio.{extension}", self.address)
    }
}

impl Drop for FixtureOrigin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(join_handle) = self.join_handle.take() {
            join_handle.join().expect("join S28C origin");
        }
    }
}

/// Выбирает exact response mechanics по режиму origin-а.
fn respond(stream: &mut TcpStream, body: &[u8], mode: OriginMode) {
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("set S28C request timeout");
    let mut request_bytes = [0_u8; 4_096];
    let request_length = stream.read(&mut request_bytes).expect("read S28C request");
    if request_length == 0 {
        return;
    }
    let request = String::from_utf8_lossy(&request_bytes[..request_length]);
    match mode {
        OriginMode::FullBody => respond_full_body(stream, body),
        OriginMode::ByteRanges => respond_byte_range(stream, body, &request),
    }
}

/// Возвращает forward-only full body ровно один раз на connection.
fn respond_full_body(stream: &mut TcpStream, body: &[u8]) {
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .expect("write S28C full-body headers");
    stream.write_all(body).expect("write S28C full body");
}

/// Возвращает exact requested byte interval и authoritative total length.
fn respond_byte_range(stream: &mut TcpStream, body: &[u8], request: &str) {
    let Some((range_start, requested_end)) = request.lines().find_map(parse_range_header) else {
        return;
    };
    assert!(range_start < body.len(), "S28C range start вне fixture");
    let range_end = requested_end.min(body.len() - 1);
    let response_body = &body[range_start..=range_end];
    let headers = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {range_start}-{range_end}/{}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
        response_body.len(),
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .expect("write S28C Range headers");
    stream
        .write_all(response_body)
        .expect("write S28C Range body");
}

/// Разбирает только app-generated single HTTP bytes range.
fn parse_range_header(header_line: &str) -> Option<(usize, usize)> {
    let (name, value) = header_line.split_once(':')?;
    if !name.eq_ignore_ascii_case("range") {
        return None;
    }
    let range = value.trim().strip_prefix("bytes=")?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?))
}

/// Открытый HTTP fixture удерживает origin и cancellation рядом с demuxer-ом.
struct OpenedFixture {
    /// Concrete blocking demuxer после transport+registry composition.
    demuxer: Box<dyn Demuxer + Send>,
    /// Token передаётся progressive worker-у.
    cancellation: CancellationToken,
    /// Origin должен жить дольше source/prefetch worker-а.
    _origin: FixtureOrigin,
}

/// Открывает generated fixture через настоящий S22 HTTP provider.
fn open_fixture(
    fixture: &AudioContainerFixture,
    mode: OriginMode,
    source_identity: u64,
) -> OpenedFixture {
    let origin = FixtureOrigin::spawn(fixture.bytes.clone(), mode);
    let source_config = SourceRuntimeConfig::from_network_config(&NetworkConfig::default())
        .expect("default S28C source config");
    let provider =
        WebMediaHttpProvider::new(source_config, media_prefetch::PrefetchConfig::default())
            .expect("S28C HTTP provider");
    let provider_id = provider.descriptor().provider_id().clone();
    let mut transport_registry = TransportRegistry::new();
    transport_registry
        .register(Box::new(provider))
        .expect("register S28C HTTP provider");
    let cancellation = CancellationToken::new();
    let target =
        HttpRequestTarget::parse_exact(origin.url(fixture.extension)).expect("S28C fixture target");
    let source = SourceIdentity::new(source_identity);
    let component = MediaComponentIdentity::new(
        CandidateIdentity::new(
            source,
            ExtractionGeneration::new(1),
            CandidateFormatIdentity::new(fixture.extension).expect("S28C format identity"),
        ),
        SemanticIdentity::new(source, fixture.extension).expect("S28C semantic identity"),
        MediaComponentRole::Audio,
    )
    .expect("S28C component identity");
    let scope = SecretRequestScope::from_target(&target, HttpPathScope::from_target_path(&target));
    let request = TransportOpenRequest::new(
        provider_id,
        component,
        target,
        MediaPresentation::Vod,
        SourceGeneration::new(1),
        SecretRequestContext::builder(scope).build(),
        RedirectPolicy::same_origin(RedirectHopLimit::new(2).expect("S28C redirect limit")),
        cancellation.clone(),
    )
    .expect("S28C transport request");
    let input = match (
        mode,
        transport_registry
            .open(request)
            .expect("open S28C transport")
            .into_input(),
    ) {
        (OriginMode::ByteRanges, TransportInput::Seekable(source)) => {
            DemuxInput::byte_source(source)
        }
        (OriginMode::FullBody, TransportInput::Streaming(source)) => {
            DemuxInput::streaming_source(source, cancellation.clone())
        }
        (OriginMode::ByteRanges, TransportInput::Streaming(_)) => {
            panic!("S28C Range origin должен дать seekable input")
        }
        (OriginMode::FullBody, TransportInput::Seekable(_)) => {
            panic!("S28C full-body origin должен дать streaming input")
        }
    };
    let mut demux_registry = DemuxRegistry::new();
    demux_registry
        .register(Box::new(
            SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("S28C factory"),
        ))
        .expect("register S28C factory");
    let hints = DemuxHints::none()
        .with_extension(DemuxSourceExtension::new(fixture.extension).expect("S28C extension hint"))
        .with_container(DemuxContainerId::new(fixture.container_id).expect("S28C container hint"));
    let sniff_budget = DemuxSniffBudget::new(
        NonZeroUsize::new(4_096).expect("S28C sniff bytes"),
        NonZeroUsize::MIN,
        Duration::from_secs(1),
    )
    .expect("S28C sniff budget");
    let demuxer = demux_registry
        .open(input, hints, sniff_budget, cancellation.clone())
        .expect("open S28C demuxer");
    OpenedFixture {
        demuxer,
        cancellation,
        _origin: origin,
    }
}

/// Запускает S22 bounded worker для non-Range blocking format reader-а.
fn progressive(opened: OpenedFixture) -> (Box<dyn Demuxer + Send>, FixtureOrigin) {
    let limits = ProgressiveDemuxBufferLimits::new(
        NonZeroUsize::new(8).expect("S28C event capacity"),
        NonZeroUsize::new(1024 * 1024).expect("S28C encoded byte capacity"),
    );
    let retry_hint = DemuxRetryHint::new(DemuxRetryHint::MIN_RETRY_AFTER).expect("S28C retry hint");
    let demuxer = ProgressiveDemuxer::new(opened.demuxer, opened.cancellation, limits, retry_hint)
        .expect("S28C progressive worker");
    (Box::new(demuxer), opened._origin)
}

/// Ждёт worker event только на test owner-е.
fn next_progressive_event(demuxer: &mut dyn Demuxer) -> anyhow::Result<DemuxReadEvent> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match demuxer.next_event()? {
            DemuxReadEvent::TemporarilyUnavailable(_) if Instant::now() < deadline => {
                thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
            }
            event => return Ok(event),
        }
    }
}

/// Читает первый audio packet, не скрывая lifecycle ordering.
fn next_packet(demuxer: &mut dyn Demuxer, progressive: bool) -> Packet {
    loop {
        let event = if progressive {
            next_progressive_event(demuxer).expect("S28C progressive event")
        } else {
            demuxer.next_event().expect("S28C Range event")
        };
        match event {
            DemuxReadEvent::Packet(packet) => return packet,
            DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => panic!("S28C HTTP fixture должен содержать packet"),
            DemuxReadEvent::TemporarilyUnavailable(_) => panic!("S28C readiness deadline"),
        }
    }
}

/// Дочитывает progressive stream до clean worker-owned EOS.
fn read_progressive_to_eos(demuxer: &mut dyn Demuxer) {
    loop {
        match next_progressive_event(demuxer).expect("read S28C progressive tail") {
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => return,
            DemuxReadEvent::TemporarilyUnavailable(_) => panic!("S28C readiness deadline"),
        }
    }
}

/// Проверяет одинаковый track, duration и packet contract для Range и non-Range paths.
fn assert_http_fixture_contract(
    fixture: &audio_fixtures::AudioContainerFixture,
    demuxer: &mut dyn Demuxer,
    progressive: bool,
) {
    assert_eq!(
        demuxer.duration().is_some(),
        if progressive {
            fixture.streaming_duration_is_known
        } else {
            fixture.duration_is_known
        },
        "{} HTTP duration authority",
        fixture.extension
    );
    let audio_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .expect("S28C HTTP audio track");
    assert_eq!(audio_track.codec_id, fixture.codec_id);
    assert_eq!(audio_track.sample_rate, Some(fixture.sample_rate));
    assert_eq!(audio_track.channels, Some(1));
    assert_eq!(
        audio_track.codec_private.as_deref(),
        fixture.codec_private.as_deref()
    );
    let audio_track_id = audio_track.id;
    let packet = next_packet(demuxer, progressive);
    assert_eq!(packet.track_id, audio_track_id);
    assert_eq!(packet.data.as_ref(), fixture.first_packet.as_slice());
    assert_eq!(packet.track_pts.expect("HTTP packet PTS").units.get(), 0);
    assert_eq!(packet.track_dts.expect("HTTP packet DTS").units.get(), 0);
    assert_eq!(
        packet
            .track_duration
            .expect("HTTP packet duration")
            .units
            .get(),
        fixture.first_packet_duration_units
    );
}

/// Все current audio families проходят настоящий seekable HTTP Range path.
#[test]
fn range_audio_containers_preserve_codec_packet_and_seek_contract() {
    for (index, fixture) in audio_fixtures::fixtures().into_iter().enumerate() {
        let mut opened = open_fixture(&fixture, OriginMode::ByteRanges, index as u64 + 1);
        assert_eq!(opened.demuxer.seekability(), DemuxSeekability::Seekable);
        opened
            .demuxer
            .seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))
            .expect("S28C Range seek");
        assert_http_fixture_contract(&fixture, opened.demuxer.as_mut(), false);
    }
}

/// Non-Range path остаётся NotSeekable, typed reject не ломает дальнейший playback/EOS.
#[test]
fn non_range_audio_containers_reject_seek_then_continue_progressive_playback() {
    for (index, fixture) in audio_fixtures::fixtures().into_iter().enumerate() {
        let mut opened = open_fixture(&fixture, OriginMode::FullBody, index as u64 + 101);
        assert!(matches!(
            opened.demuxer.seekability(),
            DemuxSeekability::NotSeekable { .. }
        ));
        let seek_error = opened
            .demuxer
            .seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))
            .expect_err("S28C non-Range seek должен fail-нуться");
        assert!(matches!(
            seek_error.downcast_ref::<MediaDemuxError>(),
            Some(MediaDemuxError::SeekUnavailable { .. })
        ));
        let (mut demuxer, _origin) = progressive(opened);
        assert_http_fixture_contract(&fixture, demuxer.as_mut(), true);
        read_progressive_to_eos(demuxer.as_mut());
    }
}
