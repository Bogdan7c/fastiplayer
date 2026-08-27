//! Hermetic HTTP/composition/container fixtures для S32B runtime tests.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::num::{NonZeroU8, NonZeroU32, NonZeroUsize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use demux_api::{
    CompositeComponentLeadPolicy, DemuxRegistry, DemuxSniffBudget, ProgressiveDemuxBufferLimits,
};
use hls_playlist_core::HlsParserLimits;
use media_core::DemuxRetryHint;
use mpeg_ts_demux::{MpegTsDemuxFactory, MpegTsDemuxOptions};
use rustiplayer_config::NetworkConfig;
use source_core::{
    CancellationToken, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig, ValidatedHttpHeaders,
};
use symphonia_demux::{DemuxerOptions, SymphoniaDemuxFactory};
use symphonia_format_isomp4::{
    FragmentAacAudioSpecificConfig, FragmentAacChannelCount, FragmentAacLcConfiguration,
    FragmentAacSampleRate, FragmentBaseDecodeTime, FragmentInitializationCodec,
    FragmentInitializationLimits, FragmentInitializationRequest, FragmentInspectionLimits,
    FragmentMediaKind, FragmentReconstructionRequest, FragmentSampleDefaults, FragmentTimescale,
    FragmentTrackId, FragmentTrackReconstructionIntent, FragmentWriteLimits,
    build_fragmented_initialization_segment, reconstruct_media_fragment,
};
use web_media_adaptive::{AdaptiveHttpContext, AdaptiveRetryPolicy, AdaptiveTransportLimits};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
    SourceIdentity,
};
use web_media_hls::HlsVodOpenPolicy;
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretQueryOverride, SecretRequestContext, SecretRequestScope,
    SourceGeneration, TransportOpenRequest, TransportProviderId,
};

const PMT_PID: u16 = 0x0100;
const VIDEO_PID: u16 = 0x0101;
const AUDIO_PID: u16 = 0x0102;

#[derive(Debug, Clone)]
pub struct ObservedRequest {
    pub request_line: String,
    pub headers: String,
}

pub struct TestServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<ObservedRequest>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    pub fn start(
        handler: impl Fn(usize, &ObservedRequest) -> Vec<u8> + Send + Sync + 'static,
    ) -> Self {
        Self::start_streaming(move |request_index, request, stream| {
            let response = handler(request_index, request);
            stream.write_all(&response).expect("write HLS response");
        })
    }

    /// Запускает fixture, где test owner управляет частичной отправкой response body.
    pub fn start_streaming(
        handler: impl Fn(usize, &ObservedRequest, &mut TcpStream) + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind HLS test server");
        listener
            .set_nonblocking(true)
            .expect("set HLS listener nonblocking");
        let address = listener.local_addr().expect("HLS listener address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let worker_requests = Arc::clone(&requests);
        let handler = Arc::new(handler);
        let thread = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = read_request(&mut stream);
                        let request_index = {
                            let mut observed = worker_requests.lock().expect("HLS requests mutex");
                            let request_index = observed.len();
                            observed.push(request.clone());
                            request_index
                        };
                        handler(request_index, &request, &mut stream);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("HLS test server accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            requests,
            thread: Some(thread),
        }
    }

    pub fn target(&self, path: &str) -> HttpRequestTarget {
        HttpRequestTarget::parse_exact(format!("http://{}{path}", self.address))
            .expect("valid HLS local target")
    }

    pub fn requests(&self) -> Vec<ObservedRequest> {
        self.requests.lock().expect("HLS requests mutex").clone()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join HLS test server");
        }
    }
}

