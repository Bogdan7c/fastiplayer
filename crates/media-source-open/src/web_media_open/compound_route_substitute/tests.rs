//! Вертикальная регрессия YouTube `616+251`: compound «HLS video + HTTPS audio».
//!
//! Подменены только внешний `yt-dlp` (скрипт печатает synthetic документ той же формы,
//! что реальный ответ для `0e3GPea1Tyg`) и loopback origin с настоящими WebM байтами.
//! Normalization, planner, BestPlayable fallback, progressive HTTP transport,
//! Symphonia demux и composite A/V demuxer проходят production boundaries.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command};
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
    BitDepth, ChromaSubsampling, DecodeBackendId, SupportedVideoDecodeFormat,
    VideoCodec as DecodeVideoCodec, VideoProfile, Vp9Profile,
};
use fastiplayer_config::{
    NetworkConfig, PlayerDemuxConfig, VideoCodec as ConfigVideoCodec, WebMediaConfig, YtDlpConfig,
};
use media_core::{DemuxReadEvent, Demuxer, TrackKind};
use service_ytdlp::{ExtractorProcessInvocation, ExtractorProcessLauncher, YtDlpExtractorAdapter};
use source_core::CancellationToken;
use tempfile::TempDir;
use video_frame_contract::VideoFrameContract;
use web_media_core::{StreamLayout, TransportFamily};

use super::super::{YtDlpCandidateOpenIntent, prepare_yt_dlp_web_media};

/// Настоящий VP9 video-only WebM (выделен ffmpeg `-c copy` из muxed test fixture).
const VP9_VIDEO_ONLY_WEBM: &[u8] = include_bytes!("fixtures/vp9-video-only.webm");
/// Настоящий Opus audio-only WebM из того же muxed fixture.
const OPUS_AUDIO_ONLY_WEBM: &[u8] = include_bytes!("fixtures/opus-audio-only.webm");
/// Progressive video-only путь, на который должна уйти замена.
const PROGRESSIVE_VIDEO_PATH: &str = "/video-248.webm";
/// Audio-only путь, выбранный yt-dlp в compound-е (оригинальный английский).
const COMPOUND_AUDIO_PATH: &str = "/audio-251-en.webm";
/// Progressive 720p строка: auto-ranking ставит её выше, но замена держит высоту 1080p.
const LOWER_PROGRESSIVE_VIDEO_PATH: &str = "/video-247.webm";
/// Соседний dub, который замена не должна подхватить.
const DUB_AUDIO_PATH: &str = "/audio-251-de.webm";
/// Префикс HLS ресурсов format 616: открывать их замена не должна.
const HLS_PATH_PREFIX: &str = "/hls/";
/// Общий bounded deadline сети и demux readiness.
const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Loopback origin с фиксированной таблицей ресурсов и журналом путей запросов.
struct LoopbackMediaOrigin {
    /// Адрес нужен документу yt-dlp и wake-up соединению при остановке.
    address: SocketAddr,
    /// Флаг остановки accept loop-а.
    stop_requested: Arc<AtomicBool>,
    /// Пути всех запросов (без query) для проверки, что HLS не открывался.
    requested_paths: Arc<Mutex<Vec<String>>>,
    /// Worker завершается в `Drop`.
    worker: Option<JoinHandle<()>>,
}

