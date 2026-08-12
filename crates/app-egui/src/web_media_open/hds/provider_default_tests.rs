//! Вертикальный proof HDS ProviderDefault через production app composition.
//!
//! Тест изолирует только внешний extractor и origin. Normalization/planning,
//! HDS discovery, F4F/FLV demux, capability filtering, catalog publication и
//! receipted seek проходят настоящими production boundaries.

#![cfg(unix)]

use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use audio::{AudioDecodeCapabilityProvider, ProductionAudioDecoderFactory};
use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, CURRENT_CAPABILITY_SCHEMA_VERSION,
    SupportedVideoOutput, SystemCapabilities,
};
use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, H264Profile, SupportedVideoDecodeFormat,
    VideoCodec as DecodeVideoCodec, VideoProfile,
};
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, TrackKind};
use player_core::{
    PreparedDemuxSeekOutcome, PreparedDemuxSeekPort, PreparedDemuxSeekReceipt,
    PreparedDemuxSeekRequestId,
};
use rustiplayer_config::{
    NetworkConfig, PlayerDemuxConfig, VideoCodec as ConfigVideoCodec, YtDlpConfig,
};
use source_core::CancellationToken;
use tempfile::TempDir;
use video_frame_contract::VideoFrameContract;
use web_media_core::StreamLayoutKind;

use crate::web_media_stream_model::component_variants::{
    WebMediaComponentVariantProjection, WebMediaInstalledComponentVariantPresentation,
};

use super::super::content_probe::ContentProbeRejection;
use super::super::{YtDlpCandidateOpenIntent, prepare_yt_dlp_web_media};

/// Маркер разделяет owner и child без изменения process-global environment.
const CHILD_PROCESS_MARKER_ENV: &str = "RUSTIPLAYER_HDS_PROVIDER_DEFAULT_CHILD";
/// Fake extractor получает точный document только через child-local environment.
const YT_DLP_DOCUMENT_ENV: &str = "RUSTIPLAYER_HDS_PROVIDER_DEFAULT_YTDLP_JSON";
/// Child получает только loopback test-origin address для наблюдения fetch count.
const HDS_FIXTURE_ORIGIN_ADDRESS_ENV: &str = "RUSTIPLAYER_HDS_PROVIDER_DEFAULT_ORIGIN_ADDRESS";
/// Exact libtest path не запускает соседние тесты повторно в subprocess-е.
const CHILD_TEST_NAME: &str = "web_media_open::hds::provider_default_tests::null_codec_provider_default_filters_unsupported_hds_and_opens_playable_catalog";
/// Второй exact child доказывает terminal classification transport failure-а.
const INFRA_FAILURE_CHILD_TEST_NAME: &str = "web_media_open::hds::provider_default_tests::all_fragment_http_failures_are_terminal_not_content_probe_rejections";
/// Третий exact child фиксирует terminal classification external bootstrap failure-а.
const BOOTSTRAP_FAILURE_CHILD_TEST_NAME: &str = "web_media_open::hds::provider_default_tests::external_bootstrap_http_failure_is_terminal_not_content_probe_rejection";
/// Test-only endpoint возвращает число fetch-ей второго fragment selected row.
const SELECTED_SECOND_FRAGMENT_COUNT_ENDPOINT: &str = "/__selected-second-fragment-count";
/// Exact media path нужен и owner-, и child-side assertions.
const SELECTED_SECOND_FRAGMENT_PATH: &str = "/media/playable-highSeg1-Frag2";
/// Все network/worker ожидания ограничены одним коротким deadline-ом.
const TEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Audio topology synthetic F4F rendition-а задаётся типом, а не позиционным bool.
#[derive(Clone, Copy)]
enum FixtureAudioCodec {
    /// Approved HDS base profile.
    Aac,
    /// Parser-valid, но запрещённый HDS sibling.
    Mp3,
}

/// Fixture mode явно отделяет playable origin от двух transport failure boundaries.
#[derive(Clone, Copy)]
enum FixtureResourceAvailability {
    /// Bootstrap и все advertised F4F fragments доступны.
    Available,
    /// Manifest/bootstrap доступны, но каждый F4F request получает 404.
    FragmentsMissing,
    /// Manifest доступен, но внешний bootstrap request получает 404.
    BootstrapMissing,
}