pub fn response(status: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
    let mut response = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
    for (name, value) in headers {
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
    response.push_str("Connection: close\r\n\r\n");
    let mut bytes = response.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

pub fn range_response(request: &ObservedRequest, full: &[u8]) -> Vec<u8> {
    let Some((start, end)) = requested_range(request) else {
        return response("200 OK", &[], full);
    };
    if start > end || end >= full.len() {
        return response("416 Range Not Satisfiable", &[], b"");
    }
    response(
        "206 Partial Content",
        &[(
            "Content-Range",
            format!("bytes {start}-{end}/{}", full.len()),
        )],
        &full[start..=end],
    )
}

fn requested_range(request: &ObservedRequest) -> Option<(usize, usize)> {
    request.headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("range") {
            return None;
        }
        let bounds = value.trim().strip_prefix("bytes=")?;
        let (start, end) = bounds.split_once('-')?;
        Some((start.parse().ok()?, end.parse().ok()?))
    })
}

fn read_request(stream: &mut TcpStream) -> ObservedRequest {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set HLS read timeout");
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1_024];
    loop {
        let read = stream.read(&mut chunk).expect("read HLS request");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let headers = String::from_utf8(bytes).expect("HLS request is HTTP text");
    let request_line = headers.lines().next().unwrap_or_default().to_owned();
    ObservedRequest {
        request_line,
        headers,
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TestQueries<'a> {
    pub segment: Option<&'a str>,
    pub key: Option<&'a str>,
}

pub fn adaptive_context(
    target: &HttpRequestTarget,
    cancellation: CancellationToken,
    generation: SourceGeneration,
    queries: TestQueries<'_>,
) -> AdaptiveHttpContext {
    let source = SourceIdentity::new(73);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(generation.value()),
        CandidateFormatIdentity::new("hls-runtime-test").expect("format identity"),
    );
    let semantic = SemanticIdentity::new(source, "hls-runtime-test").expect("semantic identity");
    let component = MediaComponentIdentity::new(exact, semantic, MediaComponentRole::Muxed)
        .expect("component identity");
    let scope =
        SecretRequestScope::from_target(target, HttpPathScope::new("/").expect("root path scope"));
    let mut secrets = SecretRequestContext::builder(scope)
        .with_headers(ValidatedHttpHeaders::new(Vec::new()).expect("empty headers"));
    if let Some(query) = queries.segment {
        secrets = secrets
            .with_segment_query_override(SecretQueryOverride::new(query).expect("segment query"));
    }
    if let Some(query) = queries.key {
        secrets =
            secrets.with_key_query_override(SecretQueryOverride::new(query).expect("key query"));
    }
    let request = TransportOpenRequest::new(
        TransportProviderId::new("hls-runtime-test").expect("provider id"),
        component,
        target.clone(),
        MediaPresentation::Vod,
        generation,
        secrets.build(),
        RedirectPolicy::same_origin(RedirectHopLimit::new(4).expect("redirect hop limit")),
        cancellation,
    )
    .expect("transport request");
    let source_config =
        SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("source config");
    AdaptiveHttpContext::new(
        request,
        &source_config,
        AdaptiveTransportLimits::new(
            NonZeroUsize::new(64 * 1_024).expect("manifest bound"),
            open_policy().maximum_seek_replay_bytes,
            NonZeroUsize::new(64).expect("descriptor bound"),
        ),
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(2).expect("retry attempts"),
            Duration::from_millis(5),
            Duration::from_millis(10),
            Duration::from_millis(10),
        )
        .expect("retry policy"),
    )
    .expect("adaptive HLS context")
}

pub fn demux_registry() -> Arc<DemuxRegistry> {
    let mpeg_ts_options = MpegTsDemuxOptions::default()
        .with_initial_probe_byte_budget(open_policy().maximum_seek_replay_bytes);
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            MpegTsDemuxFactory::new(mpeg_ts_options).expect("MPEG-TS factory"),
        ))
        .expect("register MPEG-TS");
    registry
        .register(Box::new(
            SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("Symphonia factory"),
        ))
        .expect("register Symphonia");
    Arc::new(registry)
}

