//! Сквозной путь fMP4/CMAF с E-AC-3 (`ec-3` + `dec3`) через production native
//! DASH и HLS admission: loopback HTTP → MPD / master `CODECS="avc1…,ec-3"` →
//! capability intersection → ordered fMP4 segments → Symphonia isomp4 →
//! `A_EAC3` packets → production `audio::create_audio_decoder` → тон в каждом канале.
//!
//! Фикстура — `symphonia-demux/tests/fixtures/ac3/h264-eac3-5p1-fragmented.mp4`
//! (README и `generate.sh` там же): H.264 64×64 + E-AC-3 5.1(side), 1 с, два
//! фрагмента `moof+mdat` по keyframe-ам. Тест режет её по top-level box-ам на
//! init и media сегменты — как это делает packager DASH/HLS.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use audio::{AudioDecodeCapabilityProvider, ProductionAudioDecoderFactory};
use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, CURRENT_CAPABILITY_SCHEMA_VERSION,
    SupportedVideoOutput, SystemCapabilities,
};
use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, H264Profile, SupportedVideoDecodeFormat,
    VideoCodec as DecodeVideoCodec, VideoProfile,
};
use fastiplayer_config::AppConfig;
use media_core::{Demuxer, TrackKind};
use source_core::CancellationToken;
use video_frame_contract::VideoFrameContract;
use web_media_hls::HlsVodStartIntent;

use super::dash::{NativeDashAttempt, NativeDashPreparationRequest, prepare_native_dash_attempt};
use super::hls::{
    NativeHlsAdmissionPort, NativeHlsAttempt, NativeHlsPreparationRequest,
    ProductionNativeHlsAdmissionPort,
};
use crate::ac3_test_support::{
    SAMPLES_PER_ACCESS_UNIT, SURROUND_TONES, assert_channel_tones, collect_track_packets,
    decode_track_packets, first_audio_track,
};
use crate::native_web_source::dash::NativeDashUrl;
use crate::native_web_source::hls::NativeHlsUrl;
use crate::safe_media_label::SafeMediaLabel;

/// Fragmented MP4 H.264 + E-AC-3 5.1 (init `ftyp+moov`, два `moof+mdat`, `mfra`).
const FRAGMENTED_EAC3: &[u8] =
    include_bytes!("../../../symphonia-demux/tests/fixtures/ac3/h264-eac3-5p1-fragmented.mp4");
/// Аудиопакетов E-AC-3 во всей фикстуре (1 с при 48 кГц, ffprobe).
const FIXTURE_AUDIO_PACKETS: usize = 32;
/// Частота дискретизации фикстуры.
const SAMPLE_RATE: u32 = 48_000;
/// RFC 6381 codec строки фикстуры: H.264 Constrained Baseline level 1.0 + E-AC-3.
const MUXED_CODECS: &str = "avc1.42c00a,ec-3";

/// Init (ftyp+moov) и media сегменты (moof+mdat) по top-level box-ам ISO-BMFF.
struct FragmentedFixture {
    initialization: Vec<u8>,
    media_segments: Vec<Vec<u8>>,
}

/// Режет fMP4 по 32-битным размерам top-level box-ов; `mfra` в сегменты не входит.
fn split_fragmented_fixture() -> FragmentedFixture {
    let mut boxes = Vec::new();
    let mut offset = 0;
    while offset + 8 <= FRAGMENTED_EAC3.len() {
        let size_bytes: [u8; 4] = FRAGMENTED_EAC3[offset..offset + 4]
            .try_into()
            .expect("box size");
        let size = u32::from_be_bytes(size_bytes) as usize;
        assert!(size >= 8, "fixture uses 32-bit box sizes only");
        let kind = &FRAGMENTED_EAC3[offset + 4..offset + 8];
        boxes.push((kind.to_vec(), &FRAGMENTED_EAC3[offset..offset + size]));
        offset += size;
    }
    assert_eq!(offset, FRAGMENTED_EAC3.len(), "whole top-level boxes");
    let mut initialization = Vec::new();
    let mut media_segments: Vec<Vec<u8>> = Vec::new();
    for (kind, bytes) in boxes {
        match kind.as_slice() {
            b"ftyp" | b"moov" => initialization.extend_from_slice(bytes),
            b"moof" => media_segments.push(bytes.to_vec()),
            b"mdat" => media_segments
                .last_mut()
                .expect("mdat follows its moof")
                .extend_from_slice(bytes),
            _ => {}
        }
    }
    assert_eq!(media_segments.len(), 2, "two keyframe fragments");
    FragmentedFixture {
        initialization,
        media_segments,
    }
}

