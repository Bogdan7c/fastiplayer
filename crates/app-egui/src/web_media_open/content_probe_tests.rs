//! Герметичный вертикальный proof generic `ContentProbed` playback path-а.
//!
//! Тест намеренно проходит production boundaries: системный `yt-dlp` process,
//! candidate normalization/planning, HTTP Range transport, Symphonia demux и
//! codec-neutral production audio decoder. Единственная подмена — loopback
//! origin и executable `yt-dlp`, изолированные окружением дочернего test process-а.

#![cfg(unix)]

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use audio::decoder::{
    AudioDecoderConfig, AudioPacketTimeBase, AudioPacketTiming, EncodedAudioPacket,
};
use audio::{AudioDecodeCapabilityProvider, AudioDecoderFactory, ProductionAudioDecoderFactory};
use media_core::{DemuxReadEvent, DemuxRetryHint, Demuxer, Packet, TrackInfo, TrackKind};
use rustiplayer_config::{NetworkConfig, PlayerDemuxConfig, VideoCodec, YtDlpConfig};
use source_core::CancellationToken;
use tempfile::TempDir;
use web_media_core::StreamLayoutKind;

use super::{YtDlpCandidateOpenIntent, prepare_yt_dlp_web_media};

/// Переиспользуем существующий generated Ogg/Opus fixture вместо бинарного blob-а.
#[allow(dead_code)]
#[path = "../../../symphonia-demux/src/factory/tests/audio_fixtures.rs"]
mod audio_fixtures;

/// Маркер отличает изолированный child от owner test process-а без global env mutation.
const CHILD_PROCESS_MARKER_ENV: &str = "RUSTIPLAYER_CONTENT_PROBE_CHILD";
/// Fake extractor получает document только через своё дочернее окружение.
const YT_DLP_DOCUMENT_ENV: &str = "RUSTIPLAYER_CONTENT_PROBE_YTDLP_JSON";
/// Exact libtest path не запускает соседние тесты внутри subprocess-а.
const CHILD_TEST_NAME: &str =
    "web_media_open::content_probe_tests::http_ogg_opus_null_codecs_reach_production_pcm";
/// Exact child path вертикально проверяет retry после typed content rejection.
const FALLBACK_CHILD_TEST_NAME: &str =
    "web_media_open::content_probe_tests::best_playable_content_rejection_opens_second_real_ogg";
/// Exact child path вертикально проверяет terminal transport failure.
const FATAL_CHILD_TEST_NAME: &str =
    "web_media_open::content_probe_tests::fatal_http_open_does_not_try_second_candidate";
/// Bounded ожидание progressive readiness защищает тест от вечного зависания.
const DEMUX_EVENT_DEADLINE: Duration = Duration::from_secs(2);

/// Один immutable ответ loopback origin-а для всех запросов конкретного candidate-а.
enum FixtureOriginResponse {
    /// Настоящий seekable Ogg resource обслуживает production Range transport.
    Ogg(Vec<u8>),
    /// HTTP failure доказывает terminal ветку до demux/content proof-а.
    HttpFailure,
}

/// Loopback origin владеет exact Ogg bytes и обслуживает bounded Range requests.
struct RangeFixtureOrigin {
    /// Адрес нужен одновременно для media URL и wake-up соединения при `Drop`.
    address: SocketAddr,
    /// Stop flag завершает неблокирующий accept loop без detached thread-а.
    stop_requested: Arc<AtomicBool>,
    /// Счётчик реальных HTTP requests доказывает порядок и границу fallback-а.
    request_count: Arc<AtomicUsize>,
    /// Join handle гарантирует завершение origin worker-а до удаления fixture state.
    worker: Option<JoinHandle<()>>,
}

impl RangeFixtureOrigin {
    /// Запускает hermetic HTTP origin только на loopback interface.
    fn spawn(ogg_bytes: Vec<u8>) -> Self {
        Self::spawn_with_response(FixtureOriginResponse::Ogg(ogg_bytes))
    }

    /// Запускает origin, который всегда возвращает terminal HTTP status.
    fn spawn_http_failure() -> Self {
        Self::spawn_with_response(FixtureOriginResponse::HttpFailure)
    }