pub fn open_policy() -> HlsVodOpenPolicy {
    HlsVodOpenPolicy {
        seek_landing_policy: web_media_hls::HlsVodSeekLandingPolicy::DecodeFromOrBeforeTarget,
        parser_limits: HlsParserLimits::default(),
        demux_sniff_budget: DemuxSniffBudget::new(
            NonZeroUsize::new(8 * 1_024).expect("sniff bytes"),
            NonZeroUsize::new(4).expect("sniff segments"),
            Duration::from_secs(1),
        )
        .expect("sniff budget"),
        progressive_limits: ProgressiveDemuxBufferLimits::new(
            NonZeroUsize::new(64).expect("event capacity"),
            NonZeroUsize::new(512 * 1_024).expect("packet bytes"),
        ),
        retry_hint: DemuxRetryHint::new(Duration::from_millis(5)).expect("retry hint"),
        composite_lead_policy: CompositeComponentLeadPolicy::single_pending_packet(
            Duration::from_secs(2),
            NonZeroUsize::new(256 * 1_024).expect("composite bytes"),
        )
        .expect("composite policy"),
        maximum_key_resource_bytes: NonZeroUsize::new(64).expect("key response bytes"),
        maximum_seek_index_entries: NonZeroUsize::new(256).expect("seek index entries"),
        maximum_seek_replay_events: NonZeroUsize::new(4_096).expect("seek replay events"),
        maximum_seek_replay_bytes: NonZeroUsize::new(16 * 1_024 * 1_024)
            .expect("seek replay bytes"),
    }
}

pub fn muxed_ts(pts: u64) -> Vec<u8> {
    let h264_access_unit = [
        0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x01, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00,
        0x00, 0x01, 0x65, 0x88,
    ];
    TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)])
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID), (0x0f, AUDIO_PID)])
        .pes(
            VIDEO_PID,
            pts,
            Some(pts.saturating_sub(3_000)),
            &h264_access_unit,
        )
        .pes(AUDIO_PID, pts, None, &adts_frame(&[0x11, 0x22]))
        .finish()
}

/// Строит один самостоятельный TS segment с одним RAP и packet-ом audio evidence на секунду.
pub fn long_muxed_ts_segment(start_pts_90khz: u64, duration_seconds: u64) -> Vec<u8> {
    let h264_access_unit = [
        0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x01, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00,
        0x00, 0x01, 0x65, 0x80,
    ];
    let mut builder = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)])
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID), (0x0f, AUDIO_PID)])
        .pes(
            VIDEO_PID,
            start_pts_90khz,
            Some(start_pts_90khz.saturating_sub(3_000)),
            &h264_access_unit,
        );
    for second in 0..duration_seconds {
        builder = builder.pes(
            AUDIO_PID,
            start_pts_90khz.saturating_add(second.saturating_mul(90_000)),
            None,
            &adts_frame(&[0x11, 0x22]),
        );
    }
    builder.finish()
}

/// Строит 10 MiB TS resource с ранними RAP/AAC и длинным валидным null-packet хвостом.
#[allow(dead_code)] // Fixture используется streaming manifest-seek integration binary.
pub fn large_muxed_ts_segment_with_early_landing(start_pts_90khz: u64) -> Vec<u8> {
    let mut bytes = muxed_ts_segment_with_early_landing(start_pts_90khz);
    let mut null_packet = [0xff_u8; 188];
    null_packet[..4].copy_from_slice(&[0x47, 0x1f, 0xff, 0x10]);
    while bytes.len() < 10 * 1_024 * 1_024 {
        bytes.extend_from_slice(&null_packet);
    }
    bytes
}

/// Маленький TS segment завершает RAP внутри того же resource и не требует следующего GET.
#[allow(dead_code)] // Fixture используется отдельными initial-open integration tests.
pub fn muxed_ts_segment_with_early_landing(start_pts_90khz: u64) -> Vec<u8> {
    let h264_random_access_unit = [
        0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x01, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00,
        0x00, 0x01, 0x65, 0x80,
    ];
    let h264_following_unit = [0x00, 0x00, 0x01, 0x41, 0x80];
    TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)])
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID), (0x0f, AUDIO_PID)])
        .pes(AUDIO_PID, start_pts_90khz, None, &adts_frame(&[0x11, 0x22]))
        .pes(
            AUDIO_PID,
            start_pts_90khz.saturating_add(3_000),
            None,
            &adts_frame(&[0x33, 0x44]),
        )
        .pes(
            VIDEO_PID,
            start_pts_90khz,
            Some(start_pts_90khz.saturating_sub(3_000)),
            &h264_random_access_unit,
        )
        .pes(
            VIDEO_PID,
            start_pts_90khz.saturating_add(3_000),
            None,
            &h264_random_access_unit,
        )
        .pes(
            VIDEO_PID,
            start_pts_90khz.saturating_add(6_000),
            None,
            &h264_following_unit,
        )
        .finish()
}