/// Минимальный loopback origin: GET path → фиксированное тело, остальное 404.
struct FixtureOrigin {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FixtureOrigin {
    fn start(routes: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fMP4 origin");
        let address = listener.local_addr().expect("fMP4 origin address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                if worker_stop.load(Ordering::Acquire) {
                    break;
                }
                let path = read_request_path(&mut stream);
                let (status, body) = routes
                    .get(&path)
                    .map_or(("404 Not Found", &[][..]), |body| {
                        ("200 OK", body.as_slice())
                    });
                let mut response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                response.extend_from_slice(body);
                // Клиент может закрыть соединение раньше (cancel/probe) — это не ошибка теста.
                let _ = stream.write_all(&response);
            }
        });
        Self {
            address,
            stop,
            worker: Some(worker),
        }
    }

    fn target(&self, path: &str) -> source_core::HttpRequestTarget {
        source_core::HttpRequestTarget::parse_exact(format!("http://{}{path}", self.address))
            .expect("loopback target")
    }
}

impl Drop for FixtureOrigin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Будим blocking accept, чтобы worker увидел stop.
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Читает request line до конца headers и возвращает path.
fn read_request_path(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => request.extend_from_slice(&buffer[..read]),
        }
    }
    String::from_utf8_lossy(&request)
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned()
}

