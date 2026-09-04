//! Cross-owner regressions native HLS startup без WGPU, интернета и wall-clock polling.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::num::{NonZeroU8, NonZeroUsize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use demux_api::DemuxRegistry;
use media_core::MediaTime;
use mpeg_ts_demux::{MpegTsDemuxFactory, MpegTsDemuxOptions};
use player_core::{PreparedDemuxSeekLandingPolicy, PreparedInitialPosition};
use rustiplayer_config::NetworkConfig;
use source_core::{
    CancellationToken, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig, ValidatedHttpHeaders,
};
use web_media_adaptive::{AdaptiveHttpContext, AdaptiveRetryPolicy, AdaptiveTransportLimits};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, SemanticIdentity,
    SourceIdentity,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsMainTrackLayoutIntent, HlsManifestInput, HlsRequestOverrides, HlsRequiredContainer,
    HlsVariantSelectionIntent, HlsVodOpenRequest, HlsVodStartIntent,
};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration,
    TransportOpenRequest, TransportProviderId,
};

use super::{
    PostInstalledStartupPositionFacts, PostInstalledStartupPositionPlan,
    plan_post_installed_startup_position,
};
use crate::playlist_runtime::{ResumePositionWarning, StartupPosition};
use crate::state::strong_media_open::{
    PreparedPositionRestoreStrategy, prepared_position_restore_strategy,
};
use crate::web_media_hls_open::{prepare_native_hls_player_media, prepare_native_hls_vod};

const TEST_GENERATION: SourceGeneration = SourceGeneration::new(71);
const PMT_PID: u16 = 0x0100;
const VIDEO_PID: u16 = 0x0101;
const AUDIO_PID: u16 = 0x0102;

#[derive(Debug, Clone)]
struct ServedRequest {
    path: String,
    body_bytes: usize,
}

/// Blocking loopback server завершается explicit wake-connect-ом, а не polling sleep-ом.
struct ControlledHlsServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<ServedRequest>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl ControlledHlsServer {
    fn start(routes: HashMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind controlled HLS server");
        let address = listener.local_addr().expect("controlled HLS address");
        let routes = Arc::new(routes);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let worker_requests = Arc::clone(&requests);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                let (mut stream, _) = listener.accept().expect("accept controlled HLS request");
                if worker_stop.load(Ordering::Acquire) {
                    break;
                }
                let path = read_request_path(&mut stream);
                let (status, body) = routes
                    .get(&path)
                    .map_or(("404 Not Found", &[][..]), |body| {
                        ("200 OK", body.as_slice())
                    });
                worker_requests
                    .lock()
                    .expect("controlled HLS request log")
                    .push(ServedRequest {
                        path,
                        body_bytes: body.len(),
                    });
                stream
                    .write_all(&http_response(status, body))
                    .expect("write controlled HLS response");
            }
        });
        Self {
            address,
            stop,
            requests,
            worker: Some(worker),
        }
    }

    fn target(&self, path: &str) -> HttpRequestTarget {
        HttpRequestTarget::parse_exact(format!("http://{}{path}", self.address))
            .expect("valid controlled HLS target")
    }

    fn request_count(&self, path: &str) -> usize {
        self.requests
            .lock()
            .expect("controlled HLS request log")
            .iter()
            .filter(|request| request.path == path)
            .count()
    }

    fn served_body_bytes(&self, path: &str) -> usize {
        self.requests
            .lock()
            .expect("controlled HLS request log")
            .iter()
            .filter(|request| request.path == path)
            .map(|request| request.body_bytes)
            .sum()
    }
}

impl Drop for ControlledHlsServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join controlled HLS server");
        }
    }
}

