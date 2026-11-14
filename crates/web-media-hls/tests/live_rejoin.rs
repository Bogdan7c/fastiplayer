//! 16d3 (решение 333): HLS-эфир, потерявший нить окна, встаёт на живой край и играет дальше.
//!
//! Сервер эфира «уходит вперёд», пока клиент не обновлял плейлист (сон машины, долгий
//! обрыв сети): в следующем плейлисте нет ни одного прежнего сегмента. Раньше движок
//! падал с «live refresh continuity failed». Теперь проверяем через настоящий runtime:
//! - ошибки нет, пакеты идут дальше;
//! - метки времени ни у одной дорожки не идут назад, а продолжение стоит встык с
//!   прочитанным (шкала без прыжка);
//! - после разрыва демуксер читает с точки входа нового окна (hold-back от края),
//!   а не с начала окна;
//! - край эфира на табло плеера ушёл вперёд от точки продолжения на hold-back.

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

#[allow(dead_code)]
mod support;

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use media_core::{
    DemuxReadEvent, Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelinePort,
    DynamicMediaTimelinePortGeneration, Packet,
};
use source_core::{CancellationToken, HttpRequestTarget};
use support::{
    TestQueries, TestServer, adaptive_context, demux_registry, long_muxed_ts_segment, open_policy,
    response,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsLiveOpenRequest, HlsMainTrackLayoutIntent, HlsManifestInput,
    HlsRequestOverrides, HlsRequiredContainer, HlsVariantSelectionIntent, HlsVodOpenRequest,
    prepare_hls_live,
};
use web_media_transport_api::SourceGeneration;

/// Сколько ждать нужного события эфира, прежде чем признать тест упавшим.
const TEST_TIMEOUT: Duration = Duration::from_secs(8);
/// Сегментов в окне эфира: 6 × 1 с, hold-back 3 с.
const WINDOW_SEGMENTS: u64 = 6;
/// Первая media sequence окна «после сна»: далеко за старым окном, общих сегментов нет.
const AFTER_SLEEP_FIRST_SEQUENCE: u64 = 100;
/// Сегмент входа окна «после сна»: три от края (6 − 3).
const AFTER_SLEEP_JOIN_SEQUENCE: u64 = AFTER_SLEEP_FIRST_SEQUENCE + 3;
/// Частота PTS MPEG-TS.
const TS_CLOCK_HZ: u64 = 90_000;

/// Окно эфира из `WINDOW_SEGMENTS` секундных сегментов `s{n}.ts`, начиная с `first`.
fn live_window(first: u64) -> String {
    let mut text = format!("#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{first}\n");
    for sequence in first..first + WINDOW_SEGMENTS {
        text.push_str(&format!("#EXTINF:1,\ns{sequence}.ts\n"));
    }
    text
}

/// Номер сегмента из строки запроса `GET /s{n}.ts HTTP/1.1`.
fn requested_segment(request_line: &str) -> Option<u64> {
    let path = request_line.split_whitespace().nth(1)?;
    path.strip_prefix("/s")?.strip_suffix(".ts")?.parse().ok()
}

/// Секундный сегмент со своими «сырыми» PTS: у настоящего эфира они не совпадают со
/// шкалой плеера, и движок обязан ставить пакеты по своей шкале, а не по ним.
fn segment_body(sequence: u64) -> Vec<u8> {
    long_muxed_ts_segment((1_000 + sequence) * TS_CLOCK_HZ, 1)
}

/// Endpoint не протухает: любой вызов — ошибка теста.
struct UnexpectedRefreshPort;

impl HlsEndpointRefreshPort for UnexpectedRefreshPort {
    fn refresh(
        &self,
        _request: HlsEndpointRefreshRequest,
    ) -> Result<HlsEndpointRefreshReply, HlsEndpointRefreshError> {
        Err(HlsEndpointRefreshError::AttemptsExhausted)
    }
}

/// Замена протухшей ссылки: отдаёт свежий адрес плейлиста следующего поколения.
struct FreshEndpointPort {
    fresh_target: HttpRequestTarget,
    calls: Arc<AtomicUsize>,
}

