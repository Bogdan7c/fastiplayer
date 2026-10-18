//! Live-сценарии субтитров HLS на НАСТОЯЩЕМ открытии: сегменты без `X-TIMESTAMP-MAP` и
//! стабильная шкала скользящего плейлиста (ревью сессии 13).

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic здесь работают
// как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

#[allow(dead_code)]
#[path = "../../web-media-hls/tests/support/mod.rs"]
mod support;

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use media_core::{
    DemuxReadEvent, Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelinePortGeneration,
    TrackKind,
};
use media_source_open::stream_subtitles::{
    HlsStreamSubtitleTransport, StreamPlaybackPosition, StreamSubtitleFeed,
    StreamSubtitleFeedConfig, StreamSubtitleMessage, StreamSubtitleTrackId,
    StreamSubtitleTransport,
};
use source_core::CancellationToken;
use support::{
    TestQueries, TestServer, adaptive_context, demux_registry, muxed_ts, open_policy, response,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsLiveOpenRequest, HlsMainTrackLayoutIntent, HlsManifestInput,
    HlsRequestOverrides, HlsRequiredContainer, HlsVariantSelectionIntent, prepare_hls_live,
};
use web_media_transport_api::SourceGeneration;

const WAIT: Duration = Duration::from_secs(8);
/// Допуск на округление тиков 90 кГц и микросекунд.
const TOLERANCE: Duration = Duration::from_micros(3);

fn master() -> &'static str {
    "#EXTM3U\n\
     #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",LANGUAGE=\"en\",\
     DEFAULT=YES,AUTOSELECT=YES,URI=\"subs/en.m3u8\"\n\
     #EXT-X-STREAM-INF:BANDWIDTH=1000,SUBTITLES=\"subs\"\nvideo.m3u8\n"
}

struct NoRefreshPort;

impl HlsEndpointRefreshPort for NoRefreshPort {
    fn refresh(
        &self,
        _request: HlsEndpointRefreshRequest,
    ) -> Result<HlsEndpointRefreshReply, HlsEndpointRefreshError> {
        Err(HlsEndpointRefreshError::AttemptsExhausted)
    }
}

/// Открывает live; возвращает транспорт субтитров и демуксер (его жизнь держит источник).
fn open_live(server: &TestServer) -> (HlsStreamSubtitleTransport, Box<dyn Demuxer + Send>) {
    let generation = SourceGeneration::new(1);
    let target = server.target("/master.m3u8");
    let common = web_media_hls::HlsVodOpenRequest {
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
    };
    let request = HlsLiveOpenRequest {
        common,
        endpoint_refresh: Arc::new(NoRefreshPort),
        timeline_port_generation: DynamicMediaTimelinePortGeneration::new(
            NonZeroU64::new(1).expect("non-zero test port generation"),
        ),
        initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
    };
    let opened = prepare_hls_live(request).expect("prepare live with subtitles");
    let transport =
        HlsStreamSubtitleTransport::new(opened.subtitle_transport(), opened.timestamp_origins());
    let (demuxer, _timeline, _) = opened.into_parts();
    (transport, demuxer)
}

/// Номер сегмента из строки запроса `.../subs/seg<N>.vtt`.
fn segment_number(line: &str) -> Option<u32> {
    line.split("/subs/seg")
        .nth(1)?
        .split(".vtt")
        .next()?
        .parse()
        .ok()
}

fn playlist(first_sequence: u32, count: u32) -> String {
    let mut text =
        format!("#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{first_sequence}\n");
    for number in first_sequence..first_sequence + count {
        text.push_str(&format!("#EXTINF:1,\nseg{number}.vtt\n"));
    }
    text
}

