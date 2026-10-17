//! Сквозной путь HLS-TS с AC-3/E-AC-3 звуком через production native HLS admission:
//! loopback HTTP → master `CODECS="…,ec-3"` → catalog/capability intersection →
//! MPEG-TS demux → `A_EAC3`/`A_AC3` packets → production audio decoder → PCM.
//!
//! Сегменты — уже закоммиченные ffmpeg-фикстуры `mpeg-ts-demux/tests/fixtures/ac3/`
//! (README там же: H.264 64×64 Constrained Baseline + синус 440 Гц, 0.5 с).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use audio::{
    AudioDecodeCapabilityProvider, AudioDecoderConfig, EncodedAudioPacket,
    ProductionAudioDecoderFactory, create_audio_decoder,
};
use audio_core::{AudioDecodeCapabilitySnapshot, AudioDecodeCodecFamily};
use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, CURRENT_CAPABILITY_SCHEMA_VERSION,
    SupportedVideoOutput, SystemCapabilities,
};
use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, H264Profile, SupportedVideoDecodeFormat,
    VideoCodec as DecodeVideoCodec, VideoProfile,
};
use fastiplayer_config::AppConfig;
use media_core::{DemuxReadEvent, Demuxer, Packet, TrackKind};
use source_core::CancellationToken;
use video_frame_contract::VideoFrameContract;
use web_media_hls::HlsVodStartIntent;

use super::{
    NativeHlsAdmissionPort, NativeHlsAttempt, NativeHlsPreparationRequest,
    ProductionNativeHlsAdmissionPort,
};
use crate::native_web_source::hls::NativeHlsUrl;
use crate::safe_media_label::SafeMediaLabel;

/// ATSC E-AC-3 5.1 44.1 кГц + H.264 (15 audio packets).
const H264_EAC3_SEGMENT: &[u8] =
    include_bytes!("../../../../mpeg-ts-demux/tests/fixtures/ac3/h264-eac3-5p1-44k1.ts");
/// DVB AC-3 mono 48 кГц без видео (16 audio packets).
const AUDIO_ONLY_AC3_SEGMENT: &[u8] =
    include_bytes!("../../../../mpeg-ts-demux/tests/fixtures/ac3/audio-only-ac3-dvb-mono.ts");

/// Верхняя граница ожидания demux/HTTP: тест не должен зависать при регрессии.
pub(super) const VERTICAL_DEADLINE: Duration = Duration::from_secs(10);
/// Минимальный RMS декодированного синуса: тишина/нули дали бы ~0.
pub(super) const MINIMUM_TONE_RMS: f32 = 0.01;

/// Одноразовый VOD media playlist из одного сегмента.
pub(super) fn media_playlist(segment: &str) -> Vec<u8> {
    format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:0.5,\n{segment}\n#EXT-X-ENDLIST\n"
    )
    .into_bytes()
}