    /// Создаёт общий bounded worker для media и failure fixtures.
    fn spawn_with_response(response: FixtureOriginResponse) -> Self {
        // ОС выбирает свободный ephemeral port, поэтому параллельные тесты не конфликтуют.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ContentProbed origin");
        listener
            .set_nonblocking(true)
            .expect("set ContentProbed origin nonblocking");
        let address = listener
            .local_addr()
            .expect("read ContentProbed origin address");
        let stop_requested = Arc::new(AtomicBool::new(false));
        let worker_stop_requested = Arc::clone(&stop_requested);
        let request_count = Arc::new(AtomicUsize::new(0));
        let worker_request_count = Arc::clone(&request_count);
        let worker = thread::Builder::new()
            .name("content-probed-ogg-origin".to_owned())
            .spawn(move || {
                while !worker_stop_requested.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            respond_to_fixture_request(
                                &mut stream,
                                &response,
                                worker_request_count.as_ref(),
                            );
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => return,
                    }
                }
            })
            .expect("spawn ContentProbed origin worker");

        Self {
            address,
            stop_requested,
            request_count,
            worker: Some(worker),
        }
    }

    /// Возвращает non-secret exact component URL для fake extractor document-а.
    fn media_url(&self) -> String {
        format!("http://{}/content-probed.ogg", self.address)
    }

    /// Возвращает число полноценных requests без учёта wake-up соединения в `Drop`.
    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }
}

impl Drop for RangeFixtureOrigin {
    /// Останавливает и присоединяет worker, не оставляя background state между тестами.
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join ContentProbed origin worker");
        }
    }
}

/// Читает один app-generated request и применяет immutable fixture response.
fn respond_to_fixture_request(
    stream: &mut TcpStream,
    response: &FixtureOriginResponse,
    request_count: &AtomicUsize,
) {
    // Read timeout превращает оборванный client request в bounded test failure.
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("set ContentProbed origin read timeout");
    let mut request_bytes = [0_u8; 4_096];
    let request_length = stream
        .read(&mut request_bytes)
        .expect("read ContentProbed HTTP request");
    if request_length == 0 {
        return;
    }
    request_count.fetch_add(1, Ordering::SeqCst);

    let request = String::from_utf8_lossy(&request_bytes[..request_length]);
    match response {
        FixtureOriginResponse::Ogg(ogg_bytes) => {
            respond_to_range_request(stream, &request, ogg_bytes);
        }
        FixtureOriginResponse::HttpFailure => respond_to_http_failure(stream),
    }
}

/// Возвращает exact requested byte interval настоящего Ogg resource-а.
fn respond_to_range_request(stream: &mut TcpStream, request: &str, ogg_bytes: &[u8]) {
    // Production HTTP provider обязан открыть seekable source через bounded Range.
    let (range_start, requested_end) = request
        .lines()
        .find_map(parse_range_header)
        .expect("ContentProbed provider должен отправить Range header");
    assert!(
        range_start < ogg_bytes.len(),
        "ContentProbed Range start находится за концом Ogg fixture"
    );
    let range_end = requested_end.min(ogg_bytes.len() - 1);
    let response_body = &ogg_bytes[range_start..=range_end];
    let response_headers = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Type: audio/ogg\r\nContent-Length: {}\r\nContent-Range: bytes {range_start}-{range_end}/{}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
        response_body.len(),
        ogg_bytes.len()
    );
    stream
        .write_all(response_headers.as_bytes())
        .expect("write ContentProbed Range headers");
    stream
        .write_all(response_body)
        .expect("write ContentProbed Range body");
}

/// Возвращает bounded terminal status без redirect/body ambiguity.
fn respond_to_http_failure(stream: &mut TcpStream) {
    stream
        .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .expect("write ContentProbed HTTP failure");
}

/// Разбирает только single bounded bytes range, который создаёт production provider.
fn parse_range_header(header_line: &str) -> Option<(usize, usize)> {
    let (header_name, header_value) = header_line.split_once(':')?;
    if !header_name.eq_ignore_ascii_case("range") {
        return None;
    }
    let requested_range = header_value.trim().strip_prefix("bytes=")?;
    let (start, end) = requested_range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?))
}

