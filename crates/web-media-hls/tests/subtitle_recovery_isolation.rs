//! Инвариант: сбои субтитров никогда не влияют на видео (VOD endpoint recovery не взводится).
//!
//! Реальное VOD-открытие HLS с присоединённым наблюдателем expiry (как у AUD-009), а
//! субтитровые плейлист/сегменты отвечают 404/410 много раз подряд.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

#[allow(dead_code)]
mod support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use media_core::{DemuxReadEvent, Demuxer};
use source_core::CancellationToken;
use support::{
    TestQueries, TestServer, adaptive_context_with_expiry_observer, demux_registry, muxed_ts,
    open_policy, response,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsMainTrackLayoutIntent, HlsManifestInput, HlsRequestOverrides, HlsRequiredContainer,
    HlsVariantSelectionIntent, HlsVodOpenRequest, prepare_hls_vod,
};
use web_media_transport_api::{
    EndpointExpiryObserver, EndpointExpiryResourceKind, EndpointExpirySignal, SourceGeneration,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(6);

/// Записывает все сигналы, которые получил бы AUD-009 recovery.
#[derive(Default)]
struct RecordingObserver {
    signals: Mutex<Vec<EndpointExpirySignal>>,
}

impl EndpointExpiryObserver for RecordingObserver {
    fn observe_endpoint_expiry(&self, signal: EndpointExpirySignal) {
        self.signals.lock().expect("signals mutex").push(signal);
    }
}

impl RecordingObserver {
    fn kinds(&self) -> Vec<EndpointExpiryResourceKind> {
        self.signals
            .lock()
            .expect("signals mutex")
            .iter()
            .map(EndpointExpirySignal::resource_kind)
            .collect()
    }
}

/// Мастер с видео и одной дорожкой субтитров (адрес playlist-а отдаётся по `subs_status`).
const MASTER: &str = "#EXTM3U\n\
    #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",LANGUAGE=\"en\",\
    DEFAULT=YES,URI=\"subs/en.m3u8\"\n\
    #EXT-X-STREAM-INF:BANDWIDTH=1000,SUBTITLES=\"subs\"\nvideo.m3u8\n";

/// Сервер: видео из двух TS-сегментов; второй сегмент можно «просрочить»; субтитры - 404/410.
fn start_server(second_video_segment_status: &'static str) -> TestServer {
    let first = muxed_ts(90_000);
    let second = muxed_ts(180_000);
    TestServer::start(move |_, request| {
        let line = &request.request_line;
        if line.contains("/master.m3u8") {
            response("200 OK", &[], MASTER.as_bytes())
        } else if line.contains("/video.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\na.ts\n#EXTINF:1,\nb.ts\n\
                  #EXT-X-ENDLIST\n",
            )
        } else if line.contains("/a.ts") {
            response("200 OK", &[], &first)
        } else if line.contains("/b.ts") {
            response(second_video_segment_status, &[], &second)
        } else if line.contains("/subs/en.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\ngone.vtt\n#EXTINF:2,\nmissing.vtt\n\
                  #EXT-X-ENDLIST\n",
            )
        } else if line.contains("/subs/gone.vtt") {
            response("410 Gone", &[], b"")
        } else {
            response("404 Not Found", &[], b"")
        }
    })
}

fn open(server: &TestServer, observer: &Arc<RecordingObserver>) -> web_media_hls::HlsVodOpenResult {
    let generation = SourceGeneration::new(1);
    let target = server.target("/master.m3u8");
    let observer: Arc<dyn EndpointExpiryObserver> = observer.clone();
    prepare_hls_vod(HlsVodOpenRequest {
        http: adaptive_context_with_expiry_observer(
            &target,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
            observer,
        ),
        generation,
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
        demux_registry: demux_registry(),
        policy: open_policy(),
    })
    .expect("prepare HLS VOD with subtitles")
}

/// Читает демуксер до EOF и возвращает число пакетов (ошибки чтения не скрываются паникой:
/// в контрольном тесте видео-сегмент действительно падает).
fn drain_packets(demuxer: &mut dyn Demuxer) -> usize {
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut packets = 0;
    while Instant::now() < deadline {
        match demuxer.next_event() {
            Ok(DemuxReadEvent::Packet(_)) => packets += 1,
            Ok(DemuxReadEvent::EndOfStream) | Err(_) => return packets,
            Ok(_) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    panic!("demuxer did not finish in time");
}

fn count_requests(server: &TestServer, needle: &str) -> usize {
    server
        .requests()
        .iter()
        .filter(|request| request.request_line.contains(needle))
        .count()
}

#[test]
fn repeated_subtitle_failures_never_arm_video_recovery_and_playback_continues() {
    let server = start_server("200 OK");
    let observer = Arc::new(RecordingObserver::default());
    let opened = open(&server, &observer);
    let transport = opened.subtitle_transport();
    let rendition = transport.renditions()[0].id;
    let mut demuxer = opened.into_demuxer();
    let cancellation = CancellationToken::new();

    // Много раз подряд: playlist ok, сегменты 410/404 (после встроенных повторов).
    for _ in 0..4 {
        let snapshot = transport
            .load_playlist(rendition, &cancellation)
            .expect("subtitle playlist loads");
        for segment in &snapshot.segments {
            let error = transport
                .fetch_segment(
                    &segment.reference,
                    web_media_hls::HlsSubtitleSegmentByteLimit::from_bytes(
                        std::num::NonZeroUsize::new(1024).expect("limit"),
                    ),
                    &cancellation,
                )
                .expect_err("subtitle segment must fail");
            assert!(
                matches!(
                    error,
                    web_media_hls::HlsSubtitleFetchError::NotFound
                        | web_media_hls::HlsSubtitleFetchError::Gone
                ),
                "typed error preserved: {error:?}"
            );
        }
    }

    // Видео по-прежнему играет до конца: оба сегмента дали пакеты.
    assert!(
        drain_packets(&mut *demuxer) >= 2,
        "video keeps producing packets"
    );
    assert!(
        observer.kinds().is_empty(),
        "subtitle failures must not publish endpoint expiry"
    );
    assert_eq!(
        count_requests(&server, "/master.m3u8"),
        1,
        "no root manifest refetch"
    );
    assert!(
        count_requests(&server, "/subs/") >= 8,
        "subtitle failures really happened"
    );
}

#[test]
fn control_video_segment_404_still_publishes_expiry() {
    let server = start_server("404 Not Found");
    let observer = Arc::new(RecordingObserver::default());
    let opened = open(&server, &observer);
    let mut demuxer = opened.into_demuxer();

    let _ = drain_packets(&mut *demuxer);

    assert!(
        observer
            .kinds()
            .contains(&EndpointExpiryResourceKind::MediaSegment),
        "a real video segment 404 must still reach the recovery observer"
    );
}
