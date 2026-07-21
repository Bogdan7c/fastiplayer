//! S22 real-container evidence для neutral progressive HTTP vertical slice.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::Engine as _;
use demux_api::{
    CompositeAvDemuxer, CompositeAvTrackSelection, CompositeComponentLeadPolicy, DemuxContainerId,
    DemuxHints, DemuxInput, DemuxRegistry, DemuxSniffBudget, DemuxSourceExtension,
    ProgressiveDemuxBufferLimits, ProgressiveDemuxer,
};
use media_core::{DemuxReadEvent, DemuxRetryHint, Demuxer, TrackId, TrackKind};
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

/// Tiny H.264 video-only MP4, generated once by FFmpeg 6.2 for hermetic tests.
const VIDEO_MP4_BASE64: &str = "AAAAIGZ0eXBpc29tAAACAGlzb21pc28yYXZjMW1wNDEAAAMNbW9vdgAAAGxtdmhkAAAAAAAAAAAAAAAAAAAD6AAAA+gAAQAAAQAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAgAAAjd0cmFrAAAAXHRraGQAAAADAAAAAAAAAAAAAAABAAAAAAAAA+gAAAAAAAAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAABAAAAAABAAAAAQAAAAAAAkZWR0cwAAABxlbHN0AAAAAAAAAAEAAAPoAAAAAAABAAAAAAGvbWRpYQAAACBtZGhkAAAAAAAAAAAAAAAAAABAAAAAQABVxAAAAAAALWhkbHIAAAAAAAAAAHZpZGUAAAAAAAAAAAAAAABWaWRlb0hhbmRsZXIAAAABWm1pbmYAAAAUdm1oZAAAAAEAAAAAAAAAAAAAACRkaW5mAAAAHGRyZWYAAAAAAAAAAQAAAAx1cmwgAAAAAQAAARpzdGJsAAAAtnN0c2QAAAAAAAAAAQAAAKZhdmMxAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAAAABAAEABIAAAASAAAAAAAAAABFUxhdmM2Mi4yOC4xMDIgbGlieDI2NAAAAAAAAAAAAAAAGP//AAAALGF2Y0MBQsAK/+EAFWdCwAraewEQAAADABAAAAMAKPEiagEABGjOD8gAAAAQcGFzcAAAAAEAAAABAAAAFGJ0cnQAAAAAAAATKAAAAAAAAAAYc3R0cwAAAAAAAAABAAAAAQAAQAAAAAAcc3RzYwAAAAAAAAABAAAAAQAAAAEAAAABAAAAFHN0c3oAAAAAAAACZQAAAAEAAAAUc3RjbwAAAAAAAAABAAADPQAAAGJ1ZHRhAAAAWm1ldGEAAAAAAAAAIWhkbHIAAAAAAAAAAG1kaXJhcHBsAAAAAAAAAAAAAAAALWlsc3QAAAAlqXRvbwAAAB1kYXRhAAAAAQAAAABMYXZmNjIuMTIuMTAyAAAACGZyZWUAAAJtbWRhdAAAAlMGBf//T9xF6b3m2Ui3lizYINkj7u94MjY0IC0gY29yZSAxNjUgcjMyMjIgYjM1NjA1YSAtIEguMjY0L01QRUctNCBBVkMgY29kZWMgLSBDb3B5bGVmdCAyMDAzLTIwMjUgLSBodHRwOi8vd3d3LnZpZGVvbGFuLm9yZy94MjY0Lmh0bWwgLSBvcHRpb25zOiBjYWJhYz0wIHJlZj0xIGRlYmxvY2s9MDowOjAgYW5hbHlzZT0wOjAgbWU9ZGlhIHN1Ym1lPTAgcHN5PTEgcHN5X3JkPTEuMDA6MC4wMiBtaXhlZF9yZWY9MCBtZV9yYW5nZT0xNiBjaHJvbWFfbWU9MSB0cmVsbGlzPTAgOHg4ZGN0PTAgY3FtPTAgZGVhZHpvbmU9MjEsMTEgZmFzdF9wc2tpcD0xIGNocm9tYV9xcF9vZmZzZXQ9MCB0aHJlYWRzPTEgbG9va2FoZWFkX3RocmVhZHM9MSBzbGljZWRfdGhyZWFkcz0wIG5yPTAgZGVjaW1hdGU9MSBpbnRlcmxhY2VkPTAgYmx1cmF5X2NvbXBhdD0wIGNvbnN0cmFpbmVkX2ludHJhPTAgYmZyYW1lcz0wIHdlaWdodHA9MCBrZXlpbnQ9MjUwIGtleWludF9taW49MSBzY2VuZWN1dD0wIGludHJhX3JlZnJlc2g9MCByYz1jcmYgbWJ0cmVlPTAgY3JmPTIzLjAgcWNvbXA9MC42MCBxcG1pbj0wIHFwbWF4PTY5IHFwc3RlcD00IGlwX3JhdGlvPTEuNDAgYXE9MACAAAAACmWIhDomKAAJAuA=";