fn read_request_path(stream: &mut TcpStream) -> String {
    let mut request_bytes = Vec::new();
    let mut chunk = [0_u8; 1_024];
    loop {
        let read = stream
            .read(&mut chunk)
            .expect("read controlled HLS request");
        assert!(read > 0, "HTTP request ended before its header boundary");
        request_bytes.extend_from_slice(&chunk[..read]);
        if request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8(request_bytes).expect("controlled request is UTF-8 HTTP");
    request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("controlled request path")
        .to_owned()
}

fn http_response(status: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

fn native_hls_request(server: &ControlledHlsServer, manifest_path: &str) -> HlsVodOpenRequest {
    let target = server.target(manifest_path);
    let limits = AdaptiveTransportLimits::new(
        NonZeroUsize::new(64 * 1_024).expect("manifest byte bound"),
        NonZeroUsize::new(2 * 1_024 * 1_024).expect("segment byte bound"),
        NonZeroUsize::new(64).expect("descriptor byte bound"),
    );
    let http = controlled_http_context(&target, limits);
    let mut registry = DemuxRegistry::new();
    let mpeg_ts_options =
        MpegTsDemuxOptions::default().with_initial_probe_byte_budget(limits.maximum_segment_bytes);
    registry
        .register(Box::new(
            MpegTsDemuxFactory::new(mpeg_ts_options).expect("MPEG-TS test factory"),
        ))
        .expect("register MPEG-TS test factory");
    HlsVodOpenRequest {
        http,
        generation: TEST_GENERATION,
        manifest: HlsManifestInput::Fetch {
            selected_url: target,
        },
        selection: HlsVariantSelectionIntent {
            resolution: None,
            codecs: None,
            audio: HlsAudioLayoutIntent::Muxed,
            main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
        },
        overrides: HlsRequestOverrides::new(None),
        containers: HlsComponentContainerIntent {
            main: HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream),
            alternate_audio: None,
        },
        demux_registry: Arc::new(registry),
        policy: crate::web_media_hls_open::hls_policy(limits)
            .expect("production HLS policy from controlled limits"),
    }
}

fn controlled_http_context(
    target: &HttpRequestTarget,
    limits: AdaptiveTransportLimits,
) -> AdaptiveHttpContext {
    let source = SourceIdentity::new(910);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(TEST_GENERATION.value()),
        CandidateFormatIdentity::new("app-hls-startup-test").expect("candidate format identity"),
    );
    let semantic =
        SemanticIdentity::new(source, "app-hls-startup-test").expect("semantic identity");
    let component = MediaComponentIdentity::new(exact, semantic, MediaComponentRole::Muxed)
        .expect("muxed component identity");
    let request_scope =
        SecretRequestScope::from_target(target, HttpPathScope::new("/").expect("root path scope"));
    let secrets = SecretRequestContext::builder(request_scope)
        .with_headers(ValidatedHttpHeaders::new(Vec::new()).expect("empty controlled headers"))
        .build();
    let request = TransportOpenRequest::new(
        TransportProviderId::new("app-hls-startup-test").expect("transport provider"),
        component,
        target.clone(),
        MediaPresentation::Vod,
        TEST_GENERATION,
        secrets,
        RedirectPolicy::same_origin(RedirectHopLimit::new(2).expect("redirect hop limit")),
        CancellationToken::new(),
    )
    .expect("controlled transport request");
    let source_config =
        SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("source config");
    AdaptiveHttpContext::new(
        request,
        &source_config,
        limits,
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(1).expect("single deterministic request attempt"),
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(1),
        )
        .expect("deterministic retry policy"),
    )
    .expect("controlled adaptive HTTP context")
}