/// Строит muxed TS segment, где полный AAC PES появляется позже default 4096-packet probe-а.
#[allow(dead_code)] // Fixture используется только manifest-receipted integration binary.
pub fn long_interleaved_muxed_ts_segment(start_pts_90khz: u64) -> Vec<u8> {
    let h264_access_unit = [
        0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x01, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00,
        0x00, 0x01, 0x65, 0x80,
    ];
    let audio_elementary = std::iter::repeat_with(|| adts_frame(&[0x11, 0x22]))
        .take(40)
        .flatten()
        .collect::<Vec<_>>();
    TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)])
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID), (0x0f, AUDIO_PID)])
        .pes(
            VIDEO_PID,
            start_pts_90khz,
            Some(start_pts_90khz.saturating_sub(3_000)),
            &h264_access_unit,
        )
        .pes_with_interleaved_null_packets(AUDIO_PID, start_pts_90khz, &audio_elementary, 2_100)
        .finish()
}

/// Строит валидный muxed TS segment без IDR, чтобы проверить decode-safe fallback seek-а.
#[allow(dead_code)] // Этот fixture нужен отдельной integration test binary, но не `runtime.rs`.
pub fn long_muxed_ts_segment_without_rap(start_pts_90khz: u64, duration_seconds: u64) -> Vec<u8> {
    let h264_inter_access_unit = [0x00, 0x00, 0x01, 0x41, 0x80];
    let mut builder = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)])
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID), (0x0f, AUDIO_PID)])
        .pes(
            VIDEO_PID,
            start_pts_90khz,
            Some(start_pts_90khz.saturating_sub(3_000)),
            &h264_inter_access_unit,
        );
    for second in 0..duration_seconds {
        builder = builder.pes(
            AUDIO_PID,
            start_pts_90khz.saturating_add(second.saturating_mul(90_000)),
            None,
            &adts_frame(&[0x11, 0x22]),
        );
    }
    builder.finish()
}

/// Строит самостоятельный video-only TS segment с RAP в начале manifest interval-а.
#[allow(dead_code)] // Этот fixture нужен separate-A/V integration test binary.
pub fn long_video_ts_segment(start_pts_90khz: u64) -> Vec<u8> {
    let h264_access_unit = [
        0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x01, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00,
        0x00, 0x01, 0x65, 0x80,
    ];
    TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)])
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID)])
        .pes(
            VIDEO_PID,
            start_pts_90khz,
            Some(start_pts_90khz.saturating_sub(3_000)),
            &h264_access_unit,
        )
        .finish()
}

/// Строит самостоятельный audio-only TS segment с packet evidence на каждую секунду.
#[allow(dead_code)] // Этот fixture нужен separate-A/V integration test binary.
pub fn long_audio_ts_segment(start_pts_90khz: u64, duration_seconds: u64) -> Vec<u8> {
    let mut builder =
        TsFixtureBuilder::new()
            .pat(&[(1, PMT_PID)])
            .pmt(PMT_PID, 1, &[(0x0f, AUDIO_PID)]);
    for second in 0..duration_seconds {
        builder = builder.pes(
            AUDIO_PID,
            start_pts_90khz.saturating_add(second.saturating_mul(90_000)),
            None,
            &adts_frame(&[0x11, 0x22]),
        );
    }
    builder.finish()
}

