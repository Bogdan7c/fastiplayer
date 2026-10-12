//! A13-4: packed audio HLS (`.aac` ADTS и `.mp3` с ID3 `PRIV transportStreamTimestamp`).
//!
//! Тесты идут production-путём: native admission top media playlist -> `prepare_hls_*` ->
//! демуксер -> аудио-пакеты с PTS. Фикстуры `tests/fixtures/packed_audio/*` - настоящие кадры
//! ffmpeg (AAC-LC 22.05 кГц и MP3 44.1 кГц, по ~2 с на сегмент) с ID3v2.4 `PRIV`-тегом,
//! стартующим с 10 с (900000 тактов): так видно, что абсолютный PTS из тега не ломает таймлайн.

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

#[allow(dead_code)] // Shared fixture экспортирует больше builders, чем нужно этому focused binary.
mod support;

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aes::Aes128;
use cbc::{
    Encryptor,
    cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7},
};
use demux_api::{ProgressiveAsyncSeekLimits, ProgressiveSeekFence, ProgressiveSeekRequestId};
use hls_playlist_core::HlsParserLimits;
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, Demuxer, DynamicMediaTimelineEpoch,
    DynamicMediaTimelinePortGeneration, Packet, TrackKind,
};
use source_core::{CancellationToken, HttpRequestTarget};
use support::{TestQueries, TestServer, adaptive_context, demux_registry, open_policy, response};
use web_media_core::{
    AudioTrackDescriptor, CandidateFormatIdentity, CandidateIdentity, CodecFamily,
    ComponentVariantCatalogGeneration, ComponentVariantCatalogIdentity,
    ComponentVariantCatalogLimit, ComponentVariantEdgeLimit, ExactSelectionIdentity,
    ExtractionGeneration, NormalizedCodec, PreferredHeightPolicy, RawCodecIdentity,
    SemanticIdentity, SourceIdentity, VideoTrackDescriptor,
};
use web_media_hls::{
    HlsCatalogBuildPolicy, HlsCatalogCapabilityProofPort, HlsCatalogCapabilityRejection,
    HlsCatalogDiscoveryOutcome, HlsCatalogDiscoveryRequest, HlsCatalogPresentation,
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsLiveOpenRequest, HlsManifestInput, HlsProviderDefaultAudioPolicy,
    HlsRequestOverrides, HlsVodOpenRequest, NativeHlsCatalogAdmission, NativeHlsSelectionPolicy,
    SecretInlineMediaPlaylist, admit_native_hls_catalog, discover_hls_catalog,
    prepare_hls_catalog_vod_receipted, prepare_hls_live, prepare_hls_vod_receipted,
};
use web_media_transport_api::SourceGeneration;

const TEST_TIMEOUT: Duration = Duration::from_secs(4);

const AAC_SEGMENTS: [&[u8]; 3] = [
    include_bytes!("fixtures/packed_audio/aac0.aac"),
    include_bytes!("fixtures/packed_audio/aac1.aac"),
    include_bytes!("fixtures/packed_audio/aac2.aac"),
];
const MP3_SEGMENTS: [&[u8]; 3] = [
    include_bytes!("fixtures/packed_audio/mp30.mp3"),
    include_bytes!("fixtures/packed_audio/mp31.mp3"),
    include_bytes!("fixtures/packed_audio/mp32.mp3"),
];
/// Число кадров в сегментах фикстур (посчитано разбором заголовков при генерации).
const AAC_FRAMES: [usize; 3] = [44, 43, 43];
const MP3_FRAMES: [usize; 3] = [78, 78, 77];
/// Длительность кадра: 1024 сэмпла при 22.05 кГц и 1152 при 44.1 кГц.
const AAC_FRAME: Duration = Duration::from_nanos(46_439_909);
const MP3_FRAME: Duration = Duration::from_nanos(26_122_449);

const AAC_VOD: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:3\n\
    #EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:2.043356,\nseg0.aac\n#EXTINF:1.996916,\nseg1.aac\n\
    #EXTINF:1.996916,\nseg2.aac\n#EXT-X-ENDLIST\n";
