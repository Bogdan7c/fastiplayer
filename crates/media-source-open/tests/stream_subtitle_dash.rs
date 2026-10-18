//! Hermetic сквозные тесты субтитров DASH на НАСТОЯЩЕМ открытии `prepare_dash_vod`
//! (loopback-сервер, fMP4-видео с аудио, текстовые AdaptationSet): реплики должны лечь на ту
//! же шкалу, что и пакеты видео, а сбои сегментов субтитров - не мешать остальному.

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

use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bounded_xml_reader::XmlBudgets;
use dash_mpd_core::{DashContainer, DashMediaKind, DashMpdLimits};
use demux_api::{
    CompositeComponentLeadPolicy, DemuxSniffBudget, ProgressiveAsyncSeekLimits,
    ProgressiveDemuxBufferLimits,
};
use media_core::{DemuxReadEvent, DemuxRetryHint, Demuxer, Packet, TrackKind};
use media_source_open::stream_subtitles::{
    DashStreamSubtitleTransport, StreamPlaybackPosition, StreamSegmentSkipReason,
    StreamSubtitleFeed, StreamSubtitleFeedConfig, StreamSubtitleMessage, StreamSubtitleSource,
    StreamSubtitleTrackId,
};
use source_core::CancellationToken;
use subtitle_core::{SubtitleContent, SubtitleEvent};
use support::{
    ObservedRequest, TestQueries, TestServer, adaptive_context, demux_registry, response,
};
use web_media_dash::{
    DashManifestInput, DashPresentationSelection, DashRepresentationEvidence, DashVodHttpContext,
    DashVodInput, DashVodOpenPolicy, DashVodOpenRequest, prepare_dash_vod,
};
use web_media_transport_api::SourceGeneration;

const WAIT: Duration = Duration::from_secs(8);
const SEGMENT_SECRET: &str = "SEGMENTSECRET";

// ----- открытие DASH --------------------------------------------------------------------------

fn dash_policy() -> DashVodOpenPolicy {
    DashVodOpenPolicy {
        maximum_manifest_bytes: NonZeroUsize::new(64 * 1_024).unwrap(),
        maximum_fragment_bytes: NonZeroUsize::new(256 * 1_024).unwrap(),
        maximum_range_read_bytes: NonZeroUsize::new(16 * 1_024).unwrap(),
        maximum_cached_range_pages: NonZeroUsize::new(2).unwrap(),
        maximum_planned_segments: NonZeroUsize::new(64).unwrap(),
        maximum_parallel_catalog_probes: NonZeroUsize::new(4).unwrap(),
        demux_sniff_budget: DemuxSniffBudget::new(
            NonZeroUsize::new(8 * 1_024).unwrap(),
            NonZeroUsize::new(4).unwrap(),
            Duration::from_secs(1),
        )
        .unwrap(),
        progressive_limits: ProgressiveDemuxBufferLimits::new(
            NonZeroUsize::new(64).unwrap(),
            NonZeroUsize::new(512 * 1_024).unwrap(),
        ),
        asynchronous_seek_limits: ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(4).unwrap()),
        retry_hint: DemuxRetryHint::new(Duration::from_millis(5)).unwrap(),
        composite_lead_policy: CompositeComponentLeadPolicy::single_pending_packet(
            Duration::from_secs(2),
            NonZeroUsize::new(256 * 1_024).unwrap(),
        )
        .unwrap(),
        maximum_seek_scan_events: NonZeroUsize::new(4_096).unwrap(),
        maximum_seek_scan_bytes: NonZeroUsize::new(8 * 1_024 * 1_024).unwrap(),
    }
}