/// Строит canonical audio fMP4 MAP и contiguous 4-second fragments с exact `tfdt`.
pub fn long_audio_fmp4_segments(segment_count: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
    const SEGMENT_DURATION_UNITS: u64 = 40_000_000;
    const AUDIO_FRAGMENT: &[u8] = include_bytes!(
        "../../../symphonia-format-isomp4-patch/fixtures/smooth-piff/audio-64008-0.bin"
    );
    let sample_rate = FragmentAacSampleRate::try_new(48_000).expect("valid AAC sample rate");
    let channels = FragmentAacChannelCount::try_new(2).expect("valid stereo channel count");
    let audio_specific_config =
        FragmentAacAudioSpecificConfig::try_new(&[0x11, 0x90]).expect("valid AAC-LC ASC");
    let codec = FragmentAacLcConfiguration::try_new(sample_rate, channels, audio_specific_config)
        .expect("valid AAC-LC configuration");
    let initialization_limits = FragmentInitializationLimits::builder()
        .maximum_output_bytes(16 * 1_024)
        .maximum_codec_configuration_bytes(1_024)
        .build()
        .expect("valid fMP4 initialization limits");
    let cancellation = || false;
    let track_id = FragmentTrackId::new(NonZeroU32::MIN);
    let timescale = FragmentTimescale::new(NonZeroU32::new(10_000_000).expect("valid timescale"));
    let initialization =
        build_fragmented_initialization_segment(FragmentInitializationRequest::new(
            track_id,
            timescale,
            FragmentInitializationCodec::AacLowComplexity(codec),
            &initialization_limits,
            &cancellation,
        ))
        .expect("build canonical AAC initialization")
        .into_bytes();
    let inspection_limits = FragmentInspectionLimits::builder()
        .max_input_bytes(256 * 1_024)
        .max_box_count(64)
        .max_box_depth(4)
        .max_traf_count(1)
        .max_trun_count(8)
        .max_samples(512)
        .max_sample_table_bytes(64 * 1_024)
        .max_box_payload_bytes(256 * 1_024)
        .build()
        .expect("valid fMP4 inspection limits");
    let write_limits = FragmentWriteLimits::try_new(512 * 1_024).expect("valid write limit");
    let media = (0..segment_count)
        .map(|segment_index| {
            let base_decode_time = u64::try_from(segment_index)
                .expect("test segment index fits u64")
                .saturating_mul(SEGMENT_DURATION_UNITS);
            let track = FragmentTrackReconstructionIntent::new(
                track_id,
                FragmentBaseDecodeTime::new(base_decode_time),
                FragmentMediaKind::AudioWithoutRandomAccessRequirement,
                FragmentSampleDefaults::absent(),
            );
            reconstruct_media_fragment(FragmentReconstructionRequest::new(
                AUDIO_FRAGMENT,
                symphonia_format_isomp4::FragmentCompositionOffsetSemantics::IsoBmffVersioned,
                track,
                &inspection_limits,
                write_limits,
                &cancellation,
            ))
            .expect("reconstruct canonical AAC fragment")
            .into_bytes()
        })
        .collect();
    (initialization, media)
}

pub fn video_ts(pts: u64) -> Vec<u8> {
    TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)])
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID)])
        .pes(
            VIDEO_PID,
            pts,
            Some(pts.saturating_sub(3_000)),
            &[
                0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x01, 0x00, 0x00, 0x01, 0x68, 0xce,
                0x00, 0x00, 0x01, 0x65, 0x88,
            ],
        )
        .finish()
}

pub fn ts_map_and_media(pts: u64) -> (Vec<u8>, Vec<u8>) {
    let initialization = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)])
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID), (0x0f, AUDIO_PID)])
        .finish();
    let media = TsFixtureBuilder::new()
        .pes(
            VIDEO_PID,
            pts,
            Some(pts.saturating_sub(3_000)),
            &[
                0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x01, 0x00, 0x00, 0x01, 0x68, 0xce,
                0x00, 0x00, 0x01, 0x65, 0x88,
            ],
        )
        .pes(AUDIO_PID, pts, None, &adts_frame(&[0x11, 0x22]))
        .finish();
    (initialization, media)
}

