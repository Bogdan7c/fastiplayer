//! Hermetic сквозные тесты ленты субтитров на НАСТОЯЩЕМ открытии HLS (loopback-сервер,
//! MPEG-TS видео, WebVTT-сегменты): реплики должны лечь на ту же шкалу, что и пакеты видео.

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

use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use media_core::{
    DemuxReadEvent, Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelinePortGeneration, Packet,
    TrackKind,
};
use media_source_open::stream_subtitles::{
    HlsStreamSubtitleTransport, StreamPlaybackPosition, StreamSegmentSkipReason,
    StreamSubtitleFailureKind, StreamSubtitleFeed, StreamSubtitleFeedConfig, StreamSubtitleMessage,
    StreamSubtitleSource, StreamSubtitleTrackId, StreamSubtitleTransport,
    StreamTrackUnavailableReason,
};
use source_core::CancellationToken;
use support::{
    ObservedRequest, TestQueries, TestServer, adaptive_context, demux_registry, muxed_ts,
    open_policy, response,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsLiveOpenRequest, HlsMainTrackLayoutIntent, HlsManifestInput,
    HlsRequestOverrides, HlsRequiredContainer, HlsVariantSelectionIntent, HlsVodOpenRequest,
    prepare_hls_live, prepare_hls_vod,
};
use web_media_transport_api::SourceGeneration;

const WAIT: Duration = Duration::from_secs(8);
/// Допуск на округление тиков 90 кГц и микросекунд.
const TOLERANCE: Duration = Duration::from_micros(3);
const PLAYLIST_SECRET: &str = "PLAYLISTSECRET";
const SEGMENT_SECRET: &str = "SEGMENTSECRET";
/// Первый PTS видео в фикстуре: 10 с (сырые метки НЕ начинаются с нуля).
const VIDEO_FIRST_PTS_TICKS: u64 = 900_000;

// ----- открытие HLS ---------------------------------------------------------------------------

fn muxed_selection() -> HlsVariantSelectionIntent {
    HlsVariantSelectionIntent {
        resolution: None,
        codecs: None,
        audio: HlsAudioLayoutIntent::Muxed,
        main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
    }
}

fn master() -> String {
    format!(
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",LANGUAGE=\"en\",\
         DEFAULT=YES,AUTOSELECT=YES,URI=\"subs/en.m3u8?token={PLAYLIST_SECRET}\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=1000,SUBTITLES=\"subs\"\nvideo.m3u8\n"
    )
}

fn open_common(server: &TestServer) -> (HlsVodOpenRequest, SourceGeneration) {
    let generation = SourceGeneration::new(1);
    let target = server.target("/master.m3u8");
    (
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
                main: HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream),
                alternate_audio: None,
            },
            demux_registry: demux_registry(),
            policy: open_policy(),
        },
        generation,
    )
}

