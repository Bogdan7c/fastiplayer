//! N07 vertical: native HLS master TS/fMP4 без extractor process-а.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use audio::decoder::EncodedAudioPacket;
use audio::{AudioDecodeCapabilityProvider, AudioDecoderFactory, ProductionAudioDecoderFactory};
use base64::Engine as _;
use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, CURRENT_CAPABILITY_SCHEMA_VERSION,
    SupportedVideoOutput, SystemCapabilities,
};
use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, H264Profile, SupportedVideoDecodeFormat,
    VideoCodec as DecodeVideoCodec, VideoProfile,
};
use media_core::{DemuxReadEvent, DemuxRetryHint, Demuxer, MediaTime, TrackKind};
use player_core::PreparedInitialPosition;
use render_wgpu_video::HostPlanarWgpuFrameMaterializer;
use rustiplayer_config::{AppConfig, VideoCodec};
use service_ytdlp::YtDlpExtractorAdapter;
use source_core::CancellationToken;
use video_frame_contract::VideoFrameContract;
use web_media_hls::HlsVodStartIntent;

use super::super::*;
use crate::media_open::{NativeHlsOpenIntent, NativeHlsSourceState, NativeHlsUrl, SafeMediaLabel};
use crate::startup_media::native_hls::{
    NativeHlsAdmissionPort, NativeHlsAttempt, NativeHlsPreparationRequest, PreparedNativeHlsMedia,
    ProductionNativeHlsAdmissionPort,
};
use crate::web_media_open::content_probe_tests::direct_progressive::ZeroProcessSpy;
use crate::web_media_open::content_probe_tests::direct_progressive_webm::{
    OffscreenWgpuHarness, decode_packet, drain_decoder, open_decoder,
};
use crate::web_media_open::content_probe_tests::{audio_packet_timing, decoder_config_from_track};
use crate::web_media_stream_model::component_variants::{
    ComponentVariantActionResolution, ComponentVariantSelectionAction,
    WebMediaComponentVariantAxisKind, WebMediaComponentVariantProjection,
    WebMediaInstalledComponentVariantPresentation,
};

/// Decoder/demux readiness не должен зависеть от бесконечного polling-а.
const VERTICAL_DEADLINE: Duration = Duration::from_secs(10);
/// Switch/reopen запрашивают позицию внутри первого segment-а и приземляются на post-target RAP.
const RESTORE_POSITION: Duration = Duration::from_millis(100);

/// Один обслуженный request нужен только для точного root-fetch accounting-а.
#[derive(Debug, Clone)]
struct ServedRequest {
    path: String,
}

/// Локальный immutable HTTP origin обслуживает реальные production transport запросы.
pub(super) struct ControlledHlsServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<ServedRequest>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl ControlledHlsServer {
    /// Запускает единственный blocking accept worker над фиксированным route snapshot-ом.
    pub(super) fn start(routes: HashMap<String, Vec<Vec<u8>>>) -> Self {
        Self::start_with_initial_failures(routes, HashMap::new())
    }

    /// Позволяет live vertical-у доказать endpoint recovery через bounded initial 410 budget.
    pub(super) fn start_with_initial_failures(
        routes: HashMap<String, Vec<Vec<u8>>>,
        initial_failure_budgets: HashMap<String, usize>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind N07 HLS fixture origin");
        let address = listener.local_addr().expect("N07 HLS fixture address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let requests = Arc::new(Mutex::new(Vec::<ServedRequest>::new()));
        let worker_requests = Arc::clone(&requests);
        let routes = Arc::new(routes);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                let (mut stream, _) = listener.accept().expect("accept N07 HLS request");
                if worker_stop.load(Ordering::Acquire) {
                    break;
                }
                let path = read_request_path(&mut stream);
                let mut request_log = worker_requests.lock().expect("N07 request log");
                let route_request_index = request_log
                    .iter()
                    .filter(|request| request.path == path)
                    .count();
                let initial_failure_budget =
                    initial_failure_budgets.get(&path).copied().unwrap_or(0);
                let (status, body) = if route_request_index < initial_failure_budget {
                    ("410 Gone", &[][..])
                } else {
                    let successful_request_index = route_request_index - initial_failure_budget;
                    routes
                        .get(&path)
                        .and_then(|responses| {
                            responses
                                .get(successful_request_index)
                                .or_else(|| responses.last())
                        })
                        .map_or(("404 Not Found", &[][..]), |body| {
                            ("200 OK", body.as_slice())
                        })
                };
                request_log.push(ServedRequest { path });
                drop(request_log);
                stream
                    .write_all(&http_response(status, body))
                    .expect("write N07 HLS response");
            }
        });
        Self {
            address,
            stop,
            requests,
            worker: Some(worker),
        }
    }

    /// Строит exact HTTP target локального route-а.
    pub(super) fn target(&self, path: &str) -> source_core::HttpRequestTarget {
        source_core::HttpRequestTarget::parse_exact(format!("http://{}{path}", self.address))
            .expect("валидный N07 HTTP target")
    }

    /// Возвращает точное число GET-ов выбранного route-а.
    pub(super) fn request_count(&self, path: &str) -> usize {
        self.requests
            .lock()
            .expect("N07 request log")
            .iter()
            .filter(|request| request.path == path)
            .count()
    }
}