fn manifest_input(target: source_core::HttpRequestTarget) -> DashManifestInput {
    DashManifestInput {
        target,
        xml_budgets: XmlBudgets::builder()
            .maximum_document_bytes(64 * 1024)
            .maximum_depth(32)
            .maximum_tokens(4_096)
            .maximum_attributes_per_element(32)
            .maximum_attribute_count(2_048)
            .maximum_attribute_bytes(32 * 1024)
            .maximum_namespace_declarations_per_element(8)
            .maximum_namespace_declaration_count(32)
            .maximum_namespace_bytes(4 * 1024)
            .maximum_text_bytes(32 * 1024)
            .build()
            .unwrap(),
        mpd_limits: DashMpdLimits {
            maximum_periods: 8,
            maximum_adaptation_sets_per_period: 16,
            maximum_representations_per_adaptation_set: 16,
            maximum_segments_per_list: 64,
            maximum_timeline_entries: 64,
            maximum_schema_string_bytes: 4 * 1024,
        },
    }
}

/// Открывает static MPD сервера; источник субтитров собран ДО `into_demuxer`.
fn open_dash(server: &TestServer) -> (Option<StreamSubtitleSource>, Box<dyn Demuxer + Send>) {
    let target = server.target("/manifest.mpd");
    let generation = SourceGeneration::new(1);
    let opened = prepare_dash_vod(DashVodOpenRequest {
        http: DashVodHttpContext::Manifest(Box::new(adaptive_context(
            &target,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ))),
        generation,
        input: DashVodInput::Manifest(manifest_input(target)),
        selection: DashPresentationSelection::Single {
            main: DashRepresentationEvidence {
                media_kind: DashMediaKind::Muxed,
                container: DashContainer::IsoBmff,
                representation_id: Some("muxed".to_owned()),
                codecs: None,
                bandwidth: None,
                dimensions: None,
            },
        },
        demux_registry: demux_registry(),
        policy: dash_policy(),
    })
    .expect("DASH VOD with subtitles prepares");
    let source = opened
        .text_subtitles()
        .and_then(|text| DashStreamSubtitleTransport::new(text).into_source());
    (source, Box::new(opened.into_demuxer()))
}

// ----- fMP4 субтитры --------------------------------------------------------------------------

fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(payload.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(payload);
    bytes
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![version];
    payload.extend_from_slice(&flags.to_be_bytes()[1..]);
    payload.extend_from_slice(body);
    mp4_box(kind, &payload)
}

fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.iter().flatten().copied().collect()
}

/// Инициализация текстовой дорожки (`stpp` либо `wvtt`) с таймскейлом 1000.
fn text_init(entry: &[u8; 4]) -> Vec<u8> {
    let mut tkhd = vec![0; 8];
    tkhd.extend_from_slice(&1_u32.to_be_bytes());
    let mut mdhd = vec![0; 8];
    mdhd.extend_from_slice(&1000_u32.to_be_bytes());
    mdhd.extend_from_slice(&[0; 8]);
    let mut stsd = 1_u32.to_be_bytes().to_vec();
    stsd.extend_from_slice(&mp4_box(entry, &[0, 0, 0, 0, 0, 0, 0, 1]));
    let stbl = mp4_box(b"stbl", &full_box(b"stsd", 0, 0, &stsd));
    let mdia = mp4_box(
        b"mdia",
        &concat(&[full_box(b"mdhd", 0, 0, &mdhd), mp4_box(b"minf", &stbl)]),
    );
    let trak = mp4_box(b"trak", &concat(&[full_box(b"tkhd", 0, 3, &tkhd), mdia]));
    concat(&[mp4_box(b"ftyp", b"iso6\0\0\0\0"), mp4_box(b"moov", &trak)])
}

