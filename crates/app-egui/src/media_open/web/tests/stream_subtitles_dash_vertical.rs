//! Настоящее открытие нативного DASH с текстовыми AdaptationSet (loopback-сервер, fMP4 видео
//! с аудио, две отдельные TTML-дорожки): отдаёт тестам intent, из которого приложение берёт
//! источник субтитров потока, и время первого пакета видео на шкале плеера.
//!
//! Сквозная проверка с пикселями (`state::subtitles::tests::stream_dash_vertical`) строится
//! поверх него; здесь же - проверки самого intent-а и ленты на реальном открытии.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use media_core::{DemuxReadEvent, DemuxRetryHint, Demuxer, TrackKind};
use media_source_open::stream_subtitles::{
    StreamPlaybackPosition, StreamSubtitleFeed, StreamSubtitleFeedConfig, StreamSubtitleMessage,
    StreamSubtitleTrackId,
};

use super::super::*;
use super::native_dash_vertical::{fixture_routes, native_settings, prepare_native};
use super::native_hls_vertical::ControlledHlsServer;
use crate::media_open::{NativeDashUrl, SafeMediaLabel};

/// Обычная дорожка: реплика на 0,5-1,5 с от начала Period.
const PLAIN_TTML: &str = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div>
<p begin="00:00:00.500" end="00:00:01.500">Plain DASH cue</p></div></body></tt>"#;

/// IMSC: реплика в верхней области (`origin 10% 5%`, `extent 80% 20%`), 0,5-1,5 с.
const TOP_REGION_TTML: &str = r#"<tt xmlns="http://www.w3.org/ns/ttml"
xmlns:tts="http://www.w3.org/ns/ttml#styling" xml:lang="de">
<head><layout><region xml:id="top" tts:origin="10% 5%" tts:extent="80% 20%"
tts:displayAlign="before" tts:textAlign="center"/></layout></head>
<body><div><p begin="00:00:00.500" end="00:00:01.500" region="top">Top DASH cue</p></div></body></tt>"#;

/// MPD: видео H.264/AAC (как в `native_dash_vertical`) и две отдельные TTML-дорожки.
fn manifest() -> Vec<u8> {
    r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT2S">
      <Period id="p0" duration="PT2S">
        <AdaptationSet id="fmp4" contentType="application" mimeType="application/mp4"
            codecs="avc1.42c00a,mp4a.40.2" width="16" height="16">
          <Representation id="h264-aac" bandwidth="200000" width="16" height="16">
            <SegmentList timescale="1" duration="1">
              <Initialization sourceURL="fmp4-init.mp4"/>
              <SegmentURL media="fmp4-0.m4s"/>
              <SegmentURL media="fmp4-1.m4s"/>
            </SegmentList>
          </Representation>
        </AdaptationSet>
        <AdaptationSet contentType="text" mimeType="application/ttml+xml" lang="en">
          <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>
          <Label>English</Label>
          <Representation id="ttml-en"><BaseURL>plain.ttml</BaseURL></Representation>
        </AdaptationSet>
        <AdaptationSet contentType="text" mimeType="application/ttml+xml" lang="de">
          <Label>German</Label>
          <Representation id="ttml-de"><BaseURL>top.ttml</BaseURL></Representation>
        </AdaptationSet>
      </Period>
    </MPD>"#
        .as_bytes()
        .to_vec()
}

/// Открытый DASH-источник с субтитрами.
pub(crate) struct DashSubtitleFixture {
    /// Intent открытого media: приложение берёт из него источник субтитров потока.
    pub(crate) intent: WebMediaSourceIntent,
    /// Время первого пакета видео на шкале плеера.
    pub(crate) first_video_player_time: Duration,
    /// Сервер и демуксер живут, пока жив фикстурный объект.
    _alive: Box<dyn Any>,
}