/// Минимальный loopback origin: GET path → фиксированное тело, остальное 404.
pub(super) struct LoopbackOrigin {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl LoopbackOrigin {
    pub(super) fn start(routes: HashMap<&'static str, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind AC-3 HLS origin");
        let address = listener.local_addr().expect("AC-3 HLS origin address");
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
                    .get(path.as_str())
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

    pub(super) fn target(&self, path: &str) -> source_core::HttpRequestTarget {
        source_core::HttpRequestTarget::parse_exact(format!("http://{}{path}", self.address))
            .expect("loopback target")
    }
}

impl Drop for LoopbackOrigin {
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
        let Ok(read) = stream.read(&mut buffer) else {
            break;
        };
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    String::from_utf8_lossy(&request)
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned()
}

/// Capability snapshot ровно под H.264 фикстуры (64×64 Baseline, 10 к/с).
pub(super) fn h264_fixture_capabilities() -> SystemCapabilities {
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
            display_name: "AC-3 HLS fixture backend".to_owned(),
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

/// Production native HLS admission с явным audio capability snapshot.
fn prepare(
    origin: &LoopbackOrigin,
    audio_capabilities: AudioDecodeCapabilitySnapshot,
) -> anyhow::Result<NativeHlsAttempt<super::PreparedNativeHlsMedia>> {
    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    app_config.player.preferred_video_codec_order = vec![fastiplayer_config::VideoCodec::H264];
    let source = NativeHlsUrl::new(
        origin.target("/master.m3u8"),
        SafeMediaLabel::from_service_safe_label("AC-3 native HLS"),
    );
    let system_capabilities = h264_fixture_capabilities();
    let mut port = ProductionNativeHlsAdmissionPort::new(NativeHlsPreparationRequest {
        source: &source,
        expected_selection: None,
        network_config: &app_config.network,
        web_media_config: &app_config.web_media,
        demux_config: &app_config.player.demux,
        preferred_video_codec_order: &app_config.player.preferred_video_codec_order,
        system_capabilities: &system_capabilities,
        audio_capabilities,
        start: HlsVodStartIntent::Beginning,
        cancellation: CancellationToken::new(),
    });
    NativeHlsAdmissionPort::prepare(&mut port)
}

/// Собирает audio packets до EOS, пропуская retry/metadata события.
pub(super) fn audio_packets(demuxer: &mut dyn Demuxer, audio_track_id: u32) -> Vec<Packet> {
    let deadline = Instant::now() + VERTICAL_DEADLINE;
    let mut packets = Vec::new();
    while Instant::now() < deadline {
        match demuxer.next_event().expect("demux event") {
            DemuxReadEvent::Packet(packet) if packet.track_id.get() == audio_track_id => {
                packets.push(packet);
            }
            DemuxReadEvent::Packet(_)
            | DemuxReadEvent::TracksChanged(_)
            | DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                thread::sleep(Duration::from_millis(5));
            }
            DemuxReadEvent::EndOfStream => return packets,
        }
    }
    panic!("HLS demux did not reach EOS before deadline");
}

/// Декодирует пакеты production-декодером; возвращает (sample rate, каналы, RMS, samples).
pub(super) fn decode_to_pcm(
    codec_id: &str,
    track_id: u32,
    sample_rate: Option<u32>,
    channels: Option<u32>,
    packets: &[Packet],
) -> (u32, u32, f32, usize) {
    let mut decoder = create_audio_decoder(AudioDecoderConfig::from_track_metadata(
        track_id,
        codec_id.to_owned(),
        sample_rate,
        channels,
    ))
    .expect("production decoder for Dolby HLS track");
    let mut interleaved = Vec::new();
    for packet in packets {
        let samples = decoder
            .decode(&EncodedAudioPacket::without_timing(track_id, &packet.data))
            .expect("decode Dolby packet");
        interleaved.extend_from_slice(&samples);
    }
    let spec = decoder.decoded_output_spec().expect("decoded spec");
    let rms = (interleaved
        .iter()
        .map(|sample| sample * sample)
        .sum::<f32>()
        / interleaved.len().max(1) as f32)
        .sqrt();
    (
        spec.sample_rate,
        spec.channel_layout.channel_count(),
        rms,
        interleaved.len(),
    )
}

/// Master с одним muxed TS variant-ом `CODECS="avc1.42c00a,<audio>"`.
fn muxed_master(audio_codec: &str) -> Vec<u8> {
    format!(
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=300000,RESOLUTION=64x64,\
         CODECS=\"avc1.42c00a,{audio_codec}\"\nmedia.m3u8\n"
    )
    .into_bytes()
}

/// HLS master `ec-3` + TS segment → native catalog → `A_EAC3` 5.1 packets → PCM.
#[test]
fn hls_ts_eac3_master_reaches_production_audio_decoder() {
    let origin = LoopbackOrigin::start(HashMap::from([
        ("/master.m3u8", muxed_master("ec-3")),
        ("/media.m3u8", media_playlist("seg-0.ts")),
        ("/seg-0.ts", H264_EAC3_SEGMENT.to_vec()),
    ]));
    let production_audio =
        ProductionAudioDecoderFactory::default().audio_decode_capability_snapshot();
    let NativeHlsAttempt::Prepared(mut prepared) =
        prepare(&origin, production_audio).expect("native HLS E-AC-3 preparation")
    else {
        panic!("valid E-AC-3 HLS master must stay native, not require extractor");
    };

    let audio_track = prepared
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .cloned()
        .expect("E-AC-3 audio track published");
    assert!(
        prepared
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Video),
        "muxed variant keeps its H.264 video"
    );
    assert_eq!(audio_track.codec_id, "A_EAC3");
    assert_eq!(audio_track.sample_rate, Some(44_100));
    assert_eq!(audio_track.channels, Some(6));