/// Фрагмент: один сэмпл (`tfdt`, длительность в мс).
fn text_fragment(tfdt: u64, duration_ms: u32, sample: &[u8]) -> Vec<u8> {
    let build = |data_offset: i32| {
        let tfhd = full_box(b"tfhd", 0, 0x2_0000, &1_u32.to_be_bytes());
        let tfdt = full_box(b"tfdt", 1, 0, &tfdt.to_be_bytes());
        let mut trun = 1_u32.to_be_bytes().to_vec();
        trun.extend_from_slice(&data_offset.to_be_bytes());
        trun.extend_from_slice(&duration_ms.to_be_bytes());
        trun.extend_from_slice(&u32::try_from(sample.len()).unwrap().to_be_bytes());
        let trun = full_box(b"trun", 0, 0x301, &trun);
        mp4_box(
            b"moof",
            &concat(&[
                full_box(b"mfhd", 0, 0, &1_u32.to_be_bytes()),
                mp4_box(b"traf", &concat(&[tfhd, tfdt, trun])),
            ]),
        )
    };
    let moof_len = build(0).len();
    concat(&[
        build(i32::try_from(moof_len + 8).unwrap()),
        mp4_box(b"mdat", sample),
    ])
}

fn ttml_document(begin: &str, end: &str, text: &str) -> String {
    format!(
        r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="{begin}" end="{end}">{text}</p></div></body></tt>"#
    )
}

fn wvtt_sample(text: &str, settings: Option<&str>) -> Vec<u8> {
    let mut children = Vec::new();
    if let Some(settings) = settings {
        children.extend(mp4_box(b"sttg", settings.as_bytes()));
    }
    children.extend(mp4_box(b"payl", text.as_bytes()));
    mp4_box(b"vttc", &children)
}

// ----- сервер ---------------------------------------------------------------------------------

/// Видео (muxed fMP4: init + два одинаковых фрагмента) как SegmentList `[start; start+2 с]`.
fn video_set(segment_prefix: &str) -> String {
    format!(
        r#"<AdaptationSet contentType="application" mimeType="application/mp4"
             codecs="avc1.4d401f,mp4a.40.2">
             <Representation id="muxed">
               <SegmentList timescale="1" duration="1">
                 <Initialization sourceURL="{segment_prefix}init.mp4"/>
                 <SegmentURL media="{segment_prefix}one.m4s"/>
                 <SegmentURL media="{segment_prefix}two.m4s"/>
               </SegmentList>
             </Representation>
           </AdaptationSet>"#
    )
}

