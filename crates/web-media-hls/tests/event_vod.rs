//! A13-1: завершённый `EXT-X-PLAYLIST-TYPE:EVENT` + `EXT-X-ENDLIST` открывается как обычный VOD.
//!
//! Манифесты ниже - дословный вывод `ffmpeg -f hls -hls_time 2 -hls_playlist_type event`
//! (H.264 + AAC, 6 секунд, три сегмента) после завершения записи. Байты сегментов - маленькие
//! fixture-ы харнесса (`support`): для проверки важны манифест и путь VOD-runtime, а не размер файла.

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

use std::sync::Arc;
use std::time::{Duration, Instant};

use hls_playlist_core::HlsParserLimits;
use media_core::{DemuxReadEvent, Demuxer, TrackKind};
use source_core::CancellationToken;
use support::{
    TestQueries, TestServer, adaptive_context, demux_registry, muxed_fmp4, muxed_ts, open_policy,
    response,
};
use web_media_adaptive::{
    AdaptiveResourceFetchRequest, AdaptiveResourcePurpose, AdaptiveResourceQueryApplication,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsCatalogPresentation, HlsComponentContainerIntent,
    HlsContainerEvidence, HlsMainTrackLayoutIntent, HlsManifestInput, HlsRequestOverrides,
    HlsRequiredContainer, HlsVariantSelectionIntent, HlsVodOpenError, HlsVodOpenRequest,
    detect_hls_catalog_presentation, prepare_hls_vod,
};
use web_media_transport_api::SourceGeneration;

const TEST_TIMEOUT: Duration = Duration::from_secs(3);

/// ffmpeg MPEG-TS, завершённый EVENT (3 x 2 с).
const FFMPEG_EVENT_TS: &[u8] = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n\
#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:EVENT\n\
#EXTINF:2.000000,\nseg0.ts\n#EXTINF:2.000000,\nseg1.ts\n#EXTINF:2.000000,\nseg2.ts\n\
#EXT-X-ENDLIST\n";

/// ffmpeg fMP4 (`-hls_segment_type fmp4`), завершённый EVENT (3 x 2 с).
const FFMPEG_EVENT_FMP4: &[u8] = b"#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n\
#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXT-X-MAP:URI=\"init.mp4\"\n\
#EXTINF:2.000000,\nseg0.m4s\n#EXTINF:2.000000,\nseg1.m4s\n#EXTINF:2.000000,\nseg2.m4s\n\
#EXT-X-ENDLIST\n";

fn muxed_selection() -> HlsVariantSelectionIntent {
    HlsVariantSelectionIntent {
        resolution: None,
        codecs: None,
        audio: HlsAudioLayoutIntent::Muxed,
        main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
    }
}

fn open_request(
    server: &TestServer,
    manifest_path: &str,
    container: HlsRequiredContainer,
) -> HlsVodOpenRequest {
    let generation = SourceGeneration::new(1);
    let target = server.target(manifest_path);
    HlsVodOpenRequest {
        http: adaptive_context(
            &target,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ),
        generation,
        manifest: HlsManifestInput::Fetch {
            selected_url: target,
        },
        selection: muxed_selection(),
        overrides: HlsRequestOverrides::new(None),
        containers: HlsComponentContainerIntent {
            main: HlsContainerEvidence::Exact(container),
            alternate_audio: None,
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    }
}

/// Ждёт первый настоящий пакет заданного вида; `TemporarilyUnavailable` - обычное состояние worker-а.
fn first_packet_kinds(demuxer: &mut dyn Demuxer) -> Vec<TrackKind> {
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut kinds = Vec::new();
    while kinds.len() < 2 {
        match demuxer.next_event().expect("VOD runtime reads") {
            DemuxReadEvent::Packet(packet) => kinds.push(packet.kind),
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(Instant::now() < deadline, "The HLS worker is not ready.");
                std::thread::sleep(Duration::from_millis(2));
            }
            DemuxReadEvent::EndOfStream => break,
            _ => {}
        }
    }
    kinds
}