impl HlsEndpointRefreshPort for FreshEndpointPort {
    fn refresh(
        &self,
        request: HlsEndpointRefreshRequest,
    ) -> Result<HlsEndpointRefreshReply, HlsEndpointRefreshError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let generation = SourceGeneration::new(request.previous_generation.value() + 1);
        Ok(HlsEndpointRefreshReply {
            http: adaptive_context(
                &self.fresh_target,
                CancellationToken::new(),
                generation,
                TestQueries::default(),
            ),
            generation,
            manifest: HlsManifestInput::Fetch {
                selected_url: self.fresh_target.clone(),
            },
            overrides: HlsRequestOverrides::new(None),
        })
    }
}

fn live_request(
    target: HttpRequestTarget,
    endpoint_refresh: Arc<dyn HlsEndpointRefreshPort>,
) -> HlsLiveOpenRequest {
    let generation = SourceGeneration::new(1);
    HlsLiveOpenRequest {
        common: HlsVodOpenRequest {
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
        },
        endpoint_refresh,
        timeline_port_generation: DynamicMediaTimelinePortGeneration::new(
            NonZeroU64::new(1).expect("non-zero test port generation"),
        ),
        initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
        network_reconnect: source_core::HttpReconnectPolicy::disabled(),
    }
}

/// Что прочитал демуксер: все пакеты по порядку.
struct ReadLog {
    packets: Vec<Packet>,
}

impl ReadLog {
    /// Метки времени каждой дорожки не убывают: склейка не отматывает шкалу назад.
    fn assert_timestamps_never_go_back(&self) {
        let mut last_by_track = HashMap::new();
        for packet in &self.packets {
            let previous = last_by_track.insert(packet.track_id, packet.pts);
            assert!(
                previous.is_none_or(|previous| previous <= packet.pts),
                "track {:?} went back from {previous:?} to {:?}",
                packet.track_id,
                packet.pts
            );
        }
    }

    /// Самый ранний пакет не раньше `from`.
    fn first_packet_from(&self, from: Duration) -> Option<&Packet> {
        self.packets
            .iter()
            .filter(|packet| packet.pts >= from)
            .min_by_key(|packet| packet.pts)
    }

    /// Самый поздний пакет раньше `before`.
    fn last_packet_before(&self, before: Duration) -> Option<&Packet> {
        self.packets
            .iter()
            .filter(|packet| packet.pts < before)
            .max_by_key(|packet| packet.pts)
    }
}