/// Маршрутизатор: `routes` отдаёт 200, всё остальное - 404.
struct Routes {
    entries: Vec<(&'static str, Vec<u8>)>,
}

impl Routes {
    fn serve(&self, request: &ObservedRequest) -> Vec<u8> {
        let path = request
            .request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .split('?')
            .next()
            .unwrap_or_default()
            .to_owned();
        match self.entries.iter().find(|(route, _)| *route == path) {
            Some((_, body)) => response("200 OK", &[], body),
            None => response("404 Not Found", &[], b""),
        }
    }
}

fn server_with(manifest: String, extra: Vec<(&'static str, Vec<u8>)>) -> TestServer {
    // Второй фрагмент фикстуры неполный для отдельного сегмента: берём первый дважды.
    let (video_init, first, _) = support::muxed_fmp4();
    let second = first.clone();
    let mut entries = vec![
        ("/manifest.mpd", manifest.into_bytes()),
        ("/init.mp4", video_init),
        ("/one.m4s", first),
        ("/two.m4s", second),
    ];
    entries.extend(extra);
    let routes = Routes { entries };
    TestServer::start(move |_, request| routes.serve(request))
}

// ----- видео и сообщения ----------------------------------------------------------------------

fn first_video_packet(demuxer: &mut dyn Demuxer) -> Packet {
    let deadline = Instant::now() + WAIT;
    loop {
        match demuxer.next_event().expect("DASH stays readable") {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(Instant::now() < deadline, "DASH worker timed out");
                std::thread::sleep(Duration::from_millis(2));
            }
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => return packet,
            DemuxReadEvent::EndOfStream => panic!("stream ended before a video packet"),
            _ => {}
        }
    }
}

fn feed_config() -> StreamSubtitleFeedConfig {
    StreamSubtitleFeedConfig {
        ahead: Duration::from_secs(20),
        behind: Duration::from_secs(5),
        max_segment_bytes: NonZeroUsize::new(64 * 1_024).unwrap(),
        max_pending_segments: NonZeroUsize::new(8).unwrap(),
        failure_threshold: NonZeroU32::new(3).unwrap(),
        outage_budget: Duration::from_secs(60),
        message_capacity: NonZeroUsize::new(256).unwrap(),
    }
}

struct Running {
    feed: StreamSubtitleFeed,
}

fn start_feed(source: &StreamSubtitleSource) -> Running {
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    let feed = StreamSubtitleFeed::start(
        source,
        StreamSubtitleTrackId::new(0),
        feed_config(),
        StreamPlaybackPosition::new(Duration::ZERO),
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .expect("feed starts");
    Running { feed }
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
}

fn all_events(messages: &[StreamSubtitleMessage]) -> Vec<&SubtitleEvent> {
    messages
        .iter()
        .filter_map(|message| match message {
            StreamSubtitleMessage::Events(events) => Some(events.events.iter()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn event_text(event: &SubtitleEvent) -> String {
    match event.content() {
        SubtitleContent::Text(cue) => cue.text.plain_text(),
        other => panic!("unexpected content {other:?}"),
    }
}

fn micros(event_time: subtitle_core::SubtitleTime) -> i64 {
    event_time.as_micros()
}

// ----- тесты ----------------------------------------------------------------------------------

/// Один Period 0..2 с: видео + заданные текстовые наборы.
fn single_period_manifest(text_sets: &str) -> String {
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT2S">
             <Period duration="PT2S">{}{text_sets}</Period></MPD>"#,
        video_set("")
    )
}

/// Два Period по 1 с; текст стоит во втором (старт 1 с).
fn two_period_manifest(second_period_text: &str) -> String {
    let video = |segment: &str| {
        format!(
            r#"<AdaptationSet contentType="application" mimeType="application/mp4"
                 codecs="avc1.4d401f,mp4a.40.2">
                 <Representation id="muxed">
                   <SegmentList timescale="1" duration="1">
                     <Initialization sourceURL="init.mp4"/>
                     <SegmentURL media="{segment}"/>
                   </SegmentList>
                 </Representation>
               </AdaptationSet>"#
        )
    };
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" mediaPresentationDuration="PT2S">
             <Period id="p0" duration="PT1S">{}</Period>
             <Period id="p1" duration="PT1S">{}{second_period_text}</Period></MPD>"#,
        video("one.m4s"),
        video("two.m4s"),
    )
}

#[test]
fn sidecar_ttml_cue_lands_on_the_player_timeline_and_the_track_is_described() {
    let manifest = single_period_manifest(&format!(
        r#"<AdaptationSet contentType="text" mimeType="application/ttml+xml" lang="en">
             <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>
             <Label>English</Label>
             <Representation id="ttml"><BaseURL>sub.ttml?sig={SEGMENT_SECRET}</BaseURL></Representation>
           </AdaptationSet>"#
    ));
    let server = server_with(
        manifest,
        vec![(
            "/sub.ttml",
            ttml_document("00:00:00.500", "00:00:01.500", "Hello DASH").into_bytes(),
        )],
    );
    let (source, mut demuxer) = open_dash(&server);
    let source = source.expect("the MPD declares a text track");

    let tracks = source.tracks();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].language.as_deref(), Some("en"));
    assert_eq!(&*tracks[0].name, "English");
    assert!(tracks[0].default && tracks[0].autoselect && !tracks[0].forced);
    assert!(
        !format!("{source:?}").contains(SEGMENT_SECRET),
        "Debug must not leak the URL"
    );

    let video_origin = first_video_packet(demuxer.as_mut()).pts;
    assert_eq!(
        video_origin,
        Duration::ZERO,
        "video timeline starts at the Period start"
    );

    let running = start_feed(&source);
    let messages = running.collect_until(|messages| !all_events(messages).is_empty());
    let events = all_events(&messages);
    assert_eq!(events.len(), 1);
    assert_eq!(event_text(events[0]), "Hello DASH");
    // Время TTML отсчитывается от начала Period, как и у видео.
    assert_eq!(micros(events[0].start()), 500_000);
    assert_eq!(micros(events[0].end()), 1_500_000);
    assert!(
        server
            .requests()
            .iter()
            .any(|request| request.request_line.contains(SEGMENT_SECRET)),
        "the query secret is used for the request itself"
    );
}

#[test]
fn imsc_top_region_is_kept_as_cue_placement() {
    let document = r#"<tt xmlns="http://www.w3.org/ns/ttml"
        xmlns:tts="http://www.w3.org/ns/ttml#styling" xml:lang="en">
      <head><layout>
        <region xml:id="top" tts:origin="10% 5%" tts:extent="80% 20%"
            tts:displayAlign="before" tts:textAlign="center"/>
      </layout></head>
      <body><div><p begin="00:00:00.000" end="00:00:01.000" region="top">Top line</p></div></body>
    </tt>"#;
    let manifest = single_period_manifest(
        r#"<AdaptationSet mimeType="application/ttml+xml" lang="en">
             <Representation id="ttml"><BaseURL>sub.ttml</BaseURL></Representation>
           </AdaptationSet>"#,
    );
    let server = server_with(manifest, vec![("/sub.ttml", document.as_bytes().to_vec())]);
    let (source, _demuxer) = open_dash(&server);
    let running = start_feed(&source.expect("text track"));
    let messages = running.collect_until(|messages| !all_events(messages).is_empty());
    let events = all_events(&messages);
    let SubtitleContent::Text(cue) = events[0].content() else {
        panic!("text cue expected");
    };
    assert_eq!(
        cue.placement.line,
        subtitle_core::CueLine::Percent(subtitle_core::PercentOfVideo::new(5.0).unwrap()),
        "the cue stays in the upper part of the picture"
    );
    assert_eq!(cue.placement.size.get(), 80.0);
}

#[test]
fn stpp_with_nonzero_pto_in_a_later_period_matches_the_video_mapping() {
    // Media timeline дорожки: PTO = 90 с; реплика на 90.25..90.75 с media-времени
    // внутри Period со стартом 1 с -> на шкале плеера 1.25..1.75 с.
    let manifest = two_period_manifest(
        r#"<AdaptationSet mimeType="application/mp4" codecs="stpp.ttml.im1t" lang="de">
             <SegmentTemplate timescale="1000" duration="1000" startNumber="1"
                 presentationTimeOffset="90000"
                 initialization="t-init.mp4" media="t-$Number$.m4s"/>
             <Representation id="stpp"/>
           </AdaptationSet>"#,
    );
    let sample = ttml_document("00:01:30.250", "00:01:30.750", "Hallo");
    let server = server_with(
        manifest,
        vec![
            ("/t-init.mp4", text_init(b"stpp")),
            ("/t-1.m4s", text_fragment(90_000, 1000, sample.as_bytes())),
        ],
    );
    let (source, mut demuxer) = open_dash(&server);
    let source = source.expect("segmented stpp track");

    // Первый видеопакет второго Period лежит на его старте (1 с): тот же нуль, что у текста.
    let second_period_video = loop {
        let packet = first_video_packet(demuxer.as_mut());
        if packet.pts >= Duration::from_secs(1) {
            break packet.pts;
        }
    };
    assert_eq!(second_period_video, Duration::from_secs(1));

    let running = start_feed(&source);
    let messages = running.collect_until(|messages| !all_events(messages).is_empty());
    let events = all_events(&messages);
    assert_eq!(event_text(events[0]), "Hallo");
    assert_eq!(
        micros(events[0].start()),
        second_period_video.as_micros() as i64 + 250_000
    );
    assert_eq!(
        micros(events[0].end()),
        second_period_video.as_micros() as i64 + 750_000
    );
}

#[test]
fn wvtt_segments_become_cues_with_settings_at_their_sample_time() {
    let manifest = single_period_manifest(
        r#"<AdaptationSet mimeType="application/mp4" codecs="wvtt" lang="fr">
             <Role schemeIdUri="urn:mpeg:dash:role:2011" value="caption"/>
             <SegmentTemplate timescale="1000" duration="1000" startNumber="1"
                 initialization="w-init.mp4" media="w-$Number$.m4s"/>
             <Representation id="wvtt"/>
           </AdaptationSet>"#,
    );
    let server = server_with(
        manifest,
        vec![
            ("/w-init.mp4", text_init(b"wvtt")),
            (
                "/w-1.m4s",
                text_fragment(0, 1000, &wvtt_sample("Bonjour", Some("line:10%"))),
            ),
            (
                "/w-2.m4s",
                text_fragment(1000, 1000, &wvtt_sample("Au revoir", None)),
            ),
        ],
    );
    let (source, _demuxer) = open_dash(&server);
    let source = source.expect("segmented wvtt track");
    assert_eq!(
        &*source.tracks()[0].name,
        "Captions",
        "name comes from the caption role"
    );
    let running = start_feed(&source);
    let messages = running.collect_until(|messages| all_events(messages).len() >= 2);
    let mut events = all_events(&messages);
    events.sort_by_key(|event| micros(event.start()));
    assert_eq!(event_text(events[0]), "Bonjour");
    assert_eq!(
        (micros(events[0].start()), micros(events[0].end())),
        (0, 1_000_000)
    );
    assert_eq!(event_text(events[1]), "Au revoir");
    assert_eq!(micros(events[1].start()), 1_000_000);
    let SubtitleContent::Text(first) = events[0].content() else {
        panic!("text cue expected");
    };
    assert_eq!(
        first.placement.line,
        subtitle_core::CueLine::Percent(subtitle_core::PercentOfVideo::new(10.0).unwrap()),
        "sttg settings reach the cue placement"
    );
}

#[test]
fn a_missing_text_segment_is_skipped_and_the_rest_is_shown_without_giving_up_the_track() {
    let manifest = single_period_manifest(
        r#"<AdaptationSet mimeType="application/mp4" codecs="stpp" lang="en">
             <SegmentTemplate timescale="1000" duration="1000" startNumber="1"
                 initialization="t-init.mp4" media="t-$Number$.m4s"/>
             <Representation id="stpp"/>
           </AdaptationSet>"#,
    );
    // Сегмента 1 на сервере нет (404); сегмент 2 - есть.
    let sample = ttml_document("00:00:01.100", "00:00:01.900", "Second");
    let server = server_with(
        manifest,
        vec![
            ("/t-init.mp4", text_init(b"stpp")),
            ("/t-2.m4s", text_fragment(1000, 1000, sample.as_bytes())),
        ],
    );
    let (source, _demuxer) = open_dash(&server);
    let running = start_feed(&source.expect("track"));
    let messages = running.collect_until(|messages| {
        !all_events(messages).is_empty()
            && messages
                .iter()
                .any(|message| matches!(message, StreamSubtitleMessage::SegmentSkipped { .. }))
    });
    assert!(messages.iter().any(|message| matches!(
        message,
        StreamSubtitleMessage::SegmentSkipped {
            reason: StreamSegmentSkipReason::Failed(_),
            ..
        }
    )));
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, StreamSubtitleMessage::TrackUnavailable { .. })),
        "one missing segment must not make the track unavailable"
    );
    assert_eq!(event_text(all_events(&messages)[0]), "Second");
}