const MP3_VOD: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:3\n\
    #EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:2.037551,\nseg0.mp3\n#EXTINF:2.037551,\nseg1.mp3\n\
    #EXTINF:2.011429,\nseg2.mp3\n#EXT-X-ENDLIST\n";

fn policy() -> NativeHlsSelectionPolicy {
    NativeHlsSelectionPolicy::new(
        PreferredHeightPolicy::NoPreference,
        vec![CodecFamily::H264, CodecFamily::Vp9, CodecFamily::Av1],
    )
    .expect("valid policy")
}

fn admit(playlist: &str, target: &HttpRequestTarget) -> NativeHlsCatalogAdmission {
    admit_native_hls_catalog(
        playlist.as_bytes(),
        target,
        HlsParserLimits::default(),
        &policy(),
    )
    .expect("packed audio media playlist must pass native admission")
}

fn open_request(
    target: HttpRequestTarget,
    manifest: HlsManifestInput,
    admission: &NativeHlsCatalogAdmission,
) -> HlsVodOpenRequest {
    let generation = SourceGeneration::new(1);
    HlsVodOpenRequest {
        http: adaptive_context(
            &target,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ),
        generation,
        manifest,
        selection: admission.runtime_intent(),
        overrides: HlsRequestOverrides::new(None),
        containers: admission.container_intent(),
        demux_registry: demux_registry(),
        policy: open_policy(),
    }
}

fn inline_vod_request(
    server: &TestServer,
    playlist: &'static str,
    admission: &NativeHlsCatalogAdmission,
) -> HlsVodOpenRequest {
    let target = server.target("/media.m3u8");
    open_request(
        target.clone(),
        HlsManifestInput::InlineMedia {
            selected_url: target,
            playlist: SecretInlineMediaPlaylist::new(playlist),
        },
        admission,
    )
}

fn next_ready(demuxer: &mut dyn Demuxer) -> DemuxReadEvent {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match demuxer.next_event().expect("HLS runtime reads") {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(Instant::now() < deadline, "The HLS worker is not ready.");
                std::thread::sleep(Duration::from_millis(2));
            }
            event => return event,
        }
    }
}

/// Читает VOD до конца; возвращает все аудио-пакеты и виды опубликованных дорожек.
fn read_all_packets(demuxer: &mut dyn Demuxer) -> (Vec<Packet>, Vec<TrackKind>) {
    let mut packets = Vec::new();
    let mut track_kinds = Vec::new();
    loop {
        match next_ready(demuxer) {
            DemuxReadEvent::Packet(packet) => packets.push(packet),
            DemuxReadEvent::TracksChanged(update) => {
                track_kinds = update.tracks.iter().map(|track| track.kind).collect();
            }
            DemuxReadEvent::EndOfStream => return (packets, track_kinds),
            _ => {}
        }
    }
}

/// Сервер отдаёт сегменты `seg{N}.{extension}` из набора фикстур.
fn serve_segments(segments: [Vec<u8>; 3], extension: &'static str) -> TestServer {
    TestServer::start(move |_, request| {
        for (index, segment) in segments.iter().enumerate() {
            if request
                .request_line
                .starts_with(&format!("GET /seg{index}.{extension} "))
            {
                return response("200 OK", &[], segment);
            }
        }
        response("404 Not Found", &[], b"")
    })
}

/// Убирает ID3-тег (первые `10 + size` байт) - сегмент «без ID3 PRIV».
fn strip_id3(segment: &[u8]) -> Vec<u8> {
    assert_eq!(&segment[..3], b"ID3", "Fixture starts with ID3.");
    let size = segment[6..10]
        .iter()
        .fold(0_usize, |acc, byte| (acc << 7) | usize::from(byte & 0x7f));
    segment[10 + size..].to_vec()
}

fn owned(segments: [&[u8]; 3]) -> [Vec<u8>; 3] {
    segments.map(<[u8]>::to_vec)
}