/// Loopback origin с immutable route table и журналом traversal-а.
struct HdsFixtureOrigin {
    /// Ephemeral loopback address нужен для document URL.
    address: SocketAddr,
    /// Cooperative stop flag завершает nonblocking accept loop.
    stop_requested: Arc<AtomicBool>,
    /// Secret-free paths позволяют доказать discovery и exact open.
    requested_paths: Arc<Mutex<Vec<String>>>,
    /// Join handle не оставляет worker после fixture lifetime.
    worker: Option<JoinHandle<()>>,
}

impl HdsFixtureOrigin {
    /// Запускает origin с unavailable, unsupported и двумя playable renditions.
    fn spawn(resource_availability: FixtureResourceAvailability) -> Self {
        let mut routes = HashMap::from([("/root.f4m", hds_manifest())]);
        if !matches!(
            resource_availability,
            FixtureResourceAvailability::BootstrapMissing
        ) {
            routes.insert("/media/bootstrap.bin", vod_bootstrap());
        }
        if matches!(
            resource_availability,
            FixtureResourceAvailability::Available
        ) {
            routes.extend([
                (
                    "/media/unsupportedSeg1-Frag1",
                    f4f_fragment(0, FixtureAudioCodec::Mp3),
                ),
                (
                    "/media/playable-highSeg1-Frag1",
                    f4f_fragment(0, FixtureAudioCodec::Aac),
                ),
                (
                    "/media/playable-highSeg1-Frag2",
                    f4f_fragment(1_000, FixtureAudioCodec::Aac),
                ),
                (
                    "/media/playable-lowSeg1-Frag1",
                    f4f_fragment(0, FixtureAudioCodec::Aac),
                ),
                (
                    "/media/playable-lowSeg1-Frag2",
                    f4f_fragment(1_000, FixtureAudioCodec::Aac),
                ),
            ]);
        }
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind HDS app fixture origin");
        listener
            .set_nonblocking(true)
            .expect("set HDS app fixture origin nonblocking");
        let address = listener.local_addr().expect("read HDS app fixture address");
        let stop_requested = Arc::new(AtomicBool::new(false));
        let worker_stop_requested = Arc::clone(&stop_requested);
        let requested_paths = Arc::new(Mutex::new(Vec::new()));
        let worker_requested_paths = Arc::clone(&requested_paths);
        let worker = thread::Builder::new()
            .name("hds-provider-default-origin".to_owned())
            .spawn(move || {
                while !worker_stop_requested.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _peer)) => {
                            let request = read_http_request(&mut stream);
                            let path = request_path(&request);
                            let response = if path == SELECTED_SECOND_FRAGMENT_COUNT_ENDPOINT {
                                let selected_fragment_count = requested_path_count(
                                    &worker_requested_paths
                                        .lock()
                                        .expect("lock HDS app requested paths"),
                                    SELECTED_SECOND_FRAGMENT_PATH,
                                );
                                http_response(
                                    "200 OK",
                                    selected_fragment_count.to_string().as_bytes(),
                                )
                            } else {
                                worker_requested_paths
                                    .lock()
                                    .expect("lock HDS app requested paths")
                                    .push(path.clone());
                                routes.get(path.as_str()).map_or_else(
                                    || http_response("404 Not Found", b"missing fixture route"),
                                    |body| http_response("200 OK", body),
                                )
                            };
                            stream
                                .write_all(&response)
                                .expect("write HDS app fixture response");
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("HDS app fixture accept failed: {error}"),
                    }
                }
            })
            .expect("spawn HDS app fixture worker");
        Self {
            address,
            stop_requested,
            requested_paths,
            worker: Some(worker),
        }
    }

    /// Возвращает direct F4M URL, который fake extractor передаст production app.
    fn manifest_url(&self) -> String {
        format!("http://{}/root.f4m", self.address)
    }

    /// Снимает журнал только после завершения child process-а.
    fn requested_paths(&self) -> Vec<String> {
        self.requested_paths
            .lock()
            .expect("lock HDS app requested paths")
            .clone()
    }
}