impl LoopbackMediaOrigin {
    /// Поднимает origin: два progressive ресурса, всё остальное — `404`.
    fn spawn() -> Self {
        let routes: Arc<HashMap<&'static str, &'static [u8]>> = Arc::new(HashMap::from([
            (PROGRESSIVE_VIDEO_PATH, VP9_VIDEO_ONLY_WEBM),
            (COMPOUND_AUDIO_PATH, OPUS_AUDIO_ONLY_WEBM),
        ]));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback media origin");
        let address = listener.local_addr().expect("loopback origin address");
        let stop_requested = Arc::new(AtomicBool::new(false));
        let requested_paths = Arc::new(Mutex::new(Vec::new()));
        let worker = {
            let stop_requested = Arc::clone(&stop_requested);
            let requested_paths = Arc::clone(&requested_paths);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop_requested.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut stream) = stream else { continue };
                    serve_request(&mut stream, &routes, &requested_paths);
                }
            })
        };
        Self {
            address,
            stop_requested,
            requested_paths,
            worker: Some(worker),
        }
    }

    /// Абсолютный URL ресурса origin-а.
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    /// Снимок путей всех запросов.
    fn requested_paths(&self) -> Vec<String> {
        self.requested_paths
            .lock()
            .expect("requested paths lock")
            .clone()
    }
}

impl Drop for LoopbackMediaOrigin {
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        // Wake-up соединение выводит blocking accept из ожидания.
        let _wake_up = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("loopback origin worker joins");
        }
    }
}

/// Обслуживает один HTTP/1.1 запрос: bounded Range для известных путей, иначе `404`.
fn serve_request(
    stream: &mut TcpStream,
    routes: &HashMap<&'static str, &'static [u8]>,
    requested_paths: &Mutex<Vec<String>>,
) {
    let Some(request) = read_request_head(stream) else {
        return;
    };
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default()
        .to_owned();
    requested_paths
        .lock()
        .expect("requested paths lock")
        .push(path.clone());
    let response = match routes.get(path.as_str()) {
        Some(body) => range_response(&request, body),
        None => {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        }
    };
    // Клиент может закрыть соединение раньше (probe); для fixture это не ошибка теста.
    let _write_outcome = stream.write_all(&response);
}

/// Читает header block запроса; `None` — пустое wake-up соединение.
fn read_request_head(stream: &mut TcpStream) -> Option<String> {
    stream
        .set_read_timeout(Some(TEST_TIMEOUT))
        .expect("set origin read timeout");
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1_024];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    (!request.is_empty()).then(|| String::from_utf8_lossy(&request).into_owned())
}

/// Отдаёт `206` на `Range: bytes=a-b` либо `200` на весь ресурс.
fn range_response(request: &str, body: &[u8]) -> Vec<u8> {
    let requested_range = request.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("range") {
            return None;
        }
        let (start, end) = value.trim().strip_prefix("bytes=")?.split_once('-')?;
        let start: usize = start.parse().ok()?;
        let end: usize = end.parse().unwrap_or(usize::MAX);
        Some((start, end))
    });
    let (status, start, end) = match requested_range {
        Some((start, end)) if start < body.len() => {
            ("206 Partial Content", start, end.min(body.len() - 1))
        }
        _ => ("200 OK", 0, body.len() - 1),
    };
    let slice = &body[start..=end];
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: video/webm\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
        slice.len(),
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(slice);
    response
}

/// Запускает production `Command` с каталогом подставного `yt-dlp` первым в PATH.
struct ScriptedYtDlpLauncher {
    /// Каталог с исполняемым скриптом `yt-dlp`.
    executable_directory: PathBuf,
}

impl ExtractorProcessLauncher for ScriptedYtDlpLauncher {
    fn spawn(
        &self,
        command: &mut Command,
        _invocation: ExtractorProcessInvocation,
    ) -> io::Result<Child> {
        let mut command_path = OsString::from(&self.executable_directory);
        if let Some(system_path) = std::env::var_os("PATH") {
            command_path.push(":");
            command_path.push(system_path);
        }
        command.env("PATH", command_path);
        command.spawn()
    }
}