/// Capability snapshot ровно под H.264 фикстуры (64×64 Baseline, 10 к/с).
fn h264_fixture_capabilities() -> SystemCapabilities {
    let backend_id = DecodeBackendId::new("dolby_hls").expect("valid backend ID");
    let output = SupportedVideoOutput {
        backend: backend_id.clone(),
        decode_format: SupportedVideoDecodeFormat {
            codec: DecodeVideoCodec::H264,
            profile: VideoProfile::H264(H264Profile::Baseline),
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            max_width: Some(64),
            max_height: Some(64),
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
            display_name: "E-AC-3 fMP4 fixture backend".to_owned(),
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

/// Config без extractor-а: native admission обязан справиться сам.
fn native_only_config() -> AppConfig {
    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    app_config.player.preferred_video_codec_order = vec![fastiplayer_config::VideoCodec::H264];
    app_config
}

/// Демуксер выбранной muxed row: видео рядом, `A_EAC3` 5.1 доходит до PCM в каждой lane.
fn assert_muxed_eac3_reaches_decoder(demuxer: &mut dyn Demuxer) {
    assert!(
        demuxer
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Video),
        "muxed row keeps its H.264 video: {:?}",
        demuxer.tracks()
    );
    let track = first_audio_track(demuxer);
    assert_eq!(track.codec_id, "A_EAC3");
    assert_eq!(
        (track.sample_rate, track.channels),
        (Some(SAMPLE_RATE), Some(6)),
        "dec3 gives the 5.1 layout before the first frame"
    );
    let packets = collect_track_packets(demuxer, &track);
    assert_eq!(packets.len(), FIXTURE_AUDIO_PACKETS, "both fragments");
    let decoded = decode_track_packets(&track, &packets);
    assert_eq!(decoded.sample_rate, SAMPLE_RATE);
    assert_eq!(
        decoded.interleaved.len(),
        FIXTURE_AUDIO_PACKETS * SAMPLES_PER_ACCESS_UNIT * 6
    );
    assert_channel_tones(&decoded, &SURROUND_TONES);
}

/// Static MPD: одна muxed fMP4 row `avc1…,ec-3` с Dolby channel mask 5.1 (F801).
fn dash_manifest() -> Vec<u8> {
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static"
            mediaPresentationDuration="PT1S" minBufferTime="PT1S"
            profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
          <Period id="p0" duration="PT1S">
            <AdaptationSet id="muxed" contentType="application" mimeType="video/mp4"
                codecs="{MUXED_CODECS}" width="64" height="64">
              <Representation id="h264-eac3" bandwidth="400000" width="64" height="64">
                <AudioChannelConfiguration
                    schemeIdUri="tag:dolby.com,2014:dash:audio_channel_configuration:2011"
                    value="F801"/>
                <SegmentList timescale="10" duration="5">
                  <Initialization sourceURL="init.mp4"/>
                  <SegmentURL media="seg-0.m4s"/>
                  <SegmentURL media="seg-1.m4s"/>
                </SegmentList>
              </Representation>
            </AdaptationSet>
          </Period>
        </MPD>"#
    )
    .into_bytes()
}

/// Маршруты init/media сегментов + указанный manifest.
fn segment_routes(manifest_path: &str, manifest: Vec<u8>) -> HashMap<String, Vec<u8>> {
    let fixture = split_fragmented_fixture();
    let mut routes = HashMap::from([
        (manifest_path.to_owned(), manifest),
        ("/init.mp4".to_owned(), fixture.initialization),
    ]);
    for (index, segment) in fixture.media_segments.into_iter().enumerate() {
        routes.insert(format!("/seg-{index}.m4s"), segment);
    }
    routes
}

/// DASH fMP4 `ec-3` → native admission → `A_EAC3` 5.1 → PCM с тоном в каждой lane.
#[test]
fn dash_fmp4_eac3_reaches_production_audio_decoder() {
    let origin = FixtureOrigin::start(segment_routes("/manifest.mpd", dash_manifest()));
    let app_config = native_only_config();
    let system_capabilities = h264_fixture_capabilities();
    let source = NativeDashUrl::new(
        origin.target("/manifest.mpd"),
        SafeMediaLabel::from_service_safe_label("E-AC-3 native DASH"),
    );
    let attempt = prepare_native_dash_attempt(NativeDashPreparationRequest {
        source: &source,
        expected_selection: None,
        network_config: &app_config.network,
        web_media_config: &app_config.web_media,
        demux_config: &app_config.player.demux,
        system_capabilities: &system_capabilities,
        audio_capabilities: ProductionAudioDecoderFactory::default()
            .audio_decode_capability_snapshot(),
        cancellation: CancellationToken::new(),
    })
    .expect("native DASH E-AC-3 admission");
    let NativeDashAttempt::Prepared(mut prepared) = attempt else {
        panic!("valid E-AC-3 MPD must stay native, not require extractor");
    };
    assert_muxed_eac3_reaches_decoder(prepared.demuxer.as_mut());
}

/// HLS master `CODECS="avc1…,ec-3"` + fMP4 (`EXT-X-MAP`) → `A_EAC3` 5.1 → PCM.
#[test]
fn hls_fmp4_eac3_reaches_production_audio_decoder() {
    let master = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-STREAM-INF:BANDWIDTH=400000,RESOLUTION=64x64,\
         CODECS=\"{MUXED_CODECS}\"\nmedia.m3u8\n"
    );
    let media = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:0.5,\nseg-0.m4s\n\
         #EXTINF:0.5,\nseg-1.m4s\n#EXT-X-ENDLIST\n";
    let mut routes = segment_routes("/master.m3u8", master.into_bytes());
    routes.insert("/media.m3u8".to_owned(), media.as_bytes().to_vec());
    let origin = FixtureOrigin::start(routes);
    let app_config = native_only_config();
    let system_capabilities = h264_fixture_capabilities();
    let source = NativeHlsUrl::new(
        origin.target("/master.m3u8"),
        SafeMediaLabel::from_service_safe_label("E-AC-3 native HLS fMP4"),
    );
    let mut port = ProductionNativeHlsAdmissionPort::new(NativeHlsPreparationRequest {
        source: &source,
        expected_selection: None,
        network_config: &app_config.network,
        web_media_config: &app_config.web_media,
        demux_config: &app_config.player.demux,
        preferred_video_codec_order: &app_config.player.preferred_video_codec_order,
        system_capabilities: &system_capabilities,
        audio_capabilities: ProductionAudioDecoderFactory::default()
            .audio_decode_capability_snapshot(),
        start: HlsVodStartIntent::Beginning,
        cancellation: CancellationToken::new(),
    });
    let NativeHlsAttempt::Prepared(mut prepared) =
        NativeHlsAdmissionPort::prepare(&mut port).expect("native HLS fMP4 E-AC-3 preparation")
    else {
        panic!("valid E-AC-3 HLS fMP4 master must stay native");
    };
    assert_muxed_eac3_reaches_decoder(prepared.demuxer.as_mut());
}