#[test]
fn valid_native_restore_crosses_hls_app_player_preparation_without_second_seek() {
    let requested_position = Duration::from_secs(355);
    // Native HLS VOD starts from the containing decode-safe RAP; player hides preroll to target.
    let containing_segment =
        muxed_restore_segment(Duration::from_secs(350), Duration::from_millis(355_040));
    let following_segment =
        muxed_restore_segment(Duration::from_secs(360), Duration::from_millis(360_040));
    let manifest = restore_manifest();
    let containing_segment_bytes = containing_segment.len();
    let following_segment_bytes = following_segment.len();
    let server = ControlledHlsServer::start(HashMap::from([
        ("/restore.m3u8".to_owned(), manifest),
        ("/containing.ts".to_owned(), containing_segment),
        ("/target.ts".to_owned(), following_segment),
    ]));

    let prepared_hls = prepare_native_hls_vod(
        native_hls_request(&server, "/restore.m3u8"),
        HlsVodStartIntent::RestoreOrBeginning(MediaTime::from_duration(requested_position)),
    )
    .unwrap_or_else(|error| {
        panic!(
            "valid native HLS restore preparation: {error:#}; requests: containing={}, previous={}, target={}",
            server.request_count("/containing.ts"),
            server.request_count("/unused-34.ts"),
            server.request_count("/target.ts")
        )
    });
    let receipt_probe = Arc::clone(&prepared_hls.seek_port);
    let PreparedInitialPosition::PositionedAt {
        target_position,
        result,
        landing_policy,
    } = prepared_hls.initial_position
    else {
        panic!("valid native restore must carry authoritative prepared position");
    };
    assert_eq!(target_position.as_duration(), requested_position);
    assert_eq!(result.requested_position.as_duration(), requested_position);
    assert!(result.actual_position.as_duration() <= requested_position);
    assert_eq!(
        landing_policy,
        PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget
    );
    assert_eq!(receipt_probe.poll_seek_receipt(), None);

    let prepared_media = prepare_native_hls_player_media("controlled native HLS", prepared_hls)
        .expect("production native-HLS to PreparedMedia mapping");
    assert_eq!(prepared_media.duration(), Some(Duration::from_secs(370)));
    assert_eq!(
        prepared_position_restore_strategy(
            prepared_media.prepared_initial_position(),
            StartupPosition::Restore(requested_position),
        )
        .expect("exact prepared target must be adoptable"),
        PreparedPositionRestoreStrategy::AdoptPreparedInitialPosition
    );
    assert_eq!(receipt_probe.poll_seek_receipt(), None);
    assert_eq!(server.request_count("/restore.m3u8"), 1);
    assert_eq!(server.request_count("/target.ts"), 1);
    assert_eq!(server.request_count("/unused-0.ts"), 0);
    assert_eq!(server.request_count("/containing.ts"), 1);
    assert_eq!(
        server.served_body_bytes("/containing.ts"),
        containing_segment_bytes
    );
    assert_eq!(
        server.served_body_bytes("/target.ts"),
        following_segment_bytes
    );
}

#[test]
fn stale_native_restore_keeps_beginning_candidate_and_routes_typed_warning_without_seek() {
    let requested_position = Duration::from_secs(355);
    let beginning_segment = muxed_restore_segment(Duration::ZERO, Duration::from_millis(40));
    let beginning_segment_bytes = beginning_segment.len();
    let manifest = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nbeginning.ts\n#EXT-X-ENDLIST\n".to_vec();
    let server = ControlledHlsServer::start(HashMap::from([
        ("/stale.m3u8".to_owned(), manifest),
        ("/beginning.ts".to_owned(), beginning_segment),
    ]));

    let prepared_hls = prepare_native_hls_vod(
        native_hls_request(&server, "/stale.m3u8"),
        HlsVodStartIntent::RestoreOrBeginning(MediaTime::from_duration(requested_position)),
    )
    .expect("stale checkpoint must prepare the same finite VOD from beginning");
    let receipt_probe = Arc::clone(&prepared_hls.seek_port);
    assert_eq!(
        prepared_hls.initial_position,
        PreparedInitialPosition::Beginning
    );
    assert_eq!(receipt_probe.poll_seek_receipt(), None);

    let prepared_media = prepare_native_hls_player_media("controlled stale HLS", prepared_hls)
        .expect("production stale-HLS to PreparedMedia mapping");
    assert_eq!(prepared_media.duration(), Some(Duration::from_secs(10)));
    let strategy = prepared_position_restore_strategy(
        prepared_media.prepared_initial_position(),
        StartupPosition::Restore(requested_position),
    )
    .expect("beginning fallback keeps the ordinary post-install strategy");
    assert_eq!(strategy, PreparedPositionRestoreStrategy::SeekAfterInstall);
    let position_plan = plan_post_installed_startup_position(
        StartupPosition::Restore(requested_position),
        strategy,
        PostInstalledStartupPositionFacts::static_seekable(
            prepared_media.duration(),
            Duration::ZERO,
        ),
    );
    assert_eq!(
        position_plan,
        PostInstalledStartupPositionPlan::Continue {
            warning: Some(ResumePositionWarning {
                requested_position,
                available_position: Duration::ZERO,
            }),
        },
        "Continue сохраняет installed/current candidate и не создаёт player/demux seek"
    );
    assert_eq!(receipt_probe.poll_seek_receipt(), None);
    assert_eq!(server.request_count("/stale.m3u8"), 1);
    assert_eq!(server.request_count("/beginning.ts"), 1);
    assert_eq!(
        server.served_body_bytes("/beginning.ts"),
        beginning_segment_bytes
    );
}