impl Drop for HdsFixtureOrigin {
    /// Останавливает и присоединяет worker без detached fixture state.
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join HDS app fixture worker");
        }
    }
}

/// Один test становится owner-ом либо isolated child-ом по scoped marker-у.
#[test]
fn null_codec_provider_default_filters_unsupported_hds_and_opens_playable_catalog() {
    if env::var_os(CHILD_PROCESS_MARKER_ENV).is_some() {
        assert_child_hds_open();
        return;
    }

    assert_owner_subprocess_succeeds();
}

/// Owner сохраняет origin и fake executable живыми до полного child результата.
fn assert_owner_subprocess_succeeds() {
    let origin = HdsFixtureOrigin::spawn(FixtureResourceAvailability::Available);
    let fake_tools = TempDir::new().expect("create HDS fake-tools directory");
    install_fake_yt_dlp(fake_tools.path());
    let child_output = run_isolated_test_child(
        fake_tools.path(),
        CHILD_TEST_NAME,
        yt_dlp_document(&origin.manifest_url()),
        origin.address,
    );

    assert!(
        child_output.status.success(),
        "HDS app child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&child_output.stdout),
        String::from_utf8_lossy(&child_output.stderr)
    );
    let requested_paths = origin.requested_paths();
    assert_eq!(
        requested_path_count(&requested_paths, "/media/unavailableSeg1-Frag1"),
        1,
        "unavailable sibling должен быть проверен один раз и не маскировать admitted rows"
    );
    assert_eq!(
        requested_path_count(&requested_paths, "/media/unsupportedSeg1-Frag1"),
        1,
        "discovery должен ровно один раз probe-ить unsupported sibling"
    );
    assert_eq!(
        requested_path_count(&requested_paths, "/media/playable-lowSeg1-Frag1"),
        1,
        "discovery должен ровно один раз проверить playable sibling"
    );
    assert_eq!(
        requested_path_count(&requested_paths, "/media/playable-highSeg1-Frag1"),
        1,
        "выбранный discovery demux должен перейти в runtime без повторного fetch"
    );
    assert!(
        requested_path_count(&requested_paths, SELECTED_SECOND_FRAGMENT_PATH) >= 1,
        "playback/seek должен открыть второй fragment выбранной rendition"
    );
}

/// Transport failure всех siblings остаётся terminal и не открывает parent fallback gate.
#[test]
fn all_fragment_http_failures_are_terminal_not_content_probe_rejections() {
    if env::var_os(CHILD_PROCESS_MARKER_ENV).is_some() {
        assert_child_hds_transport_failure_is_terminal();
        return;
    }

    let origin = HdsFixtureOrigin::spawn(FixtureResourceAvailability::FragmentsMissing);
    let fake_tools = TempDir::new().expect("create failing HDS fake-tools directory");
    install_fake_yt_dlp(fake_tools.path());
    let child_output = run_isolated_test_child(
        fake_tools.path(),
        INFRA_FAILURE_CHILD_TEST_NAME,
        yt_dlp_document(&origin.manifest_url()),
        origin.address,
    );
    assert!(
        child_output.status.success(),
        "failing HDS app child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&child_output.stdout),
        String::from_utf8_lossy(&child_output.stderr)
    );
    let requested_paths = origin.requested_paths();
    for fragment_path in [
        "/media/unavailableSeg1-Frag1",
        "/media/unsupportedSeg1-Frag1",
        "/media/playable-highSeg1-Frag1",
        "/media/playable-lowSeg1-Frag1",
    ] {
        assert_eq!(
            requested_path_count(&requested_paths, fragment_path),
            1,
            "discovery должен boundedly проверить каждый unavailable sibling"
        );
    }
}