fn assert_event_opens_as_vod_with_packets(
    manifest: &'static [u8],
    container: HlsRequiredContainer,
    segment_extension: &str,
) {
    let (initialization, media) = match container {
        HlsRequiredContainer::FragmentedMp4 => {
            let (init, first, _) = muxed_fmp4();
            (init, first)
        }
        _ => (Vec::new(), muxed_ts(90_000)),
    };
    let initialization = Arc::new(initialization);
    let media = Arc::new(media);
    let segment_suffix = segment_extension.to_owned();
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/media.m3u8") {
            response("200 OK", &[], manifest)
        } else if request.request_line.contains("/init.mp4") {
            response("200 OK", &[], &initialization)
        } else if request.request_line.contains(&segment_suffix) {
            response("200 OK", &[], &media)
        } else {
            response("404 Not Found", &[], b"")
        }
    });

    let opened = prepare_hls_vod(open_request(&server, "/media.m3u8", container))
        .expect("The completed EVENT should open as a VOD.");
    // Длительность - сумма EXTINF (3 x 2 с), т.е. плеер видит конечный перематываемый ролик.
    assert_eq!(opened.duration(), Duration::from_secs(6));
    let mut demuxer = opened.into_demuxer();
    let kinds = first_packet_kinds(demuxer.as_mut());
    assert!(
        kinds.contains(&TrackKind::Video) && kinds.contains(&TrackKind::Audio),
        "VOD-runtime You have to give the video and audio packages, got {kinds:?}"
    );
}

#[test]
fn finished_event_ts_opens_as_vod_and_reaches_packets() {
    assert_event_opens_as_vod_with_packets(
        FFMPEG_EVENT_TS,
        HlsRequiredContainer::TransportStream,
        ".ts",
    );
}

#[test]
fn finished_event_fmp4_opens_as_vod_and_reaches_packets() {
    assert_event_opens_as_vod_with_packets(
        FFMPEG_EVENT_FMP4,
        HlsRequiredContainer::FragmentedMp4,
        ".m4s",
    );
}

/// EVENT без ENDLIST ещё пополняется, поэтому VOD-путь по-прежнему отказывает (его обслуживает live).
#[test]
fn running_event_without_endlist_is_still_rejected_by_vod_path() {
    let running: &'static [u8] = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n\
#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:EVENT\n#EXTINF:2.000000,\nseg0.ts\n";
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/media.m3u8") {
            response("200 OK", &[], running)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let error = prepare_hls_vod(open_request(
        &server,
        "/media.m3u8",
        HlsRequiredContainer::TransportStream,
    ))
    .expect_err("EVENT without ENDLIST is not a VOD");
    assert!(
        matches!(error, HlsVodOpenError::Profile(_)),
        "A profile error was expected, received {error:?}"
    );
}

/// Master с вариантом EVENT+ENDLIST: выбранный child классифицируется как VOD и профиль его принимает.
#[test]
fn master_with_finished_event_variant_is_classified_as_vod() {
    let server = TestServer::start(move |_, request| {
        if request.request_line.contains("/master.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.42001e,mp4a.40.2\",\
                  RESOLUTION=640x360\nmedia.m3u8\n",
            )
        } else if request.request_line.contains("/media.m3u8") {
            response("200 OK", &[], FFMPEG_EVENT_TS)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let generation = SourceGeneration::new(1);
    let target = server.target("/master.m3u8");
    let http = adaptive_context(
        &target,
        CancellationToken::new(),
        generation,
        TestQueries::default(),
    );
    let top = http
        .fetch_resource_blocking(
            AdaptiveResourceFetchRequest::full(
                generation,
                target.clone(),
                http.maximum_resource_bytes(AdaptiveResourcePurpose::Manifest),
                AdaptiveResourcePurpose::Manifest,
                AdaptiveResourceQueryApplication::BypassScopedQuery,
            )
            .with_secret_forwarding(http.resource_secret_forwarding_for(&target)),
        )
        .expect("master is loaded");

    let presentation = detect_hls_catalog_presentation(
        &top,
        &http,
        generation,
        &muxed_selection(),
        None,
        HlsParserLimits::default(),
    )
    .expect("child EVENT+ENDLIST passing VOD-profile");
    assert_eq!(presentation, HlsCatalogPresentation::Vod);
}