pub fn muxed_fmp4() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    split_fmp4(
        decode_base64(include_str!("../fixtures/muxed-fmp4.base64")),
        1_248,
        2_314,
        2_747,
    )
}

/// Возвращает тот же валидный fragmented MP4, но с точным H.264 `avc3` sample entry.
pub fn muxed_avc3_fmp4() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (mut initialization, first_media, second_media) = muxed_fmp4();
    let avc1_offsets = initialization
        .windows(4)
        .enumerate()
        .filter_map(|(offset, atom_type)| (atom_type == b"avc1").then_some(offset))
        .collect::<Vec<_>>();
    assert_eq!(
        avc1_offsets.len(),
        1,
        "сгенерированный fixture должен содержать ровно один H.264 sample entry"
    );
    let sample_entry_offset = avc1_offsets[0];
    initialization[sample_entry_offset..sample_entry_offset + 4].copy_from_slice(b"avc3");
    (initialization, first_media, second_media)
}

pub fn audio_fmp4() -> (Vec<u8>, Vec<u8>) {
    let (initialization, first, _) = split_fmp4(
        decode_base64(include_str!("../fixtures/audio-fmp4.base64")),
        729,
        3_294,
        3_294,
    );
    (initialization, first)
}

fn split_fmp4(
    bytes: Vec<u8>,
    initialization_end: usize,
    first_media_end: usize,
    second_media_end: usize,
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    assert_eq!(&bytes[4..8], b"ftyp");
    assert_eq!(
        &bytes[initialization_end + 4..initialization_end + 8],
        b"moof"
    );
    (
        bytes[..initialization_end].to_vec(),
        bytes[initialization_end..first_media_end].to_vec(),
        bytes[first_media_end..second_media_end].to_vec(),
    )
}

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
            _ => panic!("invalid generated base64 fixture"),
        };
        accumulator = (accumulator << 6) | u32::from(value);
        accumulated_bits += 6;
        if accumulated_bits >= 8 {
            accumulated_bits -= 8;
            decoded.push((accumulator >> accumulated_bits) as u8);
            accumulator &= (1_u32 << accumulated_bits) - 1;
        }
    }
    decoded
}

struct TsFixtureBuilder {
    bytes: Vec<u8>,
    continuity: [u8; 8_192],
}