/// External bootstrap HTTP failure остаётся terminal до любого rendition proof/fallback.
#[test]
fn external_bootstrap_http_failure_is_terminal_not_content_probe_rejection() {
    if env::var_os(CHILD_PROCESS_MARKER_ENV).is_some() {
        assert_child_hds_transport_failure_is_terminal();
        return;
    }

    let origin = HdsFixtureOrigin::spawn(FixtureResourceAvailability::BootstrapMissing);
    let fake_tools = TempDir::new().expect("create bootstrap-failing HDS fake-tools directory");
    install_fake_yt_dlp(fake_tools.path());
    let child_output = run_isolated_test_child(
        fake_tools.path(),
        BOOTSTRAP_FAILURE_CHILD_TEST_NAME,
        yt_dlp_document(&origin.manifest_url()),
        origin.address,
    );
    assert!(
        child_output.status.success(),
        "bootstrap-failing HDS app child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&child_output.stdout),
        String::from_utf8_lossy(&child_output.stderr)
    );
    let requested_paths = origin.requested_paths();
    assert!(
        requested_path_count(&requested_paths, "/media/bootstrap.bin") >= 1,
        "external bootstrap failure должен пройти bounded transport attempts"
    );
    for fragment_path in [
        "/media/unavailableSeg1-Frag1",
        "/media/unsupportedSeg1-Frag1",
        "/media/playable-highSeg1-Frag1",
        "/media/playable-lowSeg1-Frag1",
    ] {
        assert_eq!(
            requested_path_count(&requested_paths, fragment_path),
            0,
            "bootstrap failure должен завершить discovery до fragment fetch"
        );
    }
}

/// Запускает ровно один test child с process-local extractor document и PATH.
fn run_isolated_test_child(
    fake_tools_directory: &Path,
    test_name: &str,
    yt_dlp_document: String,
    origin_address: SocketAddr,
) -> Output {
    Command::new(env::current_exe().expect("current app-egui test binary"))
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(CHILD_PROCESS_MARKER_ENV, "1")
        .env(YT_DLP_DOCUMENT_ENV, yt_dlp_document)
        .env(HDS_FIXTURE_ORIGIN_ADDRESS_ENV, origin_address.to_string())
        .env("PATH", path_with_fake_tools_first(fake_tools_directory))
        .output()
        .expect("spawn isolated HDS app test child")
}

/// Считает exact path requests без зависимости от общего порядка probe-ов.
fn requested_path_count(requested_paths: &[String], expected_path: &str) -> usize {
    requested_paths
        .iter()
        .filter(|path| path.as_str() == expected_path)
        .count()
}

/// Child проходит real app composition до catalog, packets и seek receipt.
fn assert_child_hds_open() {
    let locator =
        service_ytdlp::parse_yt_dlp_media_locator("https://page.example.test/hds-provider-default")
            .expect("parse HDS page locator");
    let audio_decoder_factory = ProductionAudioDecoderFactory::default();
    let mut prepared = prepare_yt_dlp_web_media(
        &locator,
        &NetworkConfig::default(),
        &YtDlpConfig::default(),
        &PlayerDemuxConfig::default(),
        &[ConfigVideoCodec::H264],
        &h264_system_capabilities(),
        audio_decoder_factory.audio_decode_capability_snapshot(),
        YtDlpCandidateOpenIntent::BestPlayable,
        CancellationToken::new(),
        || false,
    )
    .expect("null-codec HDS ProviderDefault должен открыть playable rendition");

    assert_eq!(
        prepared.stream_configuration.active_candidate().layout,
        StreamLayoutKind::ContentProbed
    );
    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::Coupled { coupled, .. },
    ) = prepared.stream_configuration.component_variant_projection()
    else {
        panic!("HDS ProviderDefault должен публиковать coupled catalog");
    };
    assert_eq!(coupled.variants.len(), 2);
    assert_eq!(coupled.active_index, 0);
    assert_eq!(coupled.variants[0].video.height, Some(1080));
    assert_eq!(coupled.variants[1].video.height, Some(720));

    assert_av_tracks_and_packets(prepared.demuxer.as_mut());

    let playback_window = prepared
        .playback_window
        .expect("HDS VOD должен публиковать bounded playback window");
    assert_eq!(playback_window.start().as_duration(), Duration::ZERO);
    assert_eq!(
        playback_window.end_exclusive().map(|end| end.as_duration()),
        Some(Duration::from_secs(2))
    );
    let second_fragment_requests_before_seek = selected_second_fragment_request_count();
    let seek_port = prepared
        .demux_seek_port
        .as_ref()
        .cloned()
        .expect("HDS VOD должен публиковать receipted seek port");
    let request_id = PreparedDemuxSeekRequestId::new(1);
    seek_port
        .enqueue_seek(
            request_id,
            DemuxSeekRequest::accurate(Duration::from_millis(1_500)),
        )
        .expect("HDS app seek должен быть принят worker-ом");
    let receipt = wait_for_seek_receipt(seek_port.as_ref());
    assert_eq!(receipt.request_id, request_id);
    let PreparedDemuxSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!(
            "HDS app seek должен завершиться успехом: {:?}",
            receipt.outcome
        );
    };
    assert_eq!(
        result.requested_position.as_duration(),
        Duration::from_millis(1_500)
    );
    assert_eq!(result.actual_position.as_duration(), Duration::from_secs(1));
    let second_fragment_requests_after_seek = selected_second_fragment_request_count();
    assert!(
        second_fragment_requests_after_seek > second_fragment_requests_before_seek,
        "receipted seek должен заново открыть выбранный target fragment"
    );
}