/// Читает эфир, пока шкала пакетов не дойдёт до `until`; любая ошибка эфира — провал.
fn read_until(demuxer: &mut dyn Demuxer, until: Duration) -> ReadLog {
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut packets = Vec::new();
    loop {
        assert!(
            Instant::now() < deadline,
            "the live stream stalled: read {} packets, last at {:?}",
            packets.len(),
            packets.last().map(|packet: &Packet| packet.pts)
        );
        match demuxer
            .next_event()
            .expect("the live stream must survive the lost window")
        {
            DemuxReadEvent::Packet(packet) => {
                let reached = packet.pts >= until;
                packets.push(packet);
                if reached {
                    return ReadLog { packets };
                }
            }
            DemuxReadEvent::EndOfStream => panic!("a live stream without ENDLIST never ends"),
            _ => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

/// Край эфира и точка входа по последнему опубликованному табло.
fn published_edge(timeline_port: &DynamicMediaTimelinePort) -> Duration {
    timeline_port
        .observe()
        .snapshot
        .state
        .as_live()
        .expect("the stream stays live")
        .live_edge()
        .as_duration()
}

/// Сервер эфира: старое окно 0..6, пока клиент не дочитает его до края, потом —
/// окно «после сна», в котором нет ни одного старого сегмента.
struct SleepingLiveServer {
    server: TestServer,
    requested_segments: Arc<Mutex<Vec<u64>>>,
}

impl SleepingLiveServer {
    fn start(playlist_path: &'static str) -> Self {
        let requested_segments = Arc::new(Mutex::new(Vec::<u64>::new()));
        let handler_segments = Arc::clone(&requested_segments);
        let server = TestServer::start(move |_, request| {
            if let Some(sequence) = requested_segment(&request.request_line) {
                handler_segments.lock().expect("request log").push(sequence);
                return response("200 OK", &[], &segment_body(sequence));
            }
            if request
                .request_line
                .starts_with(&format!("GET {playlist_path} "))
            {
                let old_window_read_to_edge = handler_segments
                    .lock()
                    .expect("request log")
                    .contains(&(WINDOW_SEGMENTS - 1));
                let first = if old_window_read_to_edge {
                    AFTER_SLEEP_FIRST_SEQUENCE
                } else {
                    0
                };
                return response("200 OK", &[], live_window(first).as_bytes());
            }
            response("404 Not Found", &[], b"")
        });
        Self {
            server,
            requested_segments,
        }
    }

    fn segments_after_sleep(&self) -> Vec<u64> {
        self.requested_segments
            .lock()
            .expect("request log")
            .iter()
            .copied()
            .filter(|sequence| *sequence >= AFTER_SLEEP_FIRST_SEQUENCE)
            .collect()
    }
}

#[test]
fn a_live_window_that_moved_past_everything_known_rejoins_at_the_live_edge() {
    let live = SleepingLiveServer::start("/live.m3u8");
    let opened = prepare_hls_live(live_request(
        live.server.target("/live.m3u8"),
        Arc::new(UnexpectedRefreshPort),
    ))
    .expect("prepare live stream");
    let (mut demuxer, timeline_port, _) = opened.into_parts();
    let old_edge = Duration::from_secs(WINDOW_SEGMENTS);

    // Читаем через разрыв: старое окно кончается на 6 с, после склейки — ещё 2 с эфира.
    let read = read_until(demuxer.as_mut(), old_edge + Duration::from_secs(2));

    read.assert_timestamps_never_go_back();
    // Продолжение стоит встык с прочитанным: первый пакет нового окна — в первой же
    // секунде после старого края, шкала не прыгнула на время сна.
    let resumed = read
        .first_packet_from(old_edge)
        .expect("packets after the gap");
    assert!(
        resumed.pts < old_edge + Duration::from_millis(500),
        "the stream resumed at {:?}, far from where reading stopped ({old_edge:?})",
        resumed.pts
    );
    let before_gap = read
        .last_packet_before(old_edge)
        .expect("packets before the gap");
    assert!(before_gap.pts >= old_edge - Duration::from_secs(1));
    // Новое окно читается с точки входа (три сегмента от края), а не с его начала.
    let after_sleep = live.segments_after_sleep();
    assert_eq!(after_sleep.first(), Some(&AFTER_SLEEP_JOIN_SEQUENCE));
    assert!(
        after_sleep
            .iter()
            .all(|sequence| *sequence >= AFTER_SLEEP_JOIN_SEQUENCE),
        "segments before the join point were read: {after_sleep:?}"
    );
    // Табло: край ушёл от точки продолжения ровно на hold-back.
    assert_eq!(
        published_edge(&timeline_port),
        old_edge + Duration::from_secs(3)
    );
}

#[test]
fn an_expired_endpoint_whose_fresh_playlist_lost_the_window_rejoins_too() {
    // Старый адрес отдаёт окно 0..6, а последний сегмент старого окна уже протух (404).
    // Свежий адрес отдаёт окно «после сна»: общих сегментов нет.
    let requested_segments = Arc::new(Mutex::new(Vec::<u64>::new()));
    let handler_segments = Arc::clone(&requested_segments);
    let server = TestServer::start(move |_, request| {
        if let Some(sequence) = requested_segment(&request.request_line) {
            handler_segments.lock().expect("request log").push(sequence);
            if sequence == WINDOW_SEGMENTS - 1 {
                return response("404 Not Found", &[], b"");
            }
            return response("200 OK", &[], &segment_body(sequence));
        }
        if request.request_line.starts_with("GET /fresh.m3u8 ") {
            return response(
                "200 OK",
                &[],
                live_window(AFTER_SLEEP_FIRST_SEQUENCE).as_bytes(),
            );
        }
        if request.request_line.starts_with("GET /stale.m3u8 ") {
            return response("200 OK", &[], live_window(0).as_bytes());
        }
        response("404 Not Found", &[], b"")
    });
    let endpoint_calls = Arc::new(AtomicUsize::new(0));
    let opened = prepare_hls_live(live_request(
        server.target("/stale.m3u8"),
        Arc::new(FreshEndpointPort {
            fresh_target: server.target("/fresh.m3u8"),
            calls: Arc::clone(&endpoint_calls),
        }),
    ))
    .expect("prepare live stream");
    let (mut demuxer, timeline_port, _) = opened.into_parts();
    // Протухший сегмент 5 не прочитан: дочитано до конца сегмента 4, до 5 с.
    let read_to = Duration::from_secs(WINDOW_SEGMENTS - 1);

    let read = read_until(demuxer.as_mut(), read_to + Duration::from_secs(2));

    read.assert_timestamps_never_go_back();
    assert_eq!(endpoint_calls.load(Ordering::SeqCst), 1);
    let resumed = read
        .first_packet_from(read_to)
        .expect("packets after the gap");
    assert!(
        resumed.pts < read_to + Duration::from_millis(500),
        "the stream resumed at {:?}, far from where reading stopped ({read_to:?})",
        resumed.pts
    );
    let after_sleep: Vec<u64> = requested_segments
        .lock()
        .expect("request log")
        .iter()
        .copied()
        .filter(|sequence| *sequence >= AFTER_SLEEP_FIRST_SEQUENCE)
        .collect();
    assert_eq!(after_sleep.first(), Some(&AFTER_SLEEP_JOIN_SEQUENCE));
    // Замена ссылки — новая эпоха источника, как и до 16d3.
    let observed = timeline_port.observe();
    assert_eq!(
        observed.snapshot.source_epoch,
        DynamicMediaTimelineEpoch::new(1)
    );
    assert_eq!(
        published_edge(&timeline_port),
        read_to + Duration::from_secs(3)
    );
}

/// Первая media sequence второго окна «после сна».
const SECOND_SLEEP_FIRST_SEQUENCE: u64 = 200;
/// Сегмент входа второго окна «после сна».
const SECOND_SLEEP_JOIN_SEQUENCE: u64 = SECOND_SLEEP_FIRST_SEQUENCE + 3;

#[test]
fn a_second_lost_window_before_anything_was_read_does_not_move_the_stream_back() {
    // Первое окно «после сна» недоступно (сеть едва поднялась, 503), и пока движок ждёт
    // сеть, окно снова уходит целиком вперёд. Из первого склеенного окна не прочитано
    // ничего: вторая склейка обязана продолжить с того же места, а не откатить окно к
    // позиции первого входа (там демуксеру нечего читать, и эфир встал бы навсегда).
    let requested_segments = Arc::new(Mutex::new(Vec::<u64>::new()));
    let handler_segments = Arc::clone(&requested_segments);
    let server = TestServer::start(move |_, request| {
        if let Some(sequence) = requested_segment(&request.request_line) {
            handler_segments.lock().expect("request log").push(sequence);
            let first_sleep = AFTER_SLEEP_FIRST_SEQUENCE..SECOND_SLEEP_FIRST_SEQUENCE;
            if first_sleep.contains(&sequence) {
                return response("503 Service Unavailable", &[], b"");
            }
            return response("200 OK", &[], &segment_body(sequence));
        }
        if request.request_line.starts_with("GET /live.m3u8 ") {
            let requested = handler_segments.lock().expect("request log");
            let first = if requested
                .iter()
                .any(|sequence| *sequence >= AFTER_SLEEP_FIRST_SEQUENCE)
            {
                SECOND_SLEEP_FIRST_SEQUENCE
            } else if requested.contains(&(WINDOW_SEGMENTS - 1)) {
                AFTER_SLEEP_FIRST_SEQUENCE
            } else {
                0
            };
            return response("200 OK", &[], live_window(first).as_bytes());
        }
        response("404 Not Found", &[], b"")
    });
    let mut request = live_request(server.target("/live.m3u8"), Arc::new(UnexpectedRefreshPort));
    request.network_reconnect =
        source_core::HttpReconnectPolicy::with_budget(Duration::from_secs(30));
    let opened = prepare_hls_live(request).expect("prepare live stream");
    let (mut demuxer, timeline_port, _) = opened.into_parts();
    let old_edge = Duration::from_secs(WINDOW_SEGMENTS);

    let read = read_until(demuxer.as_mut(), old_edge + Duration::from_secs(2));

    read.assert_timestamps_never_go_back();
    let resumed = read
        .first_packet_from(old_edge)
        .expect("packets after the gaps");
    assert!(
        resumed.pts < old_edge + Duration::from_millis(500),
        "the stream resumed at {:?}, far from where reading stopped ({old_edge:?})",
        resumed.pts
    );
    let second_window: Vec<u64> = requested_segments
        .lock()
        .expect("request log")
        .iter()
        .copied()
        .filter(|sequence| *sequence >= SECOND_SLEEP_FIRST_SEQUENCE)
        .collect();
    assert_eq!(second_window.first(), Some(&SECOND_SLEEP_JOIN_SEQUENCE));
    assert_eq!(
        published_edge(&timeline_port),
        old_edge + Duration::from_secs(3)
    );
}