/// Один test работает owner-ом либо isolated child-ом в зависимости от scoped env marker-а.
#[test]
fn http_ogg_opus_null_codecs_reach_production_pcm() {
    if env::var_os(CHILD_PROCESS_MARKER_ENV).is_some() {
        assert_child_content_probed_playback();
        return;
    }

    assert_owner_subprocess_succeeds();
}

/// Typed codec mismatch обязан открыть следующий planner-ranked real resource.
#[test]
fn best_playable_content_rejection_opens_second_real_ogg() {
    if env::var_os(CHILD_PROCESS_MARKER_ENV).is_some() {
        assert_child_content_probe_fallback();
        return;
    }

    assert_owner_content_probe_fallback();
}

/// HTTP open failure остаётся terminal и не маскируется соседним candidate-ом.
#[test]
fn fatal_http_open_does_not_try_second_candidate() {
    if env::var_os(CHILD_PROCESS_MARKER_ENV).is_some() {
        assert_child_fatal_http_open();
        return;
    }

    assert_owner_fatal_http_open();
}

/// Owner держит origin и fake executable живыми, пока isolated child выполняет production path.
fn assert_owner_subprocess_succeeds() {
    // Generated fixture содержит настоящий Ogg framing, OpusHead и decodable silence packet.
    let ogg_fixture = ogg_opus_fixture();
    let origin = RangeFixtureOrigin::spawn(ogg_fixture.bytes);

    // Fake binary существует только в test-owned temporary directory.
    let fake_tools = TempDir::new().expect("create ContentProbed fake-tools directory");
    install_fake_yt_dlp(fake_tools.path());

    // Child-only PATH сохраняет системные executables и не меняет окружение параллельных тестов.
    let extractor_document = yt_dlp_document(&origin.media_url());
    let child_output =
        run_content_probe_child(fake_tools.path(), CHILD_TEST_NAME, extractor_document);
    assert_child_succeeded("single ContentProbed playback", &child_output);
}

/// Owner доказывает оба network attempts вокруг typed content rejection.
fn assert_owner_content_probe_fallback() {
    let ogg_fixture = ogg_opus_fixture();
    let rejected_origin = RangeFixtureOrigin::spawn(ogg_fixture.bytes.clone());
    let playable_origin = RangeFixtureOrigin::spawn(ogg_fixture.bytes);
    let fake_tools = TempDir::new().expect("create fallback fake-tools directory");
    install_fake_yt_dlp(fake_tools.path());
    let extractor_document =
        content_probe_fallback_document(&rejected_origin.media_url(), &playable_origin.media_url());
    let child_output = run_content_probe_child(
        fake_tools.path(),
        FALLBACK_CHILD_TEST_NAME,
        extractor_document,
    );
    assert_child_succeeded("ContentProbed fallback", &child_output);
    assert!(
        rejected_origin.request_count() > 0,
        "первый ranked candidate должен дойти до real HTTP/demux proof"
    );
    assert!(
        playable_origin.request_count() > 0,
        "typed rejection должна открыть второй real HTTP candidate"
    );
}

/// Owner доказывает, что terminal HTTP failure не касается соседнего resource-а.
fn assert_owner_fatal_http_open() {
    let failed_origin = RangeFixtureOrigin::spawn_http_failure();
    let playable_origin = RangeFixtureOrigin::spawn(ogg_opus_fixture().bytes);
    let fake_tools = TempDir::new().expect("create fatal-open fake-tools directory");
    install_fake_yt_dlp(fake_tools.path());
    let extractor_document =
        fatal_open_document(&failed_origin.media_url(), &playable_origin.media_url());
    let child_output =
        run_content_probe_child(fake_tools.path(), FATAL_CHILD_TEST_NAME, extractor_document);
    assert_child_succeeded("terminal ContentProbed open", &child_output);
    assert!(
        failed_origin.request_count() > 0,
        "первый ranked candidate должен выполнить real HTTP attempt"
    );
    assert_eq!(
        playable_origin.request_count(),
        0,
        "fatal HTTP failure не должен запускать соседний candidate"
    );
}