/// Читает test-origin counter без process-global shared memory или timing guesses.
fn selected_second_fragment_request_count() -> usize {
    let origin_address = env::var(HDS_FIXTURE_ORIGIN_ADDRESS_ENV)
        .expect("HDS fixture origin address must reach child")
        .parse::<SocketAddr>()
        .expect("HDS fixture origin address is valid");
    let mut stream = TcpStream::connect(origin_address).expect("connect HDS count endpoint");
    let request = format!(
        "GET {SELECTED_SECOND_FRAGMENT_COUNT_ENDPOINT} HTTP/1.1\r\nHost: {origin_address}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .expect("write HDS count request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("read HDS count response");
    let response = String::from_utf8(response).expect("HDS count response is UTF-8 HTTP");
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("HDS count response has header terminator")
        .trim()
        .parse()
        .expect("HDS count response body is usize")
}

/// Проверяет, что unavailable F4F transport не маскируется retryable rejection-ом.
fn assert_child_hds_transport_failure_is_terminal() {
    let locator = service_ytdlp::parse_yt_dlp_media_locator(
        "https://page.example.test/hds-terminal-transport-failure",
    )
    .expect("parse failing HDS page locator");
    let audio_decoder_factory = ProductionAudioDecoderFactory::default();
    let error = match prepare_yt_dlp_web_media(
        &locator,
        &NetworkConfig::default(),
        &YtDlpConfig::default(),
        &PlayerDemuxConfig::default(),
        &[ConfigVideoCodec::H264],
        &h264_system_capabilities(),
        audio_decoder_factory.audio_decode_capability_snapshot(),
        YtDlpCandidateOpenIntent::BestPlayable,
        CancellationToken::new(),
        || false,
    ) {
        Ok(_) => panic!("all-unavailable HDS catalog не должен считаться playable"),
        Err(error) => error,
    };
    assert!(
        error
            .downcast_ref::<web_media_hds::HdsNoPlayableRendition>()
            .is_none(),
        "transport failure не является content/profile rejection-ом"
    );
    assert!(
        !matches!(
            error.downcast_ref::<ContentProbeRejection>(),
            Some(ContentProbeRejection::NoPlayableAdaptiveVariant)
        ),
        "transport failure не должен открывать BestPlayable parent fallback"
    );
}

/// Проверяет lazy track publication и оба encoded packet kind-а selected F4F.
fn assert_av_tracks_and_packets(demuxer: &mut dyn Demuxer) {
    let mut video_track_seen = demuxer
        .tracks()
        .iter()
        .any(|track| track.kind == TrackKind::Video && track.codec_id == "V_MPEG4/ISO/AVC");
    let mut audio_track_seen = demuxer
        .tracks()
        .iter()
        .any(|track| track.kind == TrackKind::Audio && track.codec_id == "A_AAC");
    let mut video_packet_seen = false;
    let mut audio_packet_seen = false;
    for _ in 0..32 {
        match next_ready_event(demuxer) {
            DemuxReadEvent::TracksChanged(update) => {
                video_track_seen |= update.tracks.iter().any(|track| {
                    track.kind == TrackKind::Video && track.codec_id == "V_MPEG4/ISO/AVC"
                });
                audio_track_seen |= update
                    .tracks
                    .iter()
                    .any(|track| track.kind == TrackKind::Audio && track.codec_id == "A_AAC");
            }
            DemuxReadEvent::Packet(packet) => match packet.kind {
                TrackKind::Video => video_packet_seen = true,
                TrackKind::Audio => audio_packet_seen = true,
            },
            DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => break,
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                unreachable!("next_ready_event filters temporary readiness")
            }
        }
        if video_track_seen && audio_track_seen && video_packet_seen && audio_packet_seen {
            break;
        }
    }
    assert!(
        video_track_seen,
        "playable F4F должен опубликовать H.264 track"
    );
    assert!(
        audio_track_seen,
        "playable F4F должен опубликовать AAC track"
    );
    assert!(video_packet_seen, "playable F4F должен выдать H.264 packet");
    assert!(audio_packet_seen, "playable F4F должен выдать AAC packet");
}