#[test]
fn live_segments_without_timestamp_map_use_container_time_not_the_epoch_start() {
    // Видео-live: два сегмента, сырые PTS 10 с и 20 с. Субтитры без X-TIMESTAMP-MAP: по
    // RFC 8216 §3.5 время реплики - время контейнера. Реплика на 10,5 с контейнера лежит у
    // первого сегмента видео (+0,5 с), на 20,5 с - у второго (+1,5 с от начала).
    let first = muxed_ts(900_000);
    let second = muxed_ts(1_800_000);
    let server = TestServer::start(move |_, request| {
        let line = &request.request_line;
        if line.contains("/master.m3u8") {
            response("200 OK", &[], master().as_bytes())
        } else if line.contains("/video.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
                  #EXTINF:1,\na.ts\n#EXTINF:1,\nb.ts\n",
            )
        } else if line.contains("/a.ts") {
            response("200 OK", &[], &first)
        } else if line.contains("/b.ts") {
            response("200 OK", &[], &second)
        } else if line.contains("/subs/en.m3u8") {
            response("200 OK", &[], playlist(0, 2).as_bytes())
        } else if let Some(number) = segment_number(line) {
            let (cue_start, cue_end) = if number == 0 {
                ("00:00:10.500", "00:00:10.900")
            } else {
                ("00:00:20.500", "00:00:20.900")
            };
            let body = format!("WEBVTT\n\n{cue_start} --> {cue_end}\ncue {number}\n");
            response("200 OK", &[], body.as_bytes())
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let (transport, mut demuxer) = open_live(&server);
    let source = transport.into_source().expect("one rendition");
    let position = StreamPlaybackPosition::new(Duration::ZERO);
    let feed = StreamSubtitleFeed::start(
        &source,
        StreamSubtitleTrackId::new(0),
        StreamSubtitleFeedConfig {
            ahead: Duration::from_secs(20),
            behind: Duration::from_secs(5),
            max_segment_bytes: NonZeroUsize::new(64 * 1_024).expect("non-zero"),
            max_pending_segments: NonZeroUsize::new(16).expect("non-zero"),
            failure_threshold: std::num::NonZeroU32::new(5).expect("non-zero"),
            outage_budget: Duration::from_secs(60),
            message_capacity: NonZeroUsize::new(64).expect("non-zero"),
        },
        position,
        Arc::new(|| {}),
    )
    .expect("feed starts");

    // Демуксируем оба сегмента видео: так публикуются пары точек отсчёта.
    let deadline = Instant::now() + WAIT;
    let mut video_pts = Vec::new();
    while video_pts.len() < 2 && Instant::now() < deadline {
        match demuxer.next_event().expect("live stays readable") {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                video_pts.push(packet.pts);
            }
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                std::thread::sleep(Duration::from_millis(2));
            }
            _ => {}
        }
    }
    assert_eq!(video_pts.len(), 2, "two video segments were demuxed");

    let mut starts = Vec::new();
    while starts.len() < 2 {
        assert!(Instant::now() < deadline, "cues did not arrive: {starts:?}");
        for message in feed.drain_messages() {
            if let StreamSubtitleMessage::Events(events) = message {
                for event in &events.events {
                    let micros = u64::try_from(event.start().as_micros()).expect("non-negative");
                    starts.push((events.segment.value(), Duration::from_micros(micros)));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    starts.sort_unstable();
    let expected = [
        (0, video_pts[0] + Duration::from_millis(500)),
        (1, video_pts[1] + Duration::from_millis(500)),
    ];
    for ((segment, actual), (_, wanted)) in starts.iter().zip(expected) {
        assert!(
            actual.abs_diff(wanted) <= TOLERANCE,
            "segment {segment}: {actual:?} differs from {wanted:?}"
        );
    }
}

#[test]
fn sliding_live_playlist_keeps_segment_starts_and_a_gap_changes_the_timeline_epoch() {
    let reloads = Arc::new(AtomicUsize::new(0));
    let reload_counter = Arc::clone(&reloads);
    let video = muxed_ts(900_000);
    let server = TestServer::start(move |_, request| {
        let line = &request.request_line;
        if line.contains("/master.m3u8") {
            response("200 OK", &[], master().as_bytes())
        } else if line.contains("/video.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:1,\na.ts\n",
            )
        } else if line.contains("/a.ts") {
            response("200 OK", &[], &video)
        } else if line.contains("/subs/en.m3u8") {
            // 4 сегмента в окне; окно скользит на 2, затем уходит далеко (разрыв).
            let body = match reload_counter.fetch_add(1, Ordering::SeqCst) {
                0 => playlist(0, 4),
                1 => playlist(2, 4),
                _ => playlist(90, 4),
            };
            response("200 OK", &[], body.as_bytes())
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let (transport, _demuxer) = open_live(&server);
    let track = StreamSubtitleTrackId::new(0);
    let cancellation = CancellationToken::new();
    let starts = |transport: &HlsStreamSubtitleTransport| {
        transport
            .load_index(track, &cancellation)
            .expect("playlist loads")
            .segments
            .iter()
            .map(|segment| (segment.key.value(), segment.start.as_secs()))
            .collect::<Vec<_>>()
    };

    assert_eq!(starts(&transport), [(0, 0), (1, 1), (2, 2), (3, 3)]);
    let epoch = transport.index_timeline_epoch(track);
    // Окно съехало: сегмент 2 по-прежнему начинается на 2 с, а не «уехал» к нулю.
    assert_eq!(starts(&transport), [(2, 2), (3, 3), (4, 4), (5, 5)]);
    assert_eq!(transport.index_timeline_epoch(track), epoch);
    // Разрыв: шкала продолжается от конца прошлого снимка, а смещения устарели.
    assert_eq!(starts(&transport), [(90, 6), (91, 7), (92, 8), (93, 9)]);
    assert_ne!(transport.index_timeline_epoch(track), epoch);
}