impl TsFixtureBuilder {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            continuity: [0; 8_192],
        }
    }

    fn pat(mut self, programs: &[(u16, u16)]) -> Self {
        let mut body = vec![0x00, 0xb0, 0x00, 0x00, 0x01, 0xc1, 0x00, 0x00];
        for &(program_number, pmt_pid) in programs {
            body.extend_from_slice(&program_number.to_be_bytes());
            body.push(0xe0 | ((pmt_pid >> 8) as u8 & 0x1f));
            body.push(pmt_pid as u8);
        }
        finalize_section(&mut body);
        let mut payload = vec![0];
        payload.extend(body);
        self.push_payload(0, true, &payload);
        self
    }

    fn pmt(mut self, pmt_pid: u16, program: u16, streams: &[(u8, u16)]) -> Self {
        let pcr_pid = streams.first().map_or(0x1fff, |stream| stream.1);
        let mut body = vec![
            0x02,
            0xb0,
            0x00,
            (program >> 8) as u8,
            program as u8,
            0xc1,
            0x00,
            0x00,
            0xe0 | ((pcr_pid >> 8) as u8 & 0x1f),
            pcr_pid as u8,
            0xf0,
            0x00,
        ];
        for &(stream_type, pid) in streams {
            body.extend_from_slice(&[
                stream_type,
                0xe0 | ((pid >> 8) as u8 & 0x1f),
                pid as u8,
                0xf0,
                0x00,
            ]);
        }
        finalize_section(&mut body);
        let mut payload = vec![0];
        payload.extend(body);
        self.push_payload(pmt_pid, true, &payload);
        self
    }

    fn pes(mut self, pid: u16, pts: u64, dts: Option<u64>, elementary: &[u8]) -> Self {
        let mut pes = vec![0x00, 0x00, 0x01, 0xe0, 0x00, 0x00, 0x80];
        if let Some(dts) = dts {
            pes.extend_from_slice(&[0xc0, 10]);
            pes.extend_from_slice(&encode_timestamp(0b0011, pts));
            pes.extend_from_slice(&encode_timestamp(0b0001, dts));
        } else {
            pes.extend_from_slice(&[0x80, 5]);
            pes.extend_from_slice(&encode_timestamp(0b0010, pts));
        }
        pes.extend_from_slice(elementary);
        let packet_length = pes.len() - 6;
        pes[4..6].copy_from_slice(&(packet_length as u16).to_be_bytes());
        for (index, chunk) in pes.chunks(184).enumerate() {
            self.push_payload(pid, index == 0, chunk);
        }
        self
    }

    /// Сохраняет один PES, но разносит его TS payload packets null-packet-ами другого PID.
    fn pes_with_interleaved_null_packets(
        mut self,
        pid: u16,
        pts: u64,
        elementary: &[u8],
        null_packets_between_payload_packets: usize,
    ) -> Self {
        let encoded_pes = Self::new().pes(pid, pts, None, elementary).finish();
        let null_payload = [0_u8; 184];
        for encoded_packet in encoded_pes.chunks_exact(188) {
            self.bytes.extend_from_slice(encoded_packet);
            for _ in 0..null_packets_between_payload_packets {
                self.push_payload(0x1fff, false, &null_payload);
            }
        }
        self
    }

    fn push_payload(&mut self, pid: u16, payload_start: bool, payload: &[u8]) {
        assert!(payload.len() <= 184);
        let mut packet = [0xff_u8; 188];
        packet[0] = 0x47;
        packet[1] = ((payload_start as u8) << 6) | ((pid >> 8) as u8 & 0x1f);
        packet[2] = pid as u8;
        let continuity = self.continuity[usize::from(pid)];
        self.continuity[usize::from(pid)] = (continuity + 1) & 0x0f;
        if payload.len() < 184 {
            let adaptation_length = 183 - payload.len();
            packet[3] = 0x30 | continuity;
            packet[4] = adaptation_length as u8;
            if adaptation_length > 0 {
                packet[5] = 0;
            }
            let payload_offset = 5 + adaptation_length;
            packet[payload_offset..payload_offset + payload.len()].copy_from_slice(payload);
        } else {
            packet[3] = 0x10 | continuity;
            packet[4..].copy_from_slice(payload);
        }
        self.bytes.extend_from_slice(&packet);
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

fn adts_frame(raw_aac: &[u8]) -> Vec<u8> {
    let frame_length = 7 + raw_aac.len();
    let mut frame = vec![
        0xff,
        0xf1,
        0x50,
        0x80 | ((frame_length >> 11) as u8 & 0x03),
        (frame_length >> 3) as u8,
        ((frame_length & 0x07) as u8) << 5 | 0x1f,
        0xfc,
    ];
    frame.extend_from_slice(raw_aac);
    frame
}

fn encode_timestamp(prefix: u8, timestamp: u64) -> [u8; 5] {
    let timestamp = timestamp & ((1_u64 << 33) - 1);
    [
        (prefix << 4) | (((timestamp >> 30) as u8 & 0x07) << 1) | 1,
        (timestamp >> 22) as u8,
        (((timestamp >> 15) as u8 & 0x7f) << 1) | 1,
        (timestamp >> 7) as u8,
        ((timestamp as u8 & 0x7f) << 1) | 1,
    ]
}

fn finalize_section(section: &mut Vec<u8>) {
    let section_length = section.len() - 3 + 4;
    section[1] = 0xb0 | ((section_length >> 8) as u8 & 0x0f);
    section[2] = section_length as u8;
    section.extend_from_slice(&mpeg_crc32(section).to_be_bytes());
}

fn mpeg_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}