fn restore_manifest() -> Vec<u8> {
    let mut manifest = String::from("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n");
    for segment_index in 0..35 {
        manifest.push_str(&format!("#EXTINF:10,\nunused-{segment_index}.ts\n"));
    }
    manifest.push_str("#EXTINF:10,\ncontaining.ts\n#EXTINF:10,\ntarget.ts\n#EXT-X-ENDLIST\n");
    manifest.into_bytes()
}

fn muxed_restore_segment(anchor: Duration, target: Duration) -> Vec<u8> {
    let anchor_pts = duration_to_90khz(anchor);
    let target_pts = duration_to_90khz(target);
    let h264_random_access_unit = [
        0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x01, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00,
        0x00, 0x01, 0x65, 0x80,
    ];
    let h264_following_unit = [0x00, 0x00, 0x01, 0x41, 0x80];
    TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)])
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID), (0x0f, AUDIO_PID)])
        .pes(AUDIO_PID, anchor_pts, None, &adts_frame(&[0x11, 0x22]))
        .pes(
            VIDEO_PID,
            anchor_pts,
            Some(anchor_pts.saturating_sub(3_000)),
            &h264_random_access_unit,
        )
        .pes(AUDIO_PID, target_pts, None, &adts_frame(&[0x33, 0x44]))
        .pes(VIDEO_PID, target_pts, None, &h264_following_unit)
        .pes(
            VIDEO_PID,
            target_pts.saturating_add(3_000),
            None,
            &h264_following_unit,
        )
        .pes(
            AUDIO_PID,
            target_pts.saturating_add(3_000),
            None,
            &adts_frame(&[0x55, 0x66]),
        )
        .finish()
}

fn duration_to_90khz(duration: Duration) -> u64 {
    duration
        .as_micros()
        .saturating_mul(90_000)
        .checked_div(1_000_000)
        .and_then(|ticks| u64::try_from(ticks).ok())
        .expect("fixture timestamp fits MPEG 90 kHz clock")
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
        let mut section = vec![0x00, 0xb0, 0x00, 0x00, 0x01, 0xc1, 0x00, 0x00];
        for &(program_number, pmt_pid) in programs {
            section.extend_from_slice(&program_number.to_be_bytes());
            section.push(0xe0 | ((pmt_pid >> 8) as u8 & 0x1f));
            section.push(pmt_pid as u8);
        }
        finalize_section(&mut section);
        let mut payload = vec![0];
        payload.extend(section);
        self.push_payload(0, true, &payload);
        self
    }

    fn pmt(mut self, pmt_pid: u16, program: u16, streams: &[(u8, u16)]) -> Self {
        let pcr_pid = streams.first().map_or(0x1fff, |stream| stream.1);
        let mut section = vec![
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
            section.extend_from_slice(&[
                stream_type,
                0xe0 | ((pid >> 8) as u8 & 0x1f),
                pid as u8,
                0xf0,
                0x00,
            ]);
        }
        finalize_section(&mut section);
        let mut payload = vec![0];
        payload.extend(section);
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
        for (packet_index, chunk) in pes.chunks(184).enumerate() {
            self.push_payload(pid, packet_index == 0, chunk);
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