/// Открывает VOD; источник субтитров собран ДО `into_demuxer`, демуксер ещё не читали.
fn open_vod(server: &TestServer) -> (StreamSubtitleSource, Box<dyn Demuxer + Send>) {
    let (request, _) = open_common(server);
    let opened = prepare_hls_vod(request).expect("prepare VOD with subtitles");
    let source =
        HlsStreamSubtitleTransport::new(opened.subtitle_transport(), opened.timestamp_origins())
            .into_source()
            .expect("master declares a subtitle rendition");
    (source, opened.into_demuxer())
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

fn open_live(server: &TestServer) -> (StreamSubtitleSource, Box<dyn Demuxer + Send>) {
    let (common, _generation) = open_common(server);
    let request = HlsLiveOpenRequest {
        common,
        endpoint_refresh: Arc::new(NoRefreshPort),
        timeline_port_generation: DynamicMediaTimelinePortGeneration::new(
            NonZeroU64::new(1).expect("non-zero test port generation"),
        ),
        initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
    };
    let opened = prepare_hls_live(request).expect("prepare live with subtitles");
    let source =
        HlsStreamSubtitleTransport::new(opened.subtitle_transport(), opened.timestamp_origins())
            .into_source()
            .expect("master declares a subtitle rendition");
    let (demuxer, _timeline, _) = opened.into_parts();
    (source, demuxer)
}

// ----- сервер ---------------------------------------------------------------------------------

/// Номер сегмента из строки запроса `.../subs/seg<N>.vtt...`.
fn segment_number(request: &ObservedRequest) -> Option<u32> {
    let line = &request.request_line;
    let tail = line.split("/subs/seg").nth(1)?;
    tail.split(".vtt").next()?.parse().ok()
}

/// Однострочный VTT-сегмент с картой времени и одной репликой.
fn vtt(mpegts_ticks: u64, local: &str, cue_start: &str, cue_end: &str, text: &str) -> String {
    format!(
        "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:{mpegts_ticks},LOCAL:{local}\n\n{cue_start} --> {cue_end}\n{text}\n"
    )
}

/// Плейлист субтитров: `count` сегментов по 1 с, с секретом в query ссылки сегмента.
fn subtitle_playlist(count: u32, first_sequence: u32, end: bool) -> String {
    let mut playlist =
        format!("#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{first_sequence}\n");
    for number in first_sequence..first_sequence + count {
        playlist.push_str(&format!(
            "#EXTINF:1,\nseg{number}.vtt?sig={SEGMENT_SECRET}\n"
        ));
    }
    if end {
        playlist.push_str("#EXT-X-ENDLIST\n");
    }
    playlist
}

/// Видео VOD: media playlist и его TS-сегменты `(имя, байты)`.
struct VideoFixture {
    playlist: String,
    segments: Vec<(&'static str, Vec<u8>)>,
}

/// Одно видео-сегмент с первым PTS = 10 с.
fn single_segment_video() -> VideoFixture {
    VideoFixture {
        playlist: "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nvideo.ts\n#EXT-X-ENDLIST\n"
            .to_owned(),
        segments: vec![("/video.ts", muxed_ts(VIDEO_FIRST_PTS_TICKS))],
    }
}

/// Сервер VOD: master, видео, плейлист субтитров и сегменты от `segment_body`.
fn vod_server(
    playlist: String,
    segment_body: impl Fn(u32) -> Option<String> + Send + Sync + 'static,
) -> TestServer {
    vod_server_with_video(single_segment_video(), playlist, segment_body)
}

fn vod_server_with_video(
    video: VideoFixture,
    playlist: String,
    segment_body: impl Fn(u32) -> Option<String> + Send + Sync + 'static,
) -> TestServer {
    TestServer::start(move |_, request| serve(request, &video, &playlist, &segment_body))
}

fn serve(
    request: &ObservedRequest,
    video: &VideoFixture,
    subtitle_playlist: &str,
    segment_body: &dyn Fn(u32) -> Option<String>,
) -> Vec<u8> {
    let line = &request.request_line;
    if line.contains("/master.m3u8") {
        response("200 OK", &[], master().as_bytes())
    } else if line.contains("/video.m3u8") {
        response("200 OK", &[], video.playlist.as_bytes())
    } else if let Some((_, bytes)) = video.segments.iter().find(|(name, _)| line.contains(name)) {
        response("200 OK", &[], bytes)
    } else if line.contains("/subs/en.m3u8") {
        response("200 OK", &[], subtitle_playlist.as_bytes())
    } else if let Some(body) = segment_number(request).and_then(segment_body) {
        response("200 OK", &[], body.as_bytes())
    } else {
        response("404 Not Found", &[], b"")
    }
}

fn subtitle_segment_requests(server: &TestServer) -> Vec<u32> {
    server
        .requests()
        .iter()
        .filter_map(segment_number)
        .collect()
}

// ----- видео и сообщения ----------------------------------------------------------------------

fn read_packets_until_eos(demuxer: &mut dyn Demuxer) -> Vec<Packet> {
    let deadline = Instant::now() + WAIT;
    let mut packets = Vec::new();
    loop {
        match demuxer.next_event().expect("HLS stays readable") {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(Instant::now() < deadline, "HLS worker timed out");
                std::thread::sleep(Duration::from_millis(2));
            }
            DemuxReadEvent::Packet(packet) => packets.push(packet),
            DemuxReadEvent::EndOfStream => return packets,
            _ => {}
        }
    }
}

/// Время плеера у видеопакета, чей сырой PTS равен 10 с (первый видеопакет фикстуры).
fn player_time_of_raw_ten_seconds(packets: &[Packet]) -> Duration {
    packets
        .iter()
        .find(|packet| packet.kind == TrackKind::Video)
        .expect("a demuxed video packet")
        .pts
}

fn feed_config() -> StreamSubtitleFeedConfig {
    StreamSubtitleFeedConfig {
        ahead: Duration::from_secs(20),
        behind: Duration::from_secs(5),
        max_segment_bytes: NonZeroUsize::new(64 * 1_024).expect("non-zero"),
        max_pending_segments: NonZeroUsize::new(64).expect("non-zero"),
        failure_threshold: NonZeroU32::new(3).expect("non-zero"),
        outage_budget: Duration::from_secs(60),
        message_capacity: NonZeroUsize::new(256).expect("non-zero"),
    }
}

struct Running {
    feed: StreamSubtitleFeed,
    position: StreamPlaybackPosition,
    wakes: Arc<AtomicUsize>,
}

fn start_feed(source: &StreamSubtitleSource, config: StreamSubtitleFeedConfig) -> Running {
    let position = StreamPlaybackPosition::new(Duration::ZERO);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    let feed = StreamSubtitleFeed::start(
        source,
        StreamSubtitleTrackId::new(0),
        config,
        position.clone(),
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .expect("feed starts");
    Running {
        feed,
        position,
        wakes,
    }
}

impl Running {
    fn collect_until(
        &self,
        done: impl Fn(&[StreamSubtitleMessage]) -> bool,
    ) -> Vec<StreamSubtitleMessage> {
        let deadline = Instant::now() + WAIT;
        let mut messages = Vec::new();
        while !done(&messages) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for feed messages, got {messages:?}"
            );
            messages.extend(self.feed.drain_messages());
            std::thread::sleep(Duration::from_millis(5));
        }
        messages
    }

    fn quiet_period(&self, duration: Duration) -> Vec<StreamSubtitleMessage> {
        std::thread::sleep(duration);
        self.feed.drain_messages()
    }
}