#[test]
fn mpd_without_text_sets_or_with_only_protected_ones_offers_no_subtitle_source() {
    let manifest = single_period_manifest(
        r#"<AdaptationSet mimeType="application/mp4" codecs="wvtt" lang="en">
             <ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/>
             <Representation id="drm"><BaseURL>drm.mp4</BaseURL></Representation>
           </AdaptationSet>"#,
    );
    let server = server_with(manifest, Vec::new());
    let (source, mut demuxer) = open_dash(&server);
    assert!(source.is_none());
    // Видео при этом играет: DRM-набор текста не блокирует A/V.
    assert_eq!(first_video_packet(demuxer.as_mut()).pts, Duration::ZERO);
}

#[test]
fn segment_list_stpp_without_pto_is_normalized_once_per_period() {
    // Адресация без PTO: первый сэмпл сегмента (tfdt) считается началом сегмента на шкале Period.
    let manifest = single_period_manifest(
        r#"<AdaptationSet mimeType="application/mp4" codecs="stpp" lang="en">
             <Representation id="list">
               <SegmentList timescale="1" duration="1">
                 <Initialization sourceURL="l-init.mp4"/>
                 <SegmentURL media="l-0.m4s"/><SegmentURL media="l-1.m4s"/>
               </SegmentList>
             </Representation>
           </AdaptationSet>"#,
    );
    let first = ttml_document("00:00:50.200", "00:00:50.800", "Zero");
    let second = ttml_document("00:00:51.300", "00:00:51.700", "One");
    let server = server_with(
        manifest,
        vec![
            ("/l-init.mp4", text_init(b"stpp")),
            ("/l-0.m4s", text_fragment(50_000, 1000, first.as_bytes())),
            ("/l-1.m4s", text_fragment(51_000, 1000, second.as_bytes())),
        ],
    );
    let (source, _demuxer) = open_dash(&server);
    let running = start_feed(&source.expect("track"));
    let messages = running.collect_until(|messages| all_events(messages).len() >= 2);
    let mut events = all_events(&messages);
    events.sort_by_key(|event| micros(event.start()));
    assert_eq!(micros(events[0].start()), 200_000);
    assert_eq!(micros(events[1].start()), 1_300_000);
}