const AUDIO_M4A_BASE64: &str = "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";

/// Tiny muxed VP9+Opus WebM, generated once by FFmpeg 6.2 for hermetic tests.
const MUXED_WEBM_BASE64: &str = "GkXfo59ChoEBQveBAULygQRC84EIQoKEd2VibUKHgQRChYECGFOAZwEAAAAAAAT6EU2bdKtNu4tTq4QVSalmU6yBoU27i1OrhBZUrmtTrIHYTbuMU6uEElTDZ1OsggGL7AEAAAAAAABoAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAVSalmsirXsYMPQkBNgI1MYXZmNjIuMTIuMTAyV0GNTGF2ZjYyLjEyLjEwMkSJiEBqAAAAAAAAFlSua0CtrgEAAAAAAAA/14EBc8WI4RVOQO1IXP2cgQAitZyDdW5kiIEAhoVWX1ZQOYOBASPjg4Q7msoA4JCwgRC6gRCagQJVsIRVuYEBrgEAAAAAAABc14ECc8WILtqFORDWzOacgQAitZyDdW5kiIEAhoZBX09QVVNWqoNjLqBWu4QExLQAg4EC4ZGfgQG1iEDncAAAAAAAYmSBEGOik09wdXNIZWFkAQE4AYC7AAAAAAASVMNnQNpzc6BjwIBnyJpFo4dFTkNPREVSRIeNTGF2ZjYyLjEyLjEwMnNz2mPAi2PFiOEVTkDtSFz9Z8ilRaOHRU5DT0RFUkSHmExhdmM2Mi4yOC4xMDIgbGlidnB4LXZwOWfIoUWjiERVUkFUSU9ORIeTMDA6MDA6MDAuMDAwMDAwMDAwAHNz12PAi2PFiC7ahTkQ1szmZ8iiRaOHRU5DT0RFUkSHlUxhdmM2Mi4yOC4xMDIgbGlib3B1c2fIoUWjiERVUkFUSU9ORIeTMDA6MDA6MDAuMjA4MDAwMDAwAB9DtnVCieeBAKPJggAAgHiCAbdsRyTqAkZv+rYDjIE01uheAesbsV7vyXgY8gqzIUHM0aaf4Yqbhp1rgeoEnutAiitHUCgJ0EZTAHRNtp96w/QA+aO/ggAVgHijP/esmIUDXCYKV5+K8o09bIVMwBPqQbZh53f7dxTMWHWvNRLH9wshBXhb4UZ4svXifY+1ePppKdPLo7mCACmAeJujElFFAKzjUZgqOLSDqFbpMHSeGsZ7FbYcChkq6O3Emtd53XZAdutGpV7d9egrqCh0sAWjt4IAPYB4m6MRtBy/Uih1GdqwJAwsUSZDbA//BI9QgCycHYoxq4Ic+sbnUMzJ9wP0HOjhbWiCb+KjuoIAUYB4m6NfdZz8STO/c/rkeazCRm0L4bmKPIbNcyzW2pEplP1EmVyzZhs/Iy83CL2CaSK296Wi1Quju4IAZYB4m6MXS922lJqWfP0tzgHa+EQUM4ZNzj3/4uyMMFraDmaE1J7E31fJjuaajPCVRFjXb/wpmMQSo7mCAHmAeJujX3Wc/EXvXpTrM9lGArnjG7FsF9NjNymkPGjDrN8mFGnLKUpF54nZ7i7pU2BI16Up08ujuYIAjYB4m6MSU6pITpKco1E0ZuEReGCRPfNQ2rcpm9Q8kk11qrWbS+mLWLrYoq4PwNVcQmSdRgPfi6O5ggChgHiboxJWzh/qBs7RgYcGHJW1jIhm50/ld2IQGsxGsQuWR5+uFiSgUuyGYV8q9fVM5RMJ/qHEo7GCALWASJujX3Wc/EkOxuZCaFcBU85u/OVpN4IrAUNW3avMnEKrO/mPDcjFBe2N0L3WoKehm4IAyQBIBdFHOi7/4JRQMk2r3TOp4vfJLxZY+JuBB3WihADN/mA=";