/// Начала реплик (шкала плеера) по номеру сегмента; номер = media sequence.
fn cue_starts(messages: &[StreamSubtitleMessage]) -> Vec<(u64, Duration)> {
    let mut starts = Vec::new();
    for message in messages {
        if let StreamSubtitleMessage::Events(events) = message {
            for event in &events.events {
                let micros = u64::try_from(event.start().as_micros()).expect("non-negative cue");
                starts.push((events.segment.value(), Duration::from_micros(micros)));
            }
        }
    }
    starts.sort_unstable();
    starts
}

fn assert_close(actual: Duration, expected: Duration, what: &str) {
    assert!(
        actual.abs_diff(expected) <= TOLERANCE,
        "{what}: {actual:?} differs from expected {expected:?}"
    );
}

fn unavailable_count(messages: &[StreamSubtitleMessage]) -> usize {
    messages
        .iter()
        .filter(|message| matches!(message, StreamSubtitleMessage::TrackUnavailable(_)))
        .count()
}

// ----- тесты ----------------------------------------------------------------------------------

#[test]
fn cues_land_on_the_same_timeline_as_the_demuxed_video_packets() {
    // Сегмент k: MPEGTS = 10 с + k, LOCAL = 0, реплика на 0,5 с файла.
    let server = vod_server(subtitle_playlist(3, 0, true), |number| {
        Some(vtt(
            VIDEO_FIRST_PTS_TICKS + u64::from(number) * 90_000,
            "00:00:00.000",
            "00:00:00.500",
            "00:00:00.900",
            &format!("cue {number}"),
        ))
    });
    let (source, mut demuxer) = open_vod(&server);
    let running = start_feed(&source, feed_config());

    let packets = read_packets_until_eos(&mut *demuxer);
    let video_ten_seconds = player_time_of_raw_ten_seconds(&packets);
    // Сырой PTS первого видеопакета (10 с) - это время плеера около 0,033 с, а не 10 с.
    assert!(video_ten_seconds < Duration::from_secs(1));

    let messages = running.collect_until(|messages| cue_starts(messages).len() >= 3);
    let starts = cue_starts(&messages);
    for (number, start) in starts {
        // cue = (MPEGTS_k - 10 с) + 0,5 с от сырых 10 с видео.
        let expected = video_ten_seconds + Duration::from_millis(500 + 1_000 * number);
        assert_close(start, expected, &format!("cue of segment {number}"));
    }
    assert!(running.wakes.load(Ordering::SeqCst) >= 3);
}