#[test]
fn single_file_fragmented_wvtt_with_segment_base_is_read_as_a_whole() {
    let manifest = single_period_manifest(
        r#"<AdaptationSet contentType="text" lang="es">
             <Representation id="file" mimeType="application/mp4" codecs="wvtt">
               <BaseURL>captions.mp4</BaseURL>
               <SegmentBase indexRange="0-31"><Initialization range="0-15"/></SegmentBase>
             </Representation>
           </AdaptationSet>"#,
    );
    let file = concat(&[
        text_init(b"wvtt"),
        text_fragment(1000, 1000, &wvtt_sample("Hola", None)),
    ]);
    let server = server_with(manifest, vec![("/captions.mp4", file)]);
    let (source, _demuxer) = open_dash(&server);
    let running = start_feed(&source.expect("track"));
    let messages = running.collect_until(|messages| !all_events(messages).is_empty());
    let events = all_events(&messages);
    assert_eq!(event_text(events[0]), "Hola");
    assert_eq!(micros(events[0].start()), 1_000_000);
}

#[test]
fn epoch_scale_stpp_times_land_on_the_player_timeline() {
    // Живой упаковщик пишет время media timeline как эпоху Unix: 1.7e9 с. PTO равен ей же,
    // поэтому реплика на 1700000000.25..1700000000.75 с media-времени (472222:13:20.250)
    // должна оказаться на 0.25..0.75 с плеера.
    let manifest = single_period_manifest(
        r#"<AdaptationSet mimeType="application/mp4" codecs="stpp.ttml.im1t" lang="en">
             <SegmentTemplate timescale="1000" duration="1000" startNumber="1"
                 presentationTimeOffset="1700000000000"
                 initialization="e-init.mp4" media="e-$Number$.m4s"/>
             <Representation id="epoch"/>
           </AdaptationSet>"#,
    );
    let sample = ttml_document("472222:13:20.250", "472222:13:20.750", "Epoch");
    let server = server_with(
        manifest,
        vec![
            ("/e-init.mp4", text_init(b"stpp")),
            (
                "/e-1.m4s",
                text_fragment(1_700_000_000_000, 1000, sample.as_bytes()),
            ),
        ],
    );
    let (source, _demuxer) = open_dash(&server);
    let running = start_feed(&source.expect("segmented stpp track"));
    let messages = running.collect_until(|messages| !all_events(messages).is_empty());
    let events = all_events(&messages);
    assert_eq!(event_text(events[0]), "Epoch");
    assert_eq!(micros(events[0].start()), 250_000);
    assert_eq!(micros(events[0].end()), 750_000);
}