/// Общая проверка VOD-потока: только аудио, все кадры дошли, PTS строго растут, стартуют с 0 и
/// стыкуются между сегментами (шаг не больше кадра + допуск на округление EXTINF/тактов).
fn assert_stitched_audio_stream(
    packets: &[Packet],
    track_kinds: &[TrackKind],
    frames_per_segment: [usize; 3],
    frame: Duration,
) {
    assert_eq!(track_kinds, [TrackKind::Audio], "The video shouldn't be");
    assert_eq!(
        packets.len(),
        frames_per_segment.iter().sum::<usize>(),
        "Each frame of each segment became a package."
    );
    assert!(packets.iter().all(|packet| packet.kind == TrackKind::Audio));
    assert_eq!(
        packets[0].pts,
        Duration::ZERO,
        "Timeline starts from scratch"
    );
    let tolerance = Duration::from_millis(3);
    for pair in packets.windows(2) {
        assert!(pair[1].pts > pair[0].pts, "PTS should be growing strongly.");
        let step = pair[1].pts - pair[0].pts;
        assert!(
            step <= frame + tolerance,
            "Hole larger than frame between packets: {step:?} (by {:?})",
            pair[1].pts
        );
    }
    let last = packets.last().expect("packets are");
    let expected_last_start = frame * (frames_per_segment.iter().sum::<usize>() as u32 - 1);
    let drift = last.pts.abs_diff(expected_last_start);
    assert!(
        drift <= Duration::from_millis(20),
        "last frame {:?}, expected {expected_last_start:?}",
        last.pts
    );
}