/// Poll-ит nonblocking demux boundary до одного готового event-а.
fn next_ready_event(demuxer: &mut dyn Demuxer) -> DemuxReadEvent {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        let call_started = Instant::now();
        match demuxer.next_event().expect("HDS app demux event") {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(
                    call_started.elapsed() < Duration::from_millis(50),
                    "HDS app owner poll должен оставаться nonblocking"
                );
                assert!(Instant::now() < deadline, "HDS app demux worker timed out");
                thread::sleep(Duration::from_millis(2));
            }
            event => return event,
        }
    }
}

/// Ждёт terminal seek receipt только внутри bounded test child-а.
fn wait_for_seek_receipt(port: &dyn PreparedDemuxSeekPort) -> PreparedDemuxSeekReceipt {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if let Some(receipt) = port.poll_seek_receipt() {
            return receipt;
        }
        assert!(Instant::now() < deadline, "HDS app seek receipt timed out");
        thread::sleep(Duration::from_millis(2));
    }
}

/// Capability report содержит один реальный H.264 software-compatible output.
fn h264_system_capabilities() -> SystemCapabilities {
    let backend_id = DecodeBackendId::new("fixture_h").expect("valid fixture backend id");
    let output = SupportedVideoOutput {
        backend: backend_id.clone(),
        decode_format: SupportedVideoDecodeFormat {
            codec: DecodeVideoCodec::H264,
            profile: VideoProfile::H264(H264Profile::Baseline),
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            max_width: Some(3840),
            max_height: Some(2160),
            max_fps: Some(60.0),
            hdr_input: false,
        },
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
    };
    SystemCapabilities {
        schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![BackendCapabilities {
            backend_id,
            display_name: "Fixture H.264 backend".to_owned(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            raw_supported_outputs: vec![output.clone()],
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            diagnostics: Vec::new(),
        }],
        render_backends: Vec::new(),
        playable_video_outputs: vec![output],
    }
}

/// Устанавливает process-compatible fake extractor в test-owned directory.
fn install_fake_yt_dlp(fake_tools_directory: &Path) {
    let executable_path = fake_tools_directory.join("yt-dlp");
    let script = concat!(
        "#!/bin/sh\n",
        "set -eu\n",
        "printf '%s\\n' \"${RUSTIPLAYER_HDS_PROVIDER_DEFAULT_YTDLP_JSON:?missing fixture JSON}\"\n",
    );
    fs::write(&executable_path, script).expect("write fake HDS yt-dlp executable");
    let mut permissions = fs::metadata(&executable_path)
        .expect("read fake HDS yt-dlp metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable_path, permissions).expect("mark fake HDS yt-dlp executable");
}

/// Строит child-only PATH без global mutation и separator assumptions.
fn path_with_fake_tools_first(fake_tools_directory: &Path) -> OsString {
    let inherited_path = env::var_os("PATH").unwrap_or_default();
    env::join_paths(
        std::iter::once(fake_tools_directory.to_path_buf())
            .chain(env::split_paths(&inherited_path)),
    )
    .expect("join child-only HDS fake-tools PATH")
}

/// Null codec fields сохраняют provider-owned ContentProbed HDS contract.
fn yt_dlp_document(manifest_url: &str) -> String {
    format!(
        r#"{{"id":"hds-provider-default","title":"HDS ProviderDefault","formats":[{{"format_id":"hds-null-codecs","url":"{manifest_url}","manifest_url":"{manifest_url}","protocol":"f4m","ext":"flv","container":"flv","vcodec":null,"acodec":null}}]}}"#
    )
}

/// Читает один HTTP request header block.
fn read_http_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(TEST_TIMEOUT))
        .expect("set HDS app fixture read timeout");
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1_024];
    loop {
        let read = stream
            .read(&mut chunk)
            .expect("read HDS app fixture request");
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(request).expect("HDS app fixture request is UTF-8 HTTP")
}