/// Пишет исполняемый `yt-dlp`, печатающий документ без интерпретации аргументов.
fn install_fake_yt_dlp(directory: &TempDir, document: &str) {
    let executable = directory.path().join("yt-dlp");
    let script = format!("#!/bin/sh\ncat <<'FASTIPLAYER_JSON'\n{document}\nFASTIPLAYER_JSON\n");
    fs::write(&executable, script).expect("write fake yt-dlp");
    let mut permissions = fs::metadata(&executable)
        .expect("fake yt-dlp metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable, permissions).expect("make fake yt-dlp executable");
}

/// Synthetic документ формы `0e3GPea1Tyg`: compound `616+251-en` и progressive альтернатива.
fn mixed_transport_compound_document(origin: &LoopbackMediaOrigin) -> String {
    let hls_video = format!(
        r#"{{"format_id":"616","url":"{}","manifest_url":"{}","protocol":"m3u8_native","ext":"mp4","vcodec":"vp09.00.40.08","acodec":"none","width":1920,"height":1080,"fps":25,"tbr":4700,"dynamic_range":"SDR"}}"#,
        origin.url(&format!("{HLS_PATH_PREFIX}616/index.m3u8")),
        origin.url(&format!("{HLS_PATH_PREFIX}master.m3u8")),
    );
    let progressive_video = format!(
        r#"{{"format_id":"248","url":"{}","protocol":"http","ext":"webm","container":"webm_dash","vcodec":"vp9","acodec":"none","width":1920,"height":1080,"fps":25,"tbr":1800,"dynamic_range":"SDR"}}"#,
        origin.url(PROGRESSIVE_VIDEO_PATH),
    );
    let lower_progressive_video = format!(
        r#"{{"format_id":"247","url":"{}","protocol":"http","ext":"webm","container":"webm_dash","vcodec":"vp9","acodec":"none","width":1280,"height":720,"fps":25,"tbr":1100,"dynamic_range":"SDR"}}"#,
        origin.url(LOWER_PROGRESSIVE_VIDEO_PATH),
    );
    let compound_audio = format!(
        r#"{{"format_id":"251-1","url":"{}","protocol":"http","ext":"webm","container":"webm_dash","vcodec":"none","acodec":"opus","abr":125,"language":"en-US","language_preference":10}}"#,
        origin.url(COMPOUND_AUDIO_PATH),
    );
    let dub_audio = format!(
        r#"{{"format_id":"251-0","url":"{}","protocol":"http","ext":"webm","container":"webm_dash","vcodec":"none","acodec":"opus","abr":126,"language":"de","language_preference":-1}}"#,
        origin.url(DUB_AUDIO_PATH),
    );
    format!(
        r#"{{"id":"route-616","title":"Mixed route fixture","duration":0.4,"is_live":false,"format_id":"616+251-1","protocol":"m3u8_native+http","requested_formats":[{hls_video},{compound_audio}],"formats":[{lower_progressive_video},{progressive_video},{hls_video},{dub_audio},{compound_audio}]}}"#
    )
}

/// Capability report с одним VP9 Profile 0 output-ом.
fn vp9_system_capabilities() -> SystemCapabilities {
    let backend_id = DecodeBackendId::new("fixture_v").expect("valid fixture backend id");
    let output = SupportedVideoOutput {
        backend: backend_id.clone(),
        decode_format: SupportedVideoDecodeFormat {
            codec: DecodeVideoCodec::Vp9,
            profile: VideoProfile::Vp9(Vp9Profile::Profile0),
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
            display_name: "Fixture VP9 backend".to_owned(),
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

/// Poll-ит nonblocking demux до готового события в пределах deadline-а.
fn next_ready_event(demuxer: &mut dyn Demuxer) -> DemuxReadEvent {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match demuxer.next_event().expect("composite demux event") {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(Instant::now() < deadline, "composite demux timed out");
                thread::sleep(Duration::from_millis(2));
            }
            event => return event,
        }
    }
}

/// Собранный demuxer отдаёт и VP9 video, и Opus audio packets (A/V, а не немое видео).
fn assert_video_and_audio_packets(demuxer: &mut dyn Demuxer) {
    assert!(
        demuxer
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Video),
        "composite must publish the video track"
    );
    assert!(
        demuxer
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Audio),
        "composite must publish the audio track"
    );
    let mut video_packet_seen = false;
    let mut audio_packet_seen = false;
    for _ in 0..256 {
        match next_ready_event(demuxer) {
            DemuxReadEvent::Packet(packet) => match packet.kind {
                TrackKind::Video => video_packet_seen = true,
                TrackKind::Audio => audio_packet_seen = true,
                TrackKind::Subtitle => {}
            },
            DemuxReadEvent::EndOfStream => break,
            _ => {}
        }
        if video_packet_seen && audio_packet_seen {
            break;
        }
    }
    assert!(
        video_packet_seen,
        "progressive VP9 video packet must arrive"
    );
    assert!(audio_packet_seen, "compound Opus audio packet must arrive");
}