    let packets = audio_packets(prepared.demuxer.as_mut(), audio_track.id.get());
    assert_eq!(packets.len(), 15, "every E-AC-3 syncframe of the segment");
    let (sample_rate, channels, rms, samples) = decode_to_pcm(
        &audio_track.codec_id,
        audio_track.id.get(),
        audio_track.sample_rate,
        audio_track.channels,
        &packets,
    );
    assert_eq!(sample_rate, 44_100);
    assert_eq!(channels, 6);
    assert!(
        rms > MINIMUM_TONE_RMS,
        "decoded PCM must carry the tone, rms={rms}"
    );
    assert_eq!(
        samples,
        15 * 1_536 * 6,
        "one 1536-sample block set per packet"
    );
}

/// Audio-only HLS радио без master (top media playlist, A13-3) → `A_AC3` mono → PCM.
///
/// Master из одних audio-only variant-ов сюда не взят намеренно: native catalog путь
/// падает на нём и для AAC (`NativeHlsSourceState::new` → «component selection shape is
/// not consistent with catalog layout») — это прежнее ограничение stream model, не AC-3.
/// Admission такого master-а с `CODECS="ac-3"` доказан unit-тестом
/// `native_ingress::ac3_tests::ac3_audio_only_variant_is_native_radio`.
#[test]
fn hls_ts_ac3_audio_only_media_playlist_reaches_production_audio_decoder() {
    let origin = LoopbackOrigin::start(HashMap::from([
        ("/master.m3u8", media_playlist("radio-0.ts")),
        ("/radio-0.ts", AUDIO_ONLY_AC3_SEGMENT.to_vec()),
    ]));
    let production_audio =
        ProductionAudioDecoderFactory::default().audio_decode_capability_snapshot();
    let NativeHlsAttempt::Prepared(mut prepared) =
        prepare(&origin, production_audio).expect("native HLS AC-3 radio preparation")
    else {
        panic!("valid AC-3 radio master must stay native");
    };
    let [audio_track] = prepared.tracks() else {
        panic!("audio-only variant publishes exactly one track");
    };
    let audio_track = audio_track.clone();
    assert_eq!(audio_track.kind, TrackKind::Audio);
    assert_eq!(audio_track.codec_id, "A_AC3");
    assert_eq!(audio_track.channels, Some(1));

    let packets = audio_packets(prepared.demuxer.as_mut(), audio_track.id.get());
    assert_eq!(packets.len(), 16);
    let (sample_rate, channels, rms, _) = decode_to_pcm(
        &audio_track.codec_id,
        audio_track.id.get(),
        audio_track.sample_rate,
        audio_track.channels,
        &packets,
    );
    assert_eq!((sample_rate, channels), (48_000, 1));
    assert!(
        rms > MINIMUM_TONE_RMS,
        "decoded PCM must carry the tone, rms={rms}"
    );
}

/// Snapshot без E-AC-3 (но с AC-3 и AAC) отвергает E-AC-3 variant typed-отказом
/// capability intersection: соседняя Dolby family не считается доказательством.
#[test]
fn hls_ts_eac3_master_is_rejected_when_snapshot_lacks_eac3() {
    let origin = LoopbackOrigin::start(HashMap::from([
        ("/master.m3u8", muxed_master("ec-3")),
        ("/media.m3u8", media_playlist("seg-0.ts")),
        ("/seg-0.ts", H264_EAC3_SEGMENT.to_vec()),
    ]));
    let without_eac3 = AudioDecodeCapabilitySnapshot::empty()
        .with_available_family(AudioDecodeCodecFamily::Ac3)
        .with_available_family(AudioDecodeCodecFamily::Aac);
    let Err(error) = prepare(&origin, without_eac3) else {
        panic!("E-AC-3 must not become playable or fall back without the exact family");
    };
    // Typed причина — отказ capability intersection у authoritative default child-а.
    let rejection = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<web_media_hls::HlsCatalogBuildError>())
        .unwrap_or_else(|| panic!("typed HLS catalog rejection expected, got {error:#}"));
    assert!(
        matches!(
            rejection,
            web_media_hls::HlsCatalogBuildError::ProviderDefaultRejected {
                reason: web_media_hls::HlsCatalogSiblingRejectionReason::CapabilityRejected,
            }
        ),
        "unexpected rejection {rejection:?}"
    );
}