/// Извлекает path без query и не сохраняет request headers.
fn request_path(request: &str) -> String {
    request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Формирует закрывающий соединение HTTP/1.1 response.
fn http_response(status: &str, body: &[u8]) -> Vec<u8> {
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut response = headers.into_bytes();
    response.extend_from_slice(body);
    response
}

/// Missing/unsupported rows выше по bitrate; после filtering default-ом станет 1080p AAC.
fn hds_manifest() -> Vec<u8> {
    br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><streamType>recorded</streamType><duration>2</duration><baseURL>media/</baseURL><media url="unavailable" bitrate="12000" width="4096" height="2160" bootstrapInfoId="boot"/><media url="unsupported" bitrate="9000" width="3840" height="2160" bootstrapInfoId="boot"/><media url="playable-high" bitrate="6000" width="1920" height="1080" bootstrapInfoId="boot"/><media url="playable-low" bitrate="3000" width="1280" height="720" bootstrapInfoId="boot"/><bootstrapInfo id="boot" url="bootstrap.bin"/></manifest>"#.to_vec()
}

/// Строит VOD `abst/asrt/afrt` с двумя fragments и terminal marker-ом.
fn vod_bootstrap() -> Vec<u8> {
    let segment_table = segment_run_table(2);
    let fragment_table = fragment_run_table();
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.push(0);
    payload.extend_from_slice(&1_000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u64.to_be_bytes());
    payload.extend_from_slice(&0_u64.to_be_bytes());
    payload.extend_from_slice(b"fixture\0");
    payload.push(0);
    payload.push(0);
    payload.extend_from_slice(b"\0\0");
    payload.push(1);
    payload.extend_from_slice(&segment_table);
    payload.push(1);
    payload.extend_from_slice(&fragment_table);
    iso_box(b"abst", &payload)
}

/// `asrt` сопоставляет оба fragments одному segment-у.
fn segment_run_table(fragments_per_segment: u32) -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0, 0];
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&fragments_per_segment.to_be_bytes());
    iso_box(b"asrt", &payload)
}

/// `afrt` задаёт два секундных fragments и конец presentation.
fn fragment_run_table() -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&1_000_u32.to_be_bytes());
    payload.push(0);
    payload.extend_from_slice(&2_u32.to_be_bytes());
    append_fragment_run(&mut payload, 1, 0, 1_000, None);
    // Unified Streaming использует нулевой ID для terminal END_OF_PRESENTATION.
    append_fragment_run(&mut payload, 0, 0, 0, Some(0));
    iso_box(b"afrt", &payload)
}

/// Кодирует одну wire-level Adobe FRAGMENTRUNENTRY.
fn append_fragment_run(
    payload: &mut Vec<u8>,
    first_fragment: u32,
    first_timestamp: u64,
    duration: u32,
    discontinuity: Option<u8>,
) {
    payload.extend_from_slice(&first_fragment.to_be_bytes());
    payload.extend_from_slice(&first_timestamp.to_be_bytes());
    payload.extend_from_slice(&duration.to_be_bytes());
    if let Some(indicator) = discontinuity {
        payload.push(indicator);
    }
}