/// BestPlayable открывает progressive video + звук compound-а вместо terminal ошибки.
#[test]
fn mixed_hls_https_compound_opens_route_compatible_video_with_compound_audio() {
    let origin = LoopbackMediaOrigin::spawn();
    let fake_tools = TempDir::new().expect("fake yt-dlp directory");
    install_fake_yt_dlp(&fake_tools, &mixed_transport_compound_document(&origin));
    let adapter = YtDlpExtractorAdapter::with_process_launcher(Arc::new(ScriptedYtDlpLauncher {
        executable_directory: fake_tools.path().to_path_buf(),
    }));
    let locator = service_ytdlp::parse_yt_dlp_media_locator("https://video.example.test/watch/616")
        .expect("parse page locator");
    let audio_decoder_factory = ProductionAudioDecoderFactory::default();

    let mut prepared = prepare_yt_dlp_web_media(
        &locator,
        &NetworkConfig::default(),
        &WebMediaConfig::default(),
        &YtDlpConfig {
            resolve_timeout_ms: 5_000,
            ..YtDlpConfig::default()
        },
        &adapter,
        &PlayerDemuxConfig::default(),
        &[ConfigVideoCodec::Vp9],
        &vp9_system_capabilities(),
        audio_decoder_factory.audio_decode_capability_snapshot(),
        YtDlpCandidateOpenIntent::BestPlayable,
        web_media_core::ExtractorInvocationReason::PageMediaResolution,
        CancellationToken::new(),
        || false,
    )
    .expect("mixed-transport compound must open through its route-compatible substitute");

    // Открылась composed пара: progressive video + тот же английский звук compound-а.
    let composed = prepared
        .source_state
        .composed_selection
        .as_deref()
        .expect("substitute is installed as a composed selection");
    let StreamLayout::Separate { video, audio } = composed.descriptor().layout() else {
        panic!("substitute must be a separate A/V pair");
    };
    assert!(matches!(
        video.transport().family(),
        TransportFamily::ProgressiveHttp(_)
    ));
    // Высота compound-а (1080p) сохранена, хотя auto-ranking ставит 720p выше.
    assert_eq!(
        video.video().height().map(|height| height.pixels()),
        Some(1080)
    );
    assert_eq!(
        audio.audio().language().map(|tag| tag.as_str()),
        Some("en-US")
    );
    // Active selection — video parent composed пары (как у ручного выбора в UI).
    assert_eq!(
        prepared.source_state.candidate_selection(),
        composed.video_parent_selection()
    );

    assert_video_and_audio_packets(prepared.demuxer.as_mut());

    let requested_paths = origin.requested_paths();
    assert!(
        requested_paths
            .iter()
            .any(|path| path == PROGRESSIVE_VIDEO_PATH),
        "progressive video must be fetched: {requested_paths:?}"
    );
    assert!(
        requested_paths
            .iter()
            .any(|path| path == COMPOUND_AUDIO_PATH),
        "compound audio must be fetched: {requested_paths:?}"
    );
    assert!(
        !requested_paths
            .iter()
            .any(|path| path.starts_with(HLS_PATH_PREFIX)
                || path == DUB_AUDIO_PATH
                || path == LOWER_PROGRESSIVE_VIDEO_PATH),
        "neither HLS 616, the 720p row nor the dub may be opened: {requested_paths:?}"
    );
}