/// Запускает exact child с process-scoped fake extractor document-ом.
fn run_content_probe_child(
    fake_tools_directory: &Path,
    child_test_name: &str,
    extractor_document: String,
) -> std::process::Output {
    Command::new(env::current_exe().expect("current app-egui test binary"))
        .arg("--exact")
        .arg(child_test_name)
        .arg("--nocapture")
        .env(CHILD_PROCESS_MARKER_ENV, "1")
        .env(YT_DLP_DOCUMENT_ENV, extractor_document)
        .env("PATH", path_with_fake_tools_first(fake_tools_directory))
        .output()
        .expect("spawn isolated ContentProbed test child")
}

/// Печатает child output только при failure, сохраняя обычный test log коротким.
fn assert_child_succeeded(scenario: &str, child_output: &std::process::Output) {
    assert!(
        child_output.status.success(),
        "{scenario} child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&child_output.stdout),
        String::from_utf8_lossy(&child_output.stderr)
    );
}

/// Выбирает ровно Ogg/Opus row из общего generated audio-container набора.
fn ogg_opus_fixture() -> audio_fixtures::AudioContainerFixture {
    audio_fixtures::fixtures()
        .into_iter()
        .find(|fixture| fixture.extension == "ogg")
        .expect("generated Ogg/Opus fixture")
}