/// Full-body loopback server моделирует non-Range progressive origin.
struct FullBodyServer {
    /// Loopback listener address.
    address: SocketAddr,
    /// Accept-loop stop flag.
    stop: Arc<AtomicBool>,
    /// Join handle исключает test thread leaks.
    join_handle: Option<JoinHandle<()>>,
}

impl FullBodyServer {
    /// Запускает origin, который игнорирует Range и возвращает `200`.
    fn spawn(body: Vec<u8>) -> Self {
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
                        Ok((mut stream, _)) => respond_full_body(&mut stream, &body),
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

impl Drop for FullBodyServer {
    /// Останавливает origin и присоединяет thread.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
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
) -> (Box<dyn Demuxer + Send>, CancellationToken) {
    let server = FullBodyServer::spawn(body);
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
    let input = match opened.into_input() {
        TransportInput::Streaming(source) => {
            DemuxInput::streaming_source(source, cancellation.clone())
        }
        TransportInput::Seekable(_) => panic!("fixture origin intentionally ignores Range"),
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

    // `server` может завершиться после полного response: body уже принадлежит transport source.
    drop(server);
    (demuxer, cancellation)
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

#[test]
fn progressive_mp4_m4a_and_webm_open_with_real_hints_and_non_range_input() {
    let (video, video_cancel) = open_component(
        decode_fixture(VIDEO_MP4_BASE64),
        "mp4",
        "iso-bmff",
        MediaComponentRole::Video,
        1,
    );
    assert!(
        video
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Video)
    );
    drop(progressive(video, video_cancel));

    let (audio, audio_cancel) = open_component(
        decode_fixture(AUDIO_M4A_BASE64),
        "m4a",
        "iso-bmff",
        MediaComponentRole::Audio,
        2,
    );
    assert!(
        audio
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Audio)
    );
    drop(progressive(audio, audio_cancel));

    let (muxed, muxed_cancel) = open_component(
        decode_fixture(MUXED_WEBM_BASE64),
        "webm",
        "webm",
        MediaComponentRole::Muxed,
        3,
    );
    assert!(
        muxed
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Video)
    );
    assert!(
        muxed
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Audio)
    );
    drop(progressive(muxed, muxed_cancel));
}

#[test]
fn separate_progressive_mp4_and_m4a_compose_through_neutral_av_demuxer() {
    let (video, video_cancel) = open_component(
        decode_fixture(VIDEO_MP4_BASE64),
        "mp4",
        "iso-bmff",
        MediaComponentRole::Video,
        11,
    );
    let video = progressive(video, video_cancel);
    let video_track = selected_track(video.as_ref(), TrackKind::Video);
    let (audio, audio_cancel) = open_component(
        decode_fixture(AUDIO_M4A_BASE64),
        "m4a",
        "iso-bmff",
        MediaComponentRole::Audio,
        12,
    );
    let audio = progressive(audio, audio_cancel);
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