/// Открывает DASH через настоящий `prepare_native_dash_attempt` и читает демуксер до первого
/// пакета видео (как делает плеер), затем собирает intent из того же открытия.
pub(crate) fn open_dash_with_subtitles() -> DashSubtitleFixture {
    let mut routes: HashMap<String, Vec<Vec<u8>>> = fixture_routes();
    routes.insert("/manifest.mpd".to_owned(), vec![manifest()]);
    routes.insert(
        "/plain.ttml".to_owned(),
        vec![PLAIN_TTML.as_bytes().to_vec()],
    );
    routes.insert(
        "/top.ttml".to_owned(),
        vec![TOP_REGION_TTML.as_bytes().to_vec()],
    );
    let server = ControlledHlsServer::start(routes);
    let source = NativeDashUrl::new(
        server.target("/manifest.mpd"),
        SafeMediaLabel::from_service_safe_label("stream subtitles dash vertical"),
    );
    let settings = native_settings();
    let mut prepared = prepare_native(&source, None, &settings);
    let first_video_player_time = first_video_packet_time(prepared.demuxer.as_mut());
    let intent = WebMediaSourceIntent::native_dash(
        source,
        web_media_core::WebMediaPresentationKind::Vod,
        prepared.source_state,
    );
    DashSubtitleFixture {
        intent,
        first_video_player_time,
        _alive: Box::new((server, prepared.demuxer, prepared.seek_port)),
    }
}

/// Время первого пакета видео (его `pts` уже на шкале плеера).
fn first_video_packet_time(demuxer: &mut dyn Demuxer) -> Duration {
    let video_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .map(|track| track.id)
        .expect("the fixture publishes a video track");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match demuxer.next_event().expect("demux event") {
            DemuxReadEvent::Packet(packet) if packet.track_id == video_track => {
                return packet.pts;
            }
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                std::thread::sleep(DemuxRetryHint::MIN_RETRY_AFTER);
            }
            DemuxReadEvent::EndOfStream => panic!("no video packet before the end of the stream"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "no video packet in time");
    }
}

/// Реальный нативный DASH-intent отдаёт источник субтитров СВОЕГО открытия, и лента на нём
/// доставляет реплики обеих дорожек на шкале плеера (включая верхнюю область IMSC).
#[test]
fn native_dash_intent_hands_out_the_subtitle_source_of_its_own_open() {
    let fixture = open_dash_with_subtitles();
    let source = fixture
        .intent
        .stream_subtitle_source()
        .expect("a native DASH open with text sets hands out a stream source");
    let tracks = source.tracks();
    assert_eq!(tracks.len(), 2);
    assert_eq!(&*tracks[0].name, "English");
    assert_eq!(tracks[0].language.as_deref(), Some("en"));
    assert!(tracks[0].default, "role main marks the default track");
    assert!(!tracks[1].default);
    assert_eq!(fixture.first_video_player_time, Duration::ZERO);

    for (index, expected_line_percent) in [(0_u32, None), (1_u32, Some(5.0_f32))] {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let feed = StreamSubtitleFeed::start(
            &source,
            StreamSubtitleTrackId::new(index),
            StreamSubtitleFeedConfig {
                ahead: Duration::from_secs(20),
                behind: Duration::from_secs(5),
                max_segment_bytes: std::num::NonZeroUsize::new(64 * 1_024).unwrap(),
                max_pending_segments: std::num::NonZeroUsize::new(8).unwrap(),
                failure_threshold: std::num::NonZeroU32::new(3).unwrap(),
                message_capacity: std::num::NonZeroUsize::new(64).unwrap(),
            },
            StreamPlaybackPosition::new(Duration::ZERO),
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .expect("feed starts");
        let deadline = Instant::now() + Duration::from_secs(10);
        let event = loop {
            let found = feed.drain_messages().into_iter().find_map(|message| {
                if let StreamSubtitleMessage::Events(events) = message {
                    events.events.into_iter().next()
                } else {
                    None
                }
            });
            if let Some(event) = found {
                break event;
            }
            assert!(Instant::now() < deadline, "no cue from track {index}");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(event.start().as_micros(), 500_000);
        assert_eq!(event.end().as_micros(), 1_500_000);
        let subtitle_core::SubtitleContent::Text(cue) = event.content() else {
            panic!("text cue expected");
        };
        match expected_line_percent {
            Some(percent) => assert_eq!(
                cue.placement.line,
                subtitle_core::CueLine::Percent(
                    subtitle_core::PercentOfVideo::new(percent).unwrap()
                ),
                "the IMSC top region survives the whole chain"
            ),
            None => assert_eq!(cue.placement.line, subtitle_core::CueLine::Auto),
        }
        assert!(
            wakes.load(Ordering::SeqCst) > 0,
            "the feed wakes the UI when cues arrive"
        );
    }
}