#[test]
fn each_discontinuity_epoch_uses_its_own_video_origin() {
    // Видео: эпоха 0 (сырые 10 с), разрыв, эпоха 1 (сырые 50 с). Субтитры повторяют разрыв:
    // сегмент 1 принадлежит эпохе 1, и его реплика обязана считаться по ЕЁ точке отсчёта.
    let video = VideoFixture {
        playlist: "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\none.ts\n\
                   #EXT-X-DISCONTINUITY\n#EXTINF:1,\ntwo.ts\n#EXT-X-ENDLIST\n"
            .to_owned(),
        segments: vec![
            ("/one.ts", muxed_ts(900_000)),
            ("/two.ts", muxed_ts(4_500_000)),
        ],
    };
    let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nseg0.vtt\n\
                    #EXT-X-DISCONTINUITY\n#EXTINF:1,\nseg1.vtt\n#EXT-X-ENDLIST\n"
        .to_owned();
    let server = vod_server_with_video(video, playlist, |number| {
        let ticks = if number == 0 { 900_000 } else { 4_500_000 };
        Some(vtt(
            ticks,
            "00:00:00.000",
            "00:00:00.500",
            "00:00:00.900",
            "cue",
        ))
    });
    let (source, mut demuxer) = open_vod(&server);
    let running = start_feed(&source, feed_config());
    let packets = read_packets_until_eos(&mut *demuxer);

    let first_epoch = packets
        .iter()
        .find(|packet| packet.kind == TrackKind::Video && packet.pts < Duration::from_secs(1))
        .expect("epoch 0 video packet")
        .pts;
    let second_epoch = packets
        .iter()
        .find(|packet| packet.kind == TrackKind::Video && packet.pts >= Duration::from_secs(1))
        .expect("epoch 1 video packet")
        .pts;
    let messages = running.collect_until(|messages| cue_starts(messages).len() >= 2);
    let starts = cue_starts(&messages);
    assert_close(
        starts[0].1,
        first_epoch + Duration::from_millis(500),
        "epoch 0 cue",
    );
    assert_close(
        starts[1].1,
        second_epoch + Duration::from_millis(500),
        "epoch 1 cue",
    );
    // Эпоха 1 начинается на шкале плеера после первой секунды: реплика там, а не около 40 с.
    assert!(starts[1].1 > Duration::from_secs(1) && starts[1].1 < Duration::from_secs(3));
}

#[test]
fn segment_without_timestamp_map_is_relative_to_the_discontinuity_epoch_start() {
    // Без X-TIMESTAMP-MAP (как hls.js): время реплик отсчитывается от начала эпохи разрыва.
    // Эпоха 0 начинается на 0 с шкалы плеера, поэтому реплика 00:00:00.500 -> ровно 0,5 с,
    // независимо от того, что сырые метки видео начинаются с 10 с.
    let server = vod_server(subtitle_playlist(1, 0, true), |_| {
        Some("WEBVTT\n\n00:00:00.500 --> 00:00:00.900\nplain cue\n".to_owned())
    });
    let (source, mut demuxer) = open_vod(&server);
    let running = start_feed(&source, feed_config());
    read_packets_until_eos(&mut *demuxer);

    let messages = running.collect_until(|messages| !cue_starts(messages).is_empty());
    assert_eq!(cue_starts(&messages), vec![(0, Duration::from_millis(500))]);
}

#[test]
fn nonzero_local_and_mpegts_offset_are_both_applied() {
    // Реплики в файле живут на «часовой» шкале: LOCAL = 10:00:00, реплика на 10:00:01.
    let server = vod_server(subtitle_playlist(1, 0, true), |_| {
        Some(vtt(
            VIDEO_FIRST_PTS_TICKS,
            "00:10:00.000",
            "00:10:01.000",
            "00:10:02.000",
            "late cue",
        ))
    });
    let (source, mut demuxer) = open_vod(&server);
    let running = start_feed(&source, feed_config());
    let packets = read_packets_until_eos(&mut *demuxer);
    let video_ten_seconds = player_time_of_raw_ten_seconds(&packets);

    let messages = running.collect_until(|messages| !cue_starts(messages).is_empty());
    let starts = cue_starts(&messages);
    assert_eq!(starts.len(), 1);
    // media = 10 с + (10:00:01 - 10:00:00) = 11 с -> на секунду позже сырых 10 с видео.
    assert_close(
        starts[0].1,
        video_ten_seconds + Duration::from_secs(1),
        "cue with non-zero LOCAL",
    );
    let StreamSubtitleMessage::Events(events) = &messages[0] else {
        panic!("first message must carry events: {messages:?}");
    };
    let duration = events.events[0].end().as_micros() - events.events[0].start().as_micros();
    assert_eq!(duration, 1_000_000, "cue duration is untouched by mapping");
}