/// Собирает доставляемый F4F media fragment с AAC либо unsupported MP3 payload.
///
/// Bootstrap доступен provider-у отдельным ресурсом, как и на живом HDS VOD source.
fn f4f_fragment(timestamp: u32, audio_codec: FixtureAudioCodec) -> Vec<u8> {
    let mut flv_tags = flv_tag(9, timestamp, &avc_sequence());
    match audio_codec {
        FixtureAudioCodec::Aac => {
            flv_tags.extend_from_slice(&flv_tag(8, timestamp, &aac_sequence()));
            flv_tags.extend_from_slice(&flv_tag(8, timestamp + 40, &aac_frame(&[0x11, 0x22])));
        }
        FixtureAudioCodec::Mp3 => {
            flv_tags.extend_from_slice(&flv_tag(8, timestamp, &mp3_frame()));
        }
    }
    flv_tags.extend_from_slice(&flv_tag(9, timestamp + 40, &avc_keyframe()));

    let mut fragment = f4f_afra();
    fragment.extend_from_slice(&f4f_moof());
    fragment.extend_from_slice(&iso_box(b"mdat", &flv_tags));
    fragment
}

/// Минимальный valid `afra` для production F4F topology validator-а.
fn f4f_afra() -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0, 0];
    payload.extend_from_slice(&1_000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    iso_box(b"afra", &payload)
}

/// Минимальный `moof` с одним declared track run.
fn f4f_moof() -> Vec<u8> {
    let mut movie_header = vec![0, 0, 0, 0];
    movie_header.extend_from_slice(&1_u32.to_be_bytes());
    let mut track_header = vec![0, 0, 0, 0];
    track_header.extend_from_slice(&1_u32.to_be_bytes());
    let mut track_run = vec![0, 0, 0, 0];
    track_run.extend_from_slice(&1_u32.to_be_bytes());
    let mut track_fragment = iso_box(b"tfhd", &track_header);
    track_fragment.extend_from_slice(&iso_box(b"trun", &track_run));
    let mut payload = iso_box(b"mfhd", &movie_header);
    payload.extend_from_slice(&iso_box(b"traf", &track_fragment));
    iso_box(b"moof", &payload)
}

/// Кодирует ISO BMFF box только для test fixture bytes.
fn iso_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(8 + payload.len()).expect("HDS app fixture box fits u32");
    let mut bytes = Vec::with_capacity(8 + payload.len());
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(box_type);
    bytes.extend_from_slice(payload);
    bytes
}

/// Кодирует headerless FLV tag внутри F4F `mdat`.
fn flv_tag(tag_type: u8, timestamp: u32, payload: &[u8]) -> Vec<u8> {
    let payload_size = u32::try_from(payload.len()).expect("FLV app fixture payload fits u32");
    let timestamp_bytes = timestamp.to_be_bytes();
    let mut bytes = Vec::new();
    bytes.push(tag_type);
    bytes.extend_from_slice(&payload_size.to_be_bytes()[1..]);
    bytes.extend_from_slice(&timestamp_bytes[1..]);
    bytes.push(timestamp_bytes[0]);
    bytes.extend_from_slice(&[0, 0, 0]);
    bytes.extend_from_slice(payload);
    let tag_size = u32::try_from(11 + payload.len()).expect("FLV app fixture tag fits u32");
    bytes.extend_from_slice(&tag_size.to_be_bytes());
    bytes
}

/// Возвращает AVC sequence header с production-validated `avcC`.
fn avc_sequence() -> Vec<u8> {
    let mut payload = vec![0x17, 0, 0, 0, 0];
    payload.extend_from_slice(&[1, 66, 0, 30, 0xff, 0xe1, 0, 2, 0x67, 0x42, 1, 0, 1, 0x68]);
    payload
}

/// Возвращает minimal length-prefixed IDR access unit.
fn avc_keyframe() -> Vec<u8> {
    vec![0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 0]
}

/// Возвращает AAC-LC AudioSpecificConfig.
fn aac_sequence() -> Vec<u8> {
    vec![0xaf, 0, 0x12, 0x10]
}

/// Возвращает один raw AAC payload за FLV AAC packet header-ом.
fn aac_frame(frame: &[u8]) -> Vec<u8> {
    let mut payload = vec![0xaf, 1];
    payload.extend_from_slice(frame);
    payload
}

/// Возвращает self-describing legacy MP3 FLV tag payload для unsupported row.
fn mp3_frame() -> Vec<u8> {
    vec![0x2f, 0xff, 0xfb, 0x90, 0x64]
}