#[test]
fn packed_aac_vod_with_id3_priv_plays_all_frames_with_stitched_pts() {
    let server = serve_segments(owned(AAC_SEGMENTS), "aac");
    let admission = admit(AAC_VOD, &server.target("/media.m3u8"));
    let opened = prepare_hls_vod_receipted(
        inline_vod_request(&server, AAC_VOD, &admission),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("packed AAC VOD should open");
    assert_eq!(opened.duration(), Duration::from_micros(6_037_188));
    let mut demuxer = opened.into_demuxer();

    let (packets, track_kinds) = read_all_packets(demuxer.as_mut());

    assert_stitched_audio_stream(&packets, &track_kinds, AAC_FRAMES, AAC_FRAME);
}

#[test]
fn packed_mp3_vod_with_id3_priv_plays_all_frames_with_stitched_pts() {
    let server = serve_segments(owned(MP3_SEGMENTS), "mp3");
    let admission = admit(MP3_VOD, &server.target("/media.m3u8"));
    let opened = prepare_hls_vod_receipted(
        inline_vod_request(&server, MP3_VOD, &admission),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("packed MP3 VOD should open");
    let mut demuxer = opened.into_demuxer();

    let (packets, track_kinds) = read_all_packets(demuxer.as_mut());

    assert_stitched_audio_stream(&packets, &track_kinds, MP3_FRAMES, MP3_FRAME);
}

/// Без ID3 PRIV начало сегмента берётся из суммы EXTINF: поток всё равно сплошной.
#[test]
fn packed_aac_vod_without_id3_uses_extinf_timeline() {
    let stripped = AAC_SEGMENTS.map(strip_id3);
    let server = serve_segments(stripped, "aac");
    let admission = admit(AAC_VOD, &server.target("/media.m3u8"));
    let opened = prepare_hls_vod_receipted(
        inline_vod_request(&server, AAC_VOD, &admission),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("packed AAC without ID3 must open");
    let mut demuxer = opened.into_demuxer();

    let (packets, track_kinds) = read_all_packets(demuxer.as_mut());

    assert_stitched_audio_stream(&packets, &track_kinds, AAC_FRAMES, AAC_FRAME);
}

/// Перемотка VOD: цель во втором сегменте, первый пакет после seek - не раньше точки посадки.
#[test]
fn packed_aac_vod_seek_lands_in_target_segment() {
    let server = serve_segments(owned(AAC_SEGMENTS), "aac");
    let admission = admit(AAC_VOD, &server.target("/media.m3u8"));
    let opened = prepare_hls_vod_receipted(
        inline_vod_request(&server, AAC_VOD, &admission),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("packed AAC VOD should open");
    let seek_handle = opened.async_seek_handle().expect("receipted seek handle");
    let mut demuxer = opened.into_demuxer();
    // Дочитываем до первого пакета, чтобы runtime был «играющим».
    while !matches!(next_ready(demuxer.as_mut()), DemuxReadEvent::Packet(_)) {}

    seek_handle
        .enqueue(
            ProgressiveSeekFence {
                runtime_generation: seek_handle.runtime_generation(),
                request_id: ProgressiveSeekRequestId::new(1),
            },
            DemuxSeekRequest::decode_point_before(Duration::from_secs(3)),
        )
        .expect("enqueue seek");
    let deadline = Instant::now() + TEST_TIMEOUT;
    let receipt = loop {
        if let Some(receipt) = seek_handle.poll_receipt() {
            break receipt;
        }
        assert!(Instant::now() < deadline, "seek receipt He didn't come");
        std::thread::sleep(Duration::from_millis(2));
    };
    let demux_api::ProgressiveAsyncSeekOutcome::Succeeded(result) = receipt.outcome else {
        panic!("seek packed AAC VOD must succeed: {receipt:?}");
    };
    let landed = result.actual_position.as_duration();
    assert!(
        landed >= Duration::from_millis(2_043) && landed <= Duration::from_secs(3),
        "seek Sit down the second segment no later than the target, got {landed:?}"
    );
    loop {
        if let DemuxReadEvent::Packet(packet) = next_ready(demuxer.as_mut()) {
            assert_eq!(packet.kind, TrackKind::Audio);
            assert!(
                packet.pts >= Duration::from_millis(2_043),
                "after seek, the first packet is no earlier than the beginning of the second segment: {:?}",
                packet.pts
            );
            break;
        }
    }
}

/// AES-128: расшифрованный packed-сегмент нормализуется так же, как открытый.
#[test]
fn encrypted_packed_aac_vod_decrypts_then_plays() {
    const KEY: [u8; 16] = [0x42; 16];
    const IV: [u8; 16] = [0x24; 16];
    let encrypt = |segment: &[u8]| {
        let mut buffer = vec![0_u8; segment.len() + 16];
        buffer[..segment.len()].copy_from_slice(segment);
        let length = Encryptor::<Aes128>::new((&KEY).into(), (&IV).into())
            .encrypt_padded::<Pkcs7>(&mut buffer, segment.len())
            .expect("encrypt fixture")
            .len();
        buffer.truncate(length);
        buffer
    };
    let encrypted = AAC_SEGMENTS.map(encrypt);
    let server = TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /key.bin ") {
            return response("200 OK", &[], &KEY);
        }
        for (index, segment) in encrypted.iter().enumerate() {
            if request
                .request_line
                .starts_with(&format!("GET /seg{index}.aac "))
            {
                return response("200 OK", &[], segment);
            }
        }
        response("404 Not Found", &[], b"")
    });
    const PLAYLIST: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:3\n\
        #EXT-X-PLAYLIST-TYPE:VOD\n\
        #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x24242424242424242424242424242424\n\
        #EXTINF:2.043356,\nseg0.aac\n#EXTINF:1.996916,\nseg1.aac\n\
        #EXTINF:1.996916,\nseg2.aac\n#EXT-X-ENDLIST\n";
    let admission = admit(PLAYLIST, &server.target("/media.m3u8"));
    let opened = prepare_hls_vod_receipted(
        inline_vod_request(&server, PLAYLIST, &admission),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("Encrypted packed AAC must open");
    let mut demuxer = opened.into_demuxer();

    let (packets, track_kinds) = read_all_packets(demuxer.as_mut());

    assert_stitched_audio_stream(&packets, &track_kinds, AAC_FRAMES, AAC_FRAME);
}

/// AC-3 packed честно не поддержан: открытие заканчивается ошибкой, а не тишиной.
#[test]
fn packed_ac3_is_refused_instead_of_silently_opening() {
    let ac3 = vec![0x0b, 0x77, 0x00, 0x00, 0x1c, 0x40, 0x00, 0x00, 0x00, 0x00];
    let server = TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /seg0.ac3 ") {
            response("200 OK", &[], &ac3)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    const PLAYLIST: &str = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n\
        #EXTINF:2,\nseg0.ac3\n#EXT-X-ENDLIST\n";
    let admission = admit(PLAYLIST, &server.target("/media.m3u8"));

    let result = prepare_hls_vod_receipted(
        inline_vod_request(&server, PLAYLIST, &admission),
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    );

    assert!(result.is_err(), "AC-3 packed should not be opened.");
}

struct NoRefresh;
impl HlsEndpointRefreshPort for NoRefresh {
    fn refresh(
        &self,
        _request: HlsEndpointRefreshRequest,
    ) -> Result<HlsEndpointRefreshReply, HlsEndpointRefreshError> {
        Err(HlsEndpointRefreshError::Cancelled)
    }
}

const LIVE_FIRST_WINDOW: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:3\n\
    #EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:2.043356,\nseg0.aac\n#EXTINF:1.996916,\nseg1.aac\n";
const LIVE_SLID_WINDOW: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:3\n\
    #EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:1.996916,\nseg1.aac\n#EXTINF:1.996916,\nseg2.aac\n";

/// Live packed AAC со скользящим окном: открывается, отдаёт аудио-пакеты, DVR-диапазон строится
/// из аудио-якорей и сдвигается при обновлении плейлиста; пакеты идут и из сдвинутого окна.
#[test]
fn packed_aac_live_sliding_window_advances_and_keeps_delivering_audio() {
    let manifest_requests = Arc::new(AtomicUsize::new(0));
    let handler_manifest_requests = Arc::clone(&manifest_requests);
    let server = TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /live.m3u8 ") {
            let count = handler_manifest_requests.fetch_add(1, Ordering::SeqCst);
            let playlist = if count == 0 {
                LIVE_FIRST_WINDOW
            } else {
                LIVE_SLID_WINDOW
            };
            return response("200 OK", &[], playlist.as_bytes());
        }
        for (index, segment) in AAC_SEGMENTS.iter().enumerate() {
            if request
                .request_line
                .starts_with(&format!("GET /seg{index}.aac "))
            {
                return response("200 OK", &[], segment);
            }
        }
        response("404 Not Found", &[], b"")
    });
    let target = server.target("/live.m3u8");
    let admission = admit(LIVE_FIRST_WINDOW, &target);
    let opened = prepare_hls_live(HlsLiveOpenRequest {
        common: open_request(
            target.clone(),
            HlsManifestInput::Fetch {
                selected_url: target,
            },
            &admission,
        ),
        endpoint_refresh: Arc::new(NoRefresh),
        timeline_port_generation: DynamicMediaTimelinePortGeneration::new(
            NonZeroU64::new(1).expect("non-zero port generation"),
        ),
        initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
    })
    .expect("packed AAC live should open");
    let (mut demuxer, timeline_port, _) = opened.into_parts();
    let track_kinds: Vec<TrackKind> = demuxer.tracks().iter().map(|track| track.kind).collect();
    assert_eq!(track_kinds, vec![TrackKind::Audio]);

    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut audio_packets = 0_usize;
    let mut last_pts = Duration::ZERO;
    let slid_range = loop {
        let observed = timeline_port.observe();
        if let Some(range) = observed.snapshot.state.seekable_range()
            && manifest_requests.load(Ordering::SeqCst) >= 2
            && range.start.as_duration() >= Duration::from_secs(2)
        {
            break range;
        }
        assert!(
            Instant::now() < deadline,
            "DVR packed live band not advanced: {:?}",
            observed.snapshot.state.seekable_range()
        );
        if let DemuxReadEvent::Packet(packet) = demuxer.next_event().expect("live read") {
            assert_eq!(packet.kind, TrackKind::Audio);
            assert!(packet.pts >= last_pts, "PTS live doesn't go backwards.");
            last_pts = packet.pts;
            audio_packets += 1;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(slid_range.end > slid_range.start);
    assert!(
        audio_packets > AAC_FRAMES[0],
        "must reach at least the first segment, received {audio_packets}"
    );
}

/// Capability-порт радио: знает только звук AAC; видео в проверяемом child быть не должно.
struct AudioOnlyCapabilities;

impl HlsCatalogCapabilityProofPort for AudioOnlyCapabilities {
    fn prove_video(
        &mut self,
        _track: &media_core::TrackInfo,
    ) -> Result<VideoTrackDescriptor, HlsCatalogCapabilityRejection> {
        panic!("An audio-only child should not have a video track.");
    }

    fn prove_audio(
        &mut self,
        track: &media_core::TrackInfo,
    ) -> Result<AudioTrackDescriptor, HlsCatalogCapabilityRejection> {
        assert_eq!(track.kind, TrackKind::Audio);
        Ok(AudioTrackDescriptor::new(
            NormalizedCodec::parse(RawCodecIdentity::new("mp4a.40.2").expect("audio codec")),
            None,
            None,
            None,
            None,
        ))
    }
}

/// Master радио с packed-сегментами: каталог зондирует child по содержимому `.aac`, а не по
/// расширению, и воспроизведение доходит до аудио-пакетов выбранного варианта.
#[test]
fn packed_aac_radio_master_is_discovered_and_plays_through_catalog() {
    const MASTER: &str = "#EXTM3U\n\
        #EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.2\"\nlow.m3u8\n\
        #EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"mp4a.40.2\"\nhigh.m3u8\n";
    let server = TestServer::start(move |_, request| {
        let child = |name: &str| AAC_VOD.replace("seg", name);
        if request.request_line.starts_with("GET /master.m3u8 ") {
            return response("200 OK", &[], MASTER.as_bytes());
        }
        for name in ["high", "low"] {
            if request
                .request_line
                .starts_with(&format!("GET /{name}.m3u8 "))
            {
                return response("200 OK", &[], child(name).as_bytes());
            }
            for (index, segment) in AAC_SEGMENTS.iter().enumerate() {
                if request
                    .request_line
                    .starts_with(&format!("GET /{name}{index}.aac "))
                {
                    return response("200 OK", &[], segment);
                }
            }
        }
        response("404 Not Found", &[], b"")
    });
    let target = server.target("/master.m3u8");
    let admission = admit(MASTER, &target);
    let open = open_request(
        target.clone(),
        HlsManifestInput::Fetch {
            selected_url: target,
        },
        &admission,
    );
    let source = SourceIdentity::new(134);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(1),
        CandidateFormatIdentity::new("hls-packed-radio").expect("format identity"),
    );
    let semantic = SemanticIdentity::new(source, "hls-packed-radio").expect("semantic identity");
    let outcome = discover_hls_catalog(
        HlsCatalogDiscoveryRequest {
            open: &open,
            catalog_identity: ComponentVariantCatalogIdentity::new(
                ExactSelectionIdentity::new(exact, semantic).expect("same source"),
                ComponentVariantCatalogGeneration::new(1),
            ),
            presentation: HlsCatalogPresentation::Vod,
            provider_default_variant_index: admission.current_master_variant_index(),
            policy: HlsCatalogBuildPolicy {
                catalog_limit: ComponentVariantCatalogLimit::new(8).expect("catalog limit"),
                compatibility_edge_limit: ComponentVariantEdgeLimit::new(8).expect("edge limit"),
                maximum_unique_children: NonZeroUsize::new(8).expect("child limit"),
                provider_default_audio: HlsProviderDefaultAudioPolicy::RequireDeclared,
            },
        },
        &mut AudioOnlyCapabilities,
    )
    .expect("packed audio master must pass discovery");
    let HlsCatalogDiscoveryOutcome::Installed(snapshot) = outcome else {
        panic!("master discovery must install a catalog");
    };
    let reopen = snapshot
        .reopen_exact(snapshot.provider_default_selection())
        .expect("provider default audio-only");
    let opened = prepare_hls_catalog_vod_receipted(
        open,
        reopen,
        ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(2).expect("seek receipt bound")),
    )
    .expect("packed audio master child should open");
    let mut demuxer = opened.into_demuxer();

    let (packets, track_kinds) = read_all_packets(demuxer.as_mut());

    assert_stitched_audio_stream(&packets, &track_kinds, AAC_FRAMES, AAC_FRAME);
}