/// Устанавливает process-compatible `yt-dlp`, печатающий переданный JSON document.
fn install_fake_yt_dlp(fake_tools_directory: &Path) {
    let executable_path = fake_tools_directory.join("yt-dlp");
    let script = concat!(
        "#!/bin/sh\n",
        "set -eu\n",
        "printf '%s\\n' \"${RUSTIPLAYER_CONTENT_PROBE_YTDLP_JSON:?missing fixture JSON}\"\n",
    );
    fs::write(&executable_path, script).expect("write fake yt-dlp executable");
    let mut permissions = fs::metadata(&executable_path)
        .expect("read fake yt-dlp metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable_path, permissions).expect("mark fake yt-dlp executable");
}

/// Строит child PATH без строковой конкатенации и platform separator assumptions.
fn path_with_fake_tools_first(fake_tools_directory: &Path) -> OsString {
    let inherited_path = env::var_os("PATH").unwrap_or_default();
    env::join_paths(
        std::iter::once(fake_tools_directory.to_path_buf())
            .chain(env::split_paths(&inherited_path)),
    )
    .expect("join child-only fake-tools PATH")
}

/// Формирует minimal valid `--dump-single-json` response с неизвестными codec fields.
fn yt_dlp_document(media_url: &str) -> String {
    format!(
        r#"{{"id":"content-probed-proof","title":"ContentProbed proof","formats":[{{"format_id":"content-probed-ogg","url":"{media_url}","protocol":"http","ext":"ogg","container":"ogg","vcodec":null,"acodec":null}}]}}"#
    )
}

/// Первый candidate имеет больший total bitrate, но объявляет AAC для реального Opus track-а.
fn content_probe_fallback_document(rejected_url: &str, playable_url: &str) -> String {
    format!(
        r#"{{"id":"content-probed-fallback","title":"ContentProbed fallback","formats":[{{"format_id":"fallback-mismatch","url":"{rejected_url}","protocol":"http","ext":"ogg","container":"ogg","vcodec":null,"acodec":"aac","preference":0,"language_preference":0,"quality":0,"audio_channels":1,"asr":48000,"tbr":192}},{{"format_id":"fallback-playable","url":"{playable_url}","protocol":"http","ext":"ogg","container":"ogg","vcodec":null,"acodec":null,"preference":0,"language_preference":0,"quality":0,"audio_channels":1,"asr":48000,"tbr":64}}]}}"#
    )
}

/// Оба rows statically playable; только первый real HTTP open является terminal.
fn fatal_open_document(failed_url: &str, unrequested_url: &str) -> String {
    format!(
        r#"{{"id":"content-probed-fatal","title":"ContentProbed fatal","formats":[{{"format_id":"fatal-first","url":"{failed_url}","protocol":"http","ext":"ogg","container":"ogg","vcodec":null,"acodec":null,"preference":0,"language_preference":0,"quality":0,"audio_channels":1,"asr":48000,"tbr":192}},{{"format_id":"must-not-open","url":"{unrequested_url}","protocol":"http","ext":"ogg","container":"ogg","vcodec":null,"acodec":null,"preference":0,"language_preference":0,"quality":0,"audio_channels":1,"asr":48000,"tbr":64}}]}}"#
    )
}

/// Собирает одинаковый production runtime для всех generic ContentProbed scenarios.
fn prepare_content_probed_test_media(
    page_id: &str,
    audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
) -> anyhow::Result<super::PreparedYtDlpWebMedia> {
    let page_url = format!("https://page.example.test/{page_id}");
    let locator = service_ytdlp::parse_yt_dlp_media_locator(&page_url)
        .expect("parse ContentProbed page locator");
    prepare_yt_dlp_web_media(
        &locator,
        &NetworkConfig::default(),
        &YtDlpConfig::default(),
        &PlayerDemuxConfig::default(),
        &[VideoCodec::Vp9],
        &capability_core::SystemCapabilities::empty(1),
        audio_capabilities,
        YtDlpCandidateOpenIntent::BestPlayable,
        CancellationToken::new(),
        || false,
    )
}

/// Child проверяет полный retry path и возвращает PCM только победившего candidate-а.
fn assert_child_content_probe_fallback() {
    let audio_decoder_factory = ProductionAudioDecoderFactory::default();
    let audio_capabilities = audio_decoder_factory.audio_decode_capability_snapshot();
    assert_eq!(
        audio_capabilities
            .query(audio::AudioDecodeCodecFamilyQuery::Known(
                audio::AudioDecodeCodecFamily::Aac,
            ))
            .expect("query production AAC capability"),
        audio::AudioDecodeCapability::Available,
        "fixture declared codec должен пройти static capability admission"
    );
    let mut prepared =
        prepare_content_probed_test_media("content-probed-fallback", audio_capabilities)
            .expect("typed content rejection должна открыть второй Ogg candidate");
    assert_prepared_opus_reaches_pcm(&mut prepared, &audio_decoder_factory, "fallback-playable");
}

/// Child доказывает, что HTTP failure не пересекает typed content-proof boundary.
fn assert_child_fatal_http_open() {
    let audio_decoder_factory = ProductionAudioDecoderFactory::default();
    let open_error = match prepare_content_probed_test_media(
        "content-probed-fatal",
        audio_decoder_factory.audio_decode_capability_snapshot(),
    ) {
        Ok(_) => panic!("terminal HTTP failure не должен открывать соседний candidate"),
        Err(error) => error,
    };
    assert!(
        open_error
            .downcast_ref::<super::content_probe::ContentProbeRejection>()
            .is_none(),
        "transport failure не должен превращаться в retryable ContentProbeRejection"
    );
}

/// Child выполняет настоящий composition path и доказывает его результат до PCM boundary.
fn assert_child_content_probed_playback() {
    let audio_decoder_factory = ProductionAudioDecoderFactory::default();
    let audio_capabilities = audio_decoder_factory.audio_decode_capability_snapshot();
    let mut prepared =
        prepare_content_probed_test_media("content-probed-proof", audio_capabilities)
            .expect("prepare generic ContentProbed HTTP/Ogg candidate");
    assert_prepared_opus_reaches_pcm(&mut prepared, &audio_decoder_factory, "content-probed-ogg");
}

/// Проверяет winning identity, authoritative tracks, packet bytes и production PCM.
fn assert_prepared_opus_reaches_pcm(
    prepared: &mut super::PreparedYtDlpWebMedia,
    audio_decoder_factory: &ProductionAudioDecoderFactory,
    expected_format_id: &str,
) {
    assert_eq!(
        prepared
            .candidate_selection
            .exact_identity()
            .format()
            .as_str(),
        expected_format_id,
        "prepared selection должна принадлежать реально открывшемуся candidate-у"
    );

    // Sidebar model фиксирует именно generic layout, а не ложный audio-only metadata guess.
    assert_eq!(
        prepared.stream_configuration.active_candidate().layout,
        StreamLayoutKind::ContentProbed
    );
    assert_eq!(
        prepared.stream_configuration.active_candidate().audio_codec,
        None,
        "null acodec должен остаться неизвестным до authoritative demux probe"
    );

    // После открытия право назвать codec принадлежит реальному demux track list-у.
    let audio_track = prepared
        .demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .cloned()
        .expect("production demuxer должен обнаружить Ogg audio track");
    assert_eq!(audio_track.codec_id, "A_OPUS");
    assert_eq!(audio_track.sample_rate, Some(48_000));
    assert_eq!(audio_track.channels, Some(1));
    assert!(
        prepared
            .demuxer
            .tracks()
            .iter()
            .all(|track| track.kind != TrackKind::Video),
        "audio-only Ogg не должен создавать выдуманный video track"
    );

    // Production decoder строится только из опубликованного neutral TrackInfo.
    let decoder_config = decoder_config_from_track(&audio_track);
    let mut decoder = audio_decoder_factory
        .create_decoder(decoder_config)
        .expect("create production Opus decoder from probed track");
    let packet = next_selected_audio_packet(prepared.demuxer.as_mut(), audio_track.id);
    let expected_packet = ogg_opus_fixture().first_packet;
    assert_eq!(packet.data.as_ref(), expected_packet.as_slice());

    // Exact production packet mapping должен дать реальные interleaved PCM samples.
    let encoded_packet = EncodedAudioPacket::new(
        packet.track_id.get(),
        audio_packet_timing(&packet),
        &packet.data,
    );
    let decoded_samples = decoder
        .decode(&encoded_packet)
        .expect("decode production Ogg/Opus packet");
    assert!(
        !decoded_samples.is_empty(),
        "production Opus decoder должен вернуть ненулевой PCM buffer"
    );
}

/// Переносит только public demux metadata в codec-neutral decoder config.
fn decoder_config_from_track(audio_track: &TrackInfo) -> AudioDecoderConfig {
    AudioDecoderConfig::from_track_metadata(
        audio_track.id.get(),
        audio_track.codec_id.clone(),
        audio_track.sample_rate,
        audio_track.channels,
    )
    .with_codec_private(
        audio_track
            .codec_private
            .as_ref()
            .map(|codec_private| codec_private.to_vec()),
    )
}

/// Читает lifecycle events до первого packet-а выбранного фактического audio track-а.
fn next_selected_audio_packet(
    demuxer: &mut dyn Demuxer,
    selected_audio_track_id: media_core::TrackId,
) -> Packet {
    let deadline = Instant::now() + DEMUX_EVENT_DEADLINE;
    loop {
        match demuxer
            .next_event()
            .expect("read ContentProbed demux event")
        {
            DemuxReadEvent::Packet(packet)
                if packet.kind == TrackKind::Audio
                    && packet.track_id == selected_audio_track_id =>
            {
                return packet;
            }
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) if Instant::now() < deadline => {
                thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
            }
            DemuxReadEvent::TemporarilyUnavailable(hint) => {
                panic!("ContentProbed demux readiness deadline exceeded: {hint:?}");
            }
            DemuxReadEvent::EndOfStream => {
                panic!("ContentProbed Ogg reached EOF before selected audio packet");
            }
        }
    }
}

/// Сохраняет исходную packet time base без догадок по sample rate.
fn audio_packet_timing(packet: &Packet) -> AudioPacketTiming {
    let Some(track_pts) = packet.track_pts else {
        return AudioPacketTiming::unknown();
    };
    let Some(time_base) =
        AudioPacketTimeBase::new(track_pts.time_base.numer, track_pts.time_base.denom)
    else {
        return AudioPacketTiming::unknown();
    };
    AudioPacketTiming::from_track_units(
        time_base,
        track_pts.units.get(),
        packet.track_dts.map(|track_dts| track_dts.units.get()),
        packet
            .track_duration
            .map(|track_duration| track_duration.units.get()),
    )
}