#[test]
fn missing_segment_is_skipped_below_the_threshold_and_cues_of_others_arrive() {
    let server = vod_server(subtitle_playlist(3, 0, true), |number| {
        (number != 1).then(|| {
            vtt(
                VIDEO_FIRST_PTS_TICKS + u64::from(number) * 90_000,
                "00:00:00.000",
                "00:00:00.100",
                "00:00:00.600",
                &format!("cue {number}"),
            )
        })
    });
    let (source, mut demuxer) = open_vod(&server);
    let running = start_feed(&source, feed_config());
    read_packets_until_eos(&mut *demuxer);

    let messages = running.collect_until(|messages| {
        cue_starts(messages).len() >= 2
            && messages
                .iter()
                .any(|m| matches!(m, StreamSubtitleMessage::SegmentSkipped { .. }))
    });
    let delivered: Vec<u64> = cue_starts(&messages).iter().map(|(n, _)| *n).collect();
    assert_eq!(delivered, vec![0, 2], "segment 1 has no cues, others do");
    assert!(messages.iter().any(|message| matches!(
        message,
        StreamSubtitleMessage::SegmentSkipped {
            reason: StreamSegmentSkipReason::Failed(StreamSubtitleFailureKind::NotFound),
            ..
        }
    )));
    let more = running.quiet_period(Duration::from_millis(500));
    assert_eq!(unavailable_count(&messages) + unavailable_count(&more), 0);
}

#[test]
fn failing_segments_in_a_row_report_unavailable_once_and_stop_requesting() {
    let server = vod_server(subtitle_playlist(8, 0, true), |_| None);
    let (source, _demuxer) = open_vod(&server);
    let running = start_feed(&source, feed_config());

    let messages = running.collect_until(|messages| unavailable_count(messages) >= 1);
    assert_eq!(
        messages.last(),
        Some(&StreamSubtitleMessage::TrackUnavailable(
            StreamTrackUnavailableReason::ConsecutiveSegmentFailures { count: 3 }
        ))
    );
    let more = running.quiet_period(Duration::from_millis(600));
    assert!(
        more.is_empty(),
        "feed must stay silent after the verdict: {more:?}"
    );
    assert_eq!(
        subtitle_segment_requests(&server).len(),
        3,
        "no requests after the threshold"
    );
}

#[test]
fn broken_subtitle_playlist_makes_the_track_unavailable() {
    let server = vod_server("<html>not a playlist</html>".to_owned(), |_| None);
    let (source, _demuxer) = open_vod(&server);
    let running = start_feed(&source, feed_config());
    let messages = running.collect_until(|messages| unavailable_count(messages) >= 1);
    assert_eq!(
        messages,
        vec![StreamSubtitleMessage::TrackUnavailable(
            StreamTrackUnavailableReason::IndexLoadFailed(StreamSubtitleFailureKind::Malformed)
        )]
    );
}