#[test]
fn sparse_segment_list_stpp_is_not_pulled_to_the_start_of_its_segment() {
    // Нормализация SegmentList делается один раз на Period: сегмент 1 первым определяет
    // смещение (tfdt 50 с = начало), и сегмент 2, чей первый сэмпл начинается на 0.5 с позже
    // своего начала (tfdt 51.5 с при начале 1 с), не должен "прилипать" к началу сегмента.
    let manifest = single_period_manifest(
        r#"<AdaptationSet mimeType="application/mp4" codecs="stpp" lang="en">
             <Representation id="sparse">
               <SegmentList timescale="1" duration="1">
                 <Initialization sourceURL="s-init.mp4"/>
                 <SegmentURL media="s-0.m4s"/><SegmentURL media="s-1.m4s"/>
               </SegmentList>
             </Representation>
           </AdaptationSet>"#,
    );
    let first = ttml_document("00:00:50.200", "00:00:50.800", "Zero");
    let second = ttml_document("00:00:51.600", "00:00:51.900", "One");
    let server = server_with(
        manifest,
        vec![
            ("/s-init.mp4", text_init(b"stpp")),
            ("/s-0.m4s", text_fragment(50_000, 1000, first.as_bytes())),
            ("/s-1.m4s", text_fragment(51_500, 500, second.as_bytes())),
        ],
    );
    let (source, _demuxer) = open_dash(&server);
    let running = start_feed(&source.expect("track"));
    let messages = running.collect_until(|messages| all_events(messages).len() >= 2);
    let mut events = all_events(&messages);
    events.sort_by_key(|event| micros(event.start()));
    assert_eq!(micros(events[0].start()), 200_000);
    assert_eq!(micros(events[1].start()), 1_600_000, "not pulled to 1.1 s");
    assert_eq!(micros(events[1].end()), 1_900_000);
}

#[test]
fn open_ended_stpp_paragraph_ends_with_its_sample_interval() {
    // Абзац без конца в сэмпле 0..1 с действует до конца сэмпла, а не запасные 5 с.
    let manifest = single_period_manifest(
        r#"<AdaptationSet mimeType="application/mp4" codecs="stpp" lang="en">
             <SegmentTemplate timescale="1000" duration="1000" startNumber="1"
                 initialization="o-init.mp4" media="o-$Number$.m4s"/>
             <Representation id="open"/>
           </AdaptationSet>"#,
    );
    let sample = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="0s">Open</p></div></body></tt>"#;
    let server = server_with(
        manifest,
        vec![
            ("/o-init.mp4", text_init(b"stpp")),
            ("/o-1.m4s", text_fragment(0, 1000, sample.as_bytes())),
        ],
    );
    let (source, _demuxer) = open_dash(&server);
    let running = start_feed(&source.expect("track"));
    let messages = running.collect_until(|messages| !all_events(messages).is_empty());
    let events = all_events(&messages);
    assert_eq!(event_text(events[0]), "Open");
    assert_eq!(micros(events[0].start()), 0);
    assert_eq!(micros(events[0].end()), 1_000_000);
}