impl Drop for ControlledHlsServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join N07 HLS fixture origin");
        }
    }
}

/// Читает только request line/header boundary; request body у HLS GET отсутствует.
fn read_request_path(stream: &mut TcpStream) -> String {
    let mut request_bytes = Vec::new();
    let mut chunk = [0_u8; 1_024];
    loop {
        let read = stream.read(&mut chunk).expect("read N07 HLS request");
        assert!(read > 0, "N07 HTTP request оборвался до конца headers");
        request_bytes.extend_from_slice(&chunk[..read]);
        if request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(request_bytes)
        .expect("N07 request обязан быть UTF-8 HTTP")
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("N07 request path")
        .to_owned()
}

/// Формирует bounded HTTP/1.1 response без chunking и keep-alive состояния.
fn http_response(status: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

/// Master намеренно содержит обе обязательные container формы одного neutral catalog-а.
fn master_manifest(fmp4_first: bool) -> Vec<u8> {
    let ts_row = concat!(
        "#EXT-X-STREAM-INF:BANDWIDTH=100000,RESOLUTION=16x16,CODECS=\"avc1.42c00a,mp4a.40.2\"\n",
        "ts.m3u8\n",
    );
    let fmp4_row = concat!(
        "#EXT-X-STREAM-INF:BANDWIDTH=200000,RESOLUTION=16x16,CODECS=\"avc1.42c00a,mp4a.40.2\"\n",
        "fmp4.m3u8\n",
    );
    format!(
        "#EXTM3U\n{}{}",
        if fmp4_first { fmp4_row } else { ts_row },
        if fmp4_first { ts_row } else { fmp4_row },
    )
    .into_bytes()
}

/// Декодирует repository fixture; никакой внешний генератор в test runtime не вызывается.
pub(super) fn decode_fixture(encoded: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(encoded.split_whitespace().collect::<String>())
        .expect("N07 fixture должен быть валидным base64")
}

/// Собирает immutable loopback routes для root, TS и fMP4 вариантов.
fn fixture_routes() -> HashMap<String, Vec<Vec<u8>>> {
    HashMap::from([
        (
            "/master.m3u8".to_owned(),
            vec![
                master_manifest(false),
                master_manifest(true),
                master_manifest(false),
            ],
        ),
        (
            "/ts.m3u8".to_owned(),
            vec![include_bytes!("fixtures/ts.m3u8").to_vec()],
        ),
        (
            "/ts-0.ts".to_owned(),
            vec![decode_fixture(include_str!("fixtures/ts-0.ts.base64"))],
        ),
        (
            "/ts-1.ts".to_owned(),
            vec![decode_fixture(include_str!("fixtures/ts-1.ts.base64"))],
        ),
        (
            "/fmp4.m3u8".to_owned(),
            vec![include_bytes!("fixtures/fmp4.m3u8").to_vec()],
        ),
        (
            "/fmp4-init.mp4".to_owned(),
            vec![decode_fixture(include_str!(
                "fixtures/fmp4-init.mp4.base64"
            ))],
        ),
        (
            "/fmp4-0.m4s".to_owned(),
            vec![decode_fixture(include_str!("fixtures/fmp4-0.m4s.base64"))],
        ),
        (
            "/fmp4-1.m4s".to_owned(),
            vec![decode_fixture(include_str!("fixtures/fmp4-1.m4s.base64"))],
        ),
    ])
}

/// Capability snapshot описывает ровно тот H.264 baseline contract, который декодирует fixture.
fn h264_system_capabilities() -> SystemCapabilities {
    let backend_id = DecodeBackendId::new("nseven_h").expect("валидный backend ID");
    let output = SupportedVideoOutput {
        backend: backend_id.clone(),
        decode_format: SupportedVideoDecodeFormat {
            codec: DecodeVideoCodec::H264,
            profile: VideoProfile::H264(H264Profile::Baseline),
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            max_width: Some(16),
            max_height: Some(16),
            max_fps: Some(10.0),
            hdr_input: false,
        },
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
    };
    SystemCapabilities {
        schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![BackendCapabilities {
            backend_id,
            display_name: "N07 software H.264 fixture backend".to_owned(),
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

/// Settings используют production AAC capability probe и запрещают extractor fallback policy.
pub(super) fn native_settings() -> WebMediaOpenSettings {
    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    app_config.player.preferred_video_codec_order = vec![VideoCodec::H264];
    let audio_capabilities =
        ProductionAudioDecoderFactory::default().audio_decode_capability_snapshot();
    WebMediaOpenSettings::from_app_config(
        &app_config,
        &h264_system_capabilities(),
        audio_capabilities,
    )
}

/// Вызывает production native admission с явным semantic rematch и start intent-ом.
pub(super) fn prepare_native(
    source: &NativeHlsUrl,
    expected_selection: Option<&web_media_core::WebMediaSemanticSelectionRequest>,
    settings: &WebMediaOpenSettings,
    start: HlsVodStartIntent,
) -> PreparedNativeHlsMedia {
    let mut port = ProductionNativeHlsAdmissionPort::new(NativeHlsPreparationRequest {
        source,
        expected_selection,
        network_config: &settings.network_config,
        web_media_config: &settings.web_media_config,
        demux_config: &settings.demux_config,
        preferred_video_codec_order: &settings.preferred_video_codec_order,
        system_capabilities: &settings.system_capabilities,
        audio_capabilities: settings.audio_capabilities,
        start,
        cancellation: CancellationToken::new(),
    });
    match NativeHlsAdmissionPort::prepare(&mut port).expect("native HLS admission должен пройти")
    {
        NativeHlsAttempt::Prepared(prepared) => prepared,
        NativeHlsAttempt::RequiresExtractorFallback(trigger) => {
            panic!("валидный N07 HLS VOD не имеет права требовать extractor: {trigger:?}")
        }
    }
}

/// Проверяет минимальный consumer path: H.264 packet -> frame -> WGPU и AAC packet -> PCM.
pub(super) fn assert_decoder_render_audio(
    demuxer: &mut dyn Demuxer,
    wgpu_harness: &mut OffscreenWgpuHarness,
) {
    assert_decoder_render_audio_for_codec(demuxer, wgpu_harness, DecodeVideoCodec::H264);
}

/// Проверяет общий adaptive consumer path для явно доказанного video codec-а и actual audio track-а.
pub(super) fn assert_decoder_render_audio_for_codec(
    demuxer: &mut dyn Demuxer,
    wgpu_harness: &mut OffscreenWgpuHarness,
    video_codec: DecodeVideoCodec,
) {
    assert_decoder_render_audio_samples(demuxer, wgpu_harness, video_codec, 1);
}

/// Live vertical требует как минимум два successive frame/audio результата одним decoder lifecycle.
pub(super) fn assert_decoder_render_audio_movement(
    demuxer: &mut dyn Demuxer,
    wgpu_harness: &mut OffscreenWgpuHarness,
) {
    assert_decoder_render_audio_samples(demuxer, wgpu_harness, DecodeVideoCodec::H264, 2);
}

/// Один decoder lifecycle сохраняет SPS/PPS и доказывает движение, не переоткрываясь mid-stream.
fn assert_decoder_render_audio_samples(
    demuxer: &mut dyn Demuxer,
    wgpu_harness: &mut OffscreenWgpuHarness,
    video_codec: DecodeVideoCodec,
    minimum_samples: usize,
) {
    let video_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .cloned()
        .expect("native HLS variant должен публиковать video track");
    let audio_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .cloned()
        .expect("native HLS variant должен публиковать audio track");
    let (video_decoder, renderer_provider) =
        open_decoder(&video_track, wgpu_harness.queue(), video_codec);
    let materializer = HostPlanarWgpuFrameMaterializer::new(
        wgpu_harness.device(),
        wgpu_harness.queue(),
        renderer_provider.clone(),
    );
    let audio_factory = ProductionAudioDecoderFactory::default();
    let mut audio_decoder = audio_factory
        .create_decoder(decoder_config_from_track(&audio_track))
        .expect("production AAC decoder должен принять native HLS track");
    let deadline = Instant::now() + VERTICAL_DEADLINE;
    let mut decoded_video_frames = Vec::new();
    let mut decoded_audio_batches = 0_usize;

    while decoded_video_frames.len() < minimum_samples || decoded_audio_batches < minimum_samples {
        match demuxer.next_event().expect("читать native HLS demux event") {
            DemuxReadEvent::Packet(packet) if packet.track_id == video_track.id => {
                for decoded_frame in decode_packet(video_decoder.as_ref(), packet) {
                    if decoded_video_frames.len() < minimum_samples {
                        decoded_video_frames.push(decoded_frame);
                    } else {
                        video_decoder.release_frame(decoded_frame.resource_handle);
                    }
                }
            }
            DemuxReadEvent::Packet(packet) if packet.track_id == audio_track.id => {
                let encoded_packet = EncodedAudioPacket::new(
                    packet.track_id.get(),
                    audio_packet_timing(&packet),
                    &packet.data,
                );
                if !audio_decoder
                    .decode(&encoded_packet)
                    .expect("production AAC decoder должен декодировать native HLS packet")
                    .is_empty()
                {
                    decoded_audio_batches += 1;
                }
            }
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) if Instant::now() < deadline => {
                thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
            }
            DemuxReadEvent::TemporarilyUnavailable(hint) => {
                panic!("native HLS vertical превысил readiness deadline: {hint:?}")
            }
            DemuxReadEvent::EndOfStream => {
                if decoded_video_frames.len() < minimum_samples {
                    decoded_video_frames.push(drain_decoder(video_decoder.as_ref()));
                }
                assert!(
                    decoded_audio_batches >= minimum_samples,
                    "native HLS достиг EOS до требуемого числа AAC PCM batches"
                );
            }
        }
        assert!(
            Instant::now() < deadline,
            "native HLS decoder/render/audio vertical timeout"
        );
    }

    assert_eq!(decoded_video_frames.len(), minimum_samples);
    for decoded_video_frame in decoded_video_frames {
        assert!(wgpu_harness.submit_and_release(
            &materializer,
            &renderer_provider,
            decoded_video_frame,
        ));
    }
}

/// Возвращает active row и semantic request второй coupled rendition.
pub(super) fn alternate_component_selection(
    source_state: &NativeHlsSourceState,
) -> (
    usize,
    usize,
    web_media_core::ComponentVariantSemanticSelectionRequest,
) {
    let stream_configuration = source_state.stream_configuration();
    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::Coupled {
            catalog_generation,
            coupled,
        },
    ) = stream_configuration.component_variant_projection()
    else {
        panic!("native HLS master должен публиковать coupled TS/fMP4 catalog");
    };
    assert_eq!(
        coupled.variants.len(),
        2,
        "catalog обязан содержать обе строки"
    );
    let alternate_index = usize::from(coupled.active_index == 0);
    let resolution = stream_configuration
        .resolve_component_variant_action(ComponentVariantSelectionAction {
            parent_generation: stream_configuration.generation(),
            catalog_generation,
            axis: WebMediaComponentVariantAxisKind::Coupled,
            variant_index: alternate_index,
        })
        .expect("fresh component action должен разрешиться");
    let ComponentVariantActionResolution::SemanticReopen(selection) = resolution else {
        panic!("alternate row не должна разрешаться как NoChange");
    };
    (coupled.active_index, alternate_index, selection)
}

/// Из neutral request извлекает только native semantic selection, без extractor material.
pub(super) fn native_request_parts(
    request: WebMediaOpenRequest,
) -> (
    NativeHlsUrl,
    web_media_core::WebMediaSemanticSelectionRequest,
    WebMediaOpenSettings,
) {
    let WebMediaOpenAdapterView::NativeHls {
        source,
        intent: NativeHlsOpenIntent::SemanticSelection(selection),
        settings,
    } = request.into_adapter()
    else {
        panic!("native action обязан остаться native semantic request-ом");
    };
    (source, selection, settings)
}

/// Доказывает initial fMP4, exact switch на TS, receipted seek и semantic reopen end-to-end.
#[test]
fn native_hls_master_ts_fmp4_switch_seek_reopen_reaches_consumers_without_extractor() {
    let server = ControlledHlsServer::start(fixture_routes());
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let _extractor_adapter = YtDlpExtractorAdapter::with_process_launcher(process_spy.clone());
    let settings = native_settings();
    let source = NativeHlsUrl::new(
        server.target("/master.m3u8"),
        SafeMediaLabel::from_service_safe_label("controlled native HLS master"),
    );
    let stable_source_identity = source.source_identity();
    let mut wgpu_harness = OffscreenWgpuHarness::new();

    let mut initial = prepare_native(&source, None, &settings, HlsVodStartIntent::Beginning);
    assert_eq!(server.request_count("/master.m3u8"), 1);
    assert_decoder_render_audio(initial.demuxer.as_mut(), &mut wgpu_harness);
    let (initial_index, alternate_index, alternate_selection) =
        alternate_component_selection(&initial.source_state);
    assert_eq!(
        initial_index, 1,
        "higher-bandwidth fMP4 row должна быть provider default"
    );
    assert_eq!(alternate_index, 0, "switch должен выбрать TS row");
    let expected_ts_selection = alternate_selection.clone();
    let initial_intent = WebMediaSourceIntent::native_hls(
        source.clone(),
        web_media_core::WebMediaPresentationKind::Vod,
        initial.source_state,
    );
    assert_eq!(
        initial_intent.recovery(),
        web_media_core::WebMediaRecoveryStrategy::RefreshRootManifestAndRematch
    );
    let WebMediaSelectionSwitchResolution::Ready(switch_request) = initial_intent
        .selection_switch_request(
            WebMediaSelectionSwitchIntent::ComponentSemantic(alternate_selection),
            settings.clone(),
        )
    else {
        panic!("fresh native component action обязан запустить exact same-item switch");
    };
    let (switch_source, switch_selection, switch_settings) = native_request_parts(switch_request);
    let mut switched = prepare_native(
        &switch_source,
        Some(&switch_selection),
        &switch_settings,
        HlsVodStartIntent::RestoreOrBeginning(MediaTime::from_duration(RESTORE_POSITION)),
    );
    assert_eq!(server.request_count("/master.m3u8"), 2);
    assert!(
        matches!(
            switched.vod_initial_position(),
            Some(PreparedInitialPosition::PositionedAt { .. })
        ),
        "switch должен вернуть authoritative seek receipt: {:?}",
        switched.vod_initial_position()
    );
    assert_decoder_render_audio(switched.demuxer.as_mut(), &mut wgpu_harness);
    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::Coupled { coupled, .. },
    ) = switched
        .source_state
        .stream_configuration()
        .component_variant_projection()
    else {
        panic!("switched source должен сохранить coupled catalog");
    };
    assert_eq!(coupled.variants.len(), 2);
    let web_media_core::WebMediaSelectionShape::Components(switched_components) =
        switched.source_state.neutral_selection().shape()
    else {
        panic!("switched native selection должна сохранить component shape");
    };
    assert_eq!(
        switched_components.semantic_rematch_request(),
        expected_ts_selection,
        "switch должен semantic-rematch TS после перестановки master rows"
    );
    assert_eq!(
        switched
            .source_state
            .neutral_selection()
            .parent()
            .exact()
            .source(),
        stable_source_identity,
        "switch обязан сохранить stable source lineage"
    );

    let switched_intent = WebMediaSourceIntent::native_hls(
        switch_source.clone(),
        web_media_core::WebMediaPresentationKind::Vod,
        switched.source_state,
    );
    let reopen_request = switched_intent
        .controlled_reopen_request(
            switch_settings.network_config.clone(),
            switch_settings.demux_config,
            Some(switch_settings.clone()),
        )
        .expect("native controlled reopen требует semantic rematch");
    let (reopen_source, reopen_selection, reopen_settings) = native_request_parts(reopen_request);
    let mut reopened = prepare_native(
        &reopen_source,
        Some(&reopen_selection),
        &reopen_settings,
        HlsVodStartIntent::RestoreOrBeginning(MediaTime::from_duration(RESTORE_POSITION)),
    );
    assert_eq!(server.request_count("/master.m3u8"), 3);
    assert!(
        matches!(
            reopened.vod_initial_position(),
            Some(PreparedInitialPosition::PositionedAt { .. })
        ),
        "reopen должен вернуть authoritative seek receipt: {:?}",
        reopened.vod_initial_position()
    );
    assert_decoder_render_audio(reopened.demuxer.as_mut(), &mut wgpu_harness);
    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::Coupled { coupled, .. },
    ) = reopened
        .source_state
        .stream_configuration()
        .component_variant_projection()
    else {
        panic!("reopen должен заново опубликовать полный coupled catalog");
    };
    assert_eq!(coupled.variants.len(), 2);
    let web_media_core::WebMediaSelectionShape::Components(reopened_components) =
        reopened.source_state.neutral_selection().shape()
    else {
        panic!("reopened native selection должна сохранить component shape");
    };
    assert_eq!(
        reopened_components.semantic_rematch_request(),
        expected_ts_selection,
        "reopen должен снова semantic-rematch TS после обратной перестановки rows"
    );
    assert_eq!(
        reopened
            .source_state
            .neutral_selection()
            .parent()
            .exact()
            .source(),
        stable_source_identity,
        "root refresh/rematch не должен менять stable source lineage"
    );
    assert_eq!(
        process_spy.invocation_count(),
        0,
        "valid TS/fMP4 open, seek, switch и reopen не запускают extractor"
    );
}