#[test]
fn seek_moves_the_window_and_requests_segments_around_the_new_position() {
    // 40 сегментов по 1 с; окно ahead 5 с / behind 2 с.
    let server = vod_server(subtitle_playlist(40, 0, true), |number| {
        Some(vtt(
            VIDEO_FIRST_PTS_TICKS + u64::from(number) * 90_000,
            "00:00:00.000",
            "00:00:00.100",
            "00:00:00.600",
            "cue",
        ))
    });
    let (source, _demuxer) = open_vod(&server);
    let mut config = feed_config();
    config.ahead = Duration::from_secs(5);
    config.behind = Duration::from_secs(2);
    let running = start_feed(&source, config);

    // Видео не демуксируем: сегменты ждут привязку, но запросы идут по окну.
    let deadline = Instant::now() + WAIT;
    while subtitle_segment_requests(&server).len() < 5 {
        assert!(Instant::now() < deadline, "initial window never requested");
        std::thread::sleep(Duration::from_millis(5));
    }
    let initial: std::collections::BTreeSet<u32> =
        subtitle_segment_requests(&server).into_iter().collect();
    assert_eq!(initial, (0..5).collect(), "window [0, 5) from the start");

    running.position.jump_to(Duration::from_secs(30));
    let deadline = Instant::now() + WAIT;
    while subtitle_segment_requests(&server).len() < 5 + 7 {
        assert!(
            Instant::now() < deadline,
            "window after seek never requested"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let after: std::collections::BTreeSet<u32> =
        subtitle_segment_requests(&server).into_iter().collect();
    // Окно [28, 35): семь новых сегментов, середина плейлиста не запрашивалась.
    assert_eq!(
        after.difference(&initial).copied().collect::<Vec<_>>(),
        (28..35).collect::<Vec<_>>()
    );
}

#[test]
fn live_playlist_reload_brings_new_segments_and_cancel_is_prompt() {
    // Видео-live: два сегмента (PTS 10 с и 20 с). Субтитры: сначала два сегмента, после
    // нескольких перечитываний - третий.
    let first = muxed_ts(900_000);
    let second = muxed_ts(1_800_000);
    let reloads = Arc::new(AtomicUsize::new(0));
    let reload_counter = Arc::clone(&reloads);
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
            let count = if reload_counter.fetch_add(1, Ordering::SeqCst) == 0 {
                2
            } else {
                3
            };
            response("200 OK", &[], subtitle_playlist(count, 0, false).as_bytes())
        } else if let Some(number) = segment_number(request) {
            // Сегменты 0 и 2 привязаны к видео-сегменту a (10 с), сегмент 1 - к b (20 с).
            let ticks = if number == 1 { 1_800_000 } else { 900_000 };
            let body = vtt(
                ticks,
                "00:00:00.000",
                "00:00:00.500",
                "00:00:00.900",
                &format!("cue {number}"),
            );
            response("200 OK", &[], body.as_bytes())
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let (source, mut demuxer) = open_live(&server);
    let running = start_feed(&source, feed_config());

    // Читаем первые пакеты live: origin сегментов публикуется демуксером.
    let deadline = Instant::now() + WAIT;
    let mut packets = Vec::new();
    while packets.len() < 4 && Instant::now() < deadline {
        match demuxer.next_event().expect("live stays readable") {
            DemuxReadEvent::Packet(packet) => packets.push(packet),
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                std::thread::sleep(Duration::from_millis(2));
            }
            _ => {}
        }
    }
    let first_video = packets
        .iter()
        .find(|packet| packet.kind == TrackKind::Video)
        .expect("live video packet")
        .pts;

    // Сегменты 0 и 1 из первого снимка, сегмент 2 - только из перечитанного плейлиста.
    let messages = running
        .collect_until(|messages| cue_starts(messages).iter().any(|(number, _)| *number == 2));
    assert!(reloads.load(Ordering::SeqCst) >= 2, "playlist was reloaded");
    let starts = cue_starts(&messages);
    let segment_two = starts
        .iter()
        .find(|(number, _)| *number == 2)
        .expect("cue of the new segment")
        .1;
    // Сегмент 2 привязан к сырым 10 с: реплика на 0,5 с файла.
    assert_close(
        segment_two,
        first_video + Duration::from_millis(500),
        "cue of the segment that appeared on reload",
    );

    // Закрытие ленты не ждёт поток.
    let started = Instant::now();
    drop(running);
    assert!(started.elapsed() < Duration::from_millis(200));
}

#[test]
fn debug_output_never_reveals_urls_or_query_secrets() {
    let server = vod_server(subtitle_playlist(2, 0, true), |number| {
        Some(vtt(
            VIDEO_FIRST_PTS_TICKS,
            "00:00:00.000",
            "00:00:00.100",
            "00:00:00.600",
            &format!("cue {number}"),
        ))
    });
    let (source, mut demuxer) = open_vod(&server);
    let transport = source.clone();
    let running = start_feed(&source, feed_config());
    read_packets_until_eos(&mut *demuxer);
    let messages = running.collect_until(|messages| cue_starts(messages).len() >= 2);

    let index = {
        let (request, _) = open_common(&server);
        let opened = prepare_hls_vod(request).expect("second open for index");
        let direct = HlsStreamSubtitleTransport::new(
            opened.subtitle_transport(),
            opened.timestamp_origins(),
        );
        let _keep = opened.into_demuxer();
        let index = direct
            .load_index(StreamSubtitleTrackId::new(0), &CancellationToken::new())
            .expect("index");
        format!("{direct:?} {index:?}")
    };
    let everything = format!(
        "{source:?} {transport:?} {:?} {messages:?} {index}",
        running.feed
    );
    for secret in [
        PLAYLIST_SECRET,
        SEGMENT_SECRET,
        "127.0.0.1",
        "seg0.vtt",
        "en.m3u8",
    ] {
        assert!(!everything.contains(secret), "Debug leaked {secret}");
    }
}
