//! 16d3 (решение 334): HLS-эфир переживает обрыв сети, а не падает на первом сбое.
//!
//! После пробуждения машины сеть поднимается секундами; бывает и обрыв посреди эфира.
//! Раньше у эфира было лишь три быстрые попытки транспорта (~0,3 с), потом роковая
//! ошибка. Теперь плейлист и сегменты ждут сеть по общему правилу плеера (паузы
//! 0,5 → 8 с, бюджет `network.reconnect_wait_ms`). Проверяем через настоящий runtime:
//! - плейлист недоступен несколько секунд — эфир ждёт и идёт дальше;
//! - сегмент оборвался посреди тела — повтор без дублей и без дыр;
//! - сегмент отвечает 503 дольше, чем повторяет транспорт, — эфир дожидается его;
//! - сеть не вернулась за бюджет — честная ошибка, а не вечное ожидание;
//! - выключенное ожидание (`0`) сохраняет прежнее поведение «сразу ошибка».

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
use std::io::Write;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use media_core::{
    DemuxReadEvent, Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelinePortGeneration, Packet,
};
use source_core::{CancellationToken, HttpReconnectPolicy, HttpRequestTarget};
use support::{
    TestQueries, TestServer, adaptive_context, demux_registry,
    large_muxed_ts_segment_with_early_landing, long_muxed_ts_segment, open_policy, response,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsLiveOpenRequest, HlsMainTrackLayoutIntent, HlsManifestInput,
    HlsRequestOverrides, HlsRequiredContainer, HlsVariantSelectionIntent, HlsVodOpenRequest,
    prepare_hls_live,
};
use web_media_transport_api::SourceGeneration;

/// Предел ожидания одного теста: больше любого бюджета ниже.
const TEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Сегментов в окне: 6 × 1 с.
const WINDOW_SEGMENTS: u64 = 6;
/// Бюджет ожидания сети в тестах, где сеть возвращается.
const GENEROUS_BUDGET: Duration = Duration::from_secs(15);
/// Сколько длится обрыв плейлиста в тесте «сеть вернулась».
const PLAYLIST_OUTAGE: Duration = Duration::from_millis(2_500);
/// Частота PTS MPEG-TS.
const TS_CLOCK_HZ: u64 = 90_000;

/// Окно эфира `s{first}..s{first+5}`.
fn live_window(first: u64) -> String {
    let mut text = format!("#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{first}\n");
    for sequence in first..first + WINDOW_SEGMENTS {
        text.push_str(&format!("#EXTINF:1,\ns{sequence}.ts\n"));
    }
    text
}

fn requested_segment(request_line: &str) -> Option<u64> {
    let path = request_line.split_whitespace().nth(1)?;
    path.strip_prefix("/s")?.strip_suffix(".ts")?.parse().ok()
}

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

fn live_request(
    target: HttpRequestTarget,
    network_reconnect: HttpReconnectPolicy,
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
        endpoint_refresh: Arc::new(UnexpectedRefreshPort),
        timeline_port_generation: DynamicMediaTimelinePortGeneration::new(
            NonZeroU64::new(1).expect("non-zero test port generation"),
        ),
        initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
        network_reconnect,
    }
}

/// Как сервер ведёт себя с плейлистом после того, как клиент дочитал первое окно.
#[derive(Clone, Copy)]
enum PlaylistOutage {
    /// Сеть не пропадала.
    None,
    /// Плейлист отвечает 503 столько времени, потом сеть возвращается.
    For(Duration),
    /// Плейлист больше не отвечает никогда.
    Forever,
}

/// Эфир, где окно сдвигается на сегмент с каждым успешным плейлистом.
struct OutageServer {
    server: TestServer,
    playlist_failures: Arc<AtomicUsize>,
    segment_requests: Arc<Mutex<Vec<u64>>>,
}

impl OutageServer {
    /// `broken_segment` — сегмент, который первые `segment_failures` раз срывается:
    /// `cut_body` — обрыв посреди тела, иначе 503.
    fn start(
        playlist_outage: PlaylistOutage,
        broken_segment: Option<(u64, usize, SegmentFailure)>,
    ) -> Self {
        let playlist_failures = Arc::new(AtomicUsize::new(0));
        let served_playlists = Arc::new(AtomicUsize::new(0));
        let segment_requests = Arc::new(Mutex::new(Vec::<u64>::new()));
        let outage_started = Arc::new(Mutex::new(None::<Instant>));
        let handler_failures = Arc::clone(&playlist_failures);
        let handler_segments = Arc::clone(&segment_requests);
        let server = TestServer::start_streaming(move |_, request, stream| {
            if let Some(sequence) = requested_segment(&request.request_line) {
                let attempt = {
                    let mut requests = handler_segments.lock().expect("request log");
                    requests.push(sequence);
                    requests.iter().filter(|seen| **seen == sequence).count()
                };
                if sequence == WINDOW_SEGMENTS - 1 {
                    outage_started
                        .lock()
                        .expect("outage clock")
                        .get_or_insert_with(Instant::now);
                }
                let body = match broken_segment {
                    // Обрыв посреди тела проверяем на большом сегменте: его первые пакеты
                    // плеер получает раньше, чем сеть рвётся (пропуск уже отданного).
                    Some((broken, _, SegmentFailure::CutBody)) if broken == sequence => {
                        large_muxed_ts_segment_with_early_landing((1_000 + sequence) * TS_CLOCK_HZ)
                    }
                    _ => segment_body(sequence),
                };
                let reply = match broken_segment {
                    Some((broken, failures, failure))
                        if broken == sequence && attempt <= failures =>
                    {
                        failure.reply(&body)
                    }
                    _ => response("200 OK", &[], &body),
                };
                let _ = stream.write_all(&reply);
                return;
            }
            if request.request_line.starts_with("GET /live.m3u8 ") {
                let started = *outage_started.lock().expect("outage clock");
                let in_outage = match (playlist_outage, started) {
                    (PlaylistOutage::For(length), Some(started)) => started.elapsed() < length,
                    (PlaylistOutage::Forever, Some(_)) => true,
                    _ => false,
                };
                if in_outage {
                    handler_failures.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.write_all(&response("503 Service Unavailable", &[], b""));
                    return;
                }
                let first = served_playlists.fetch_add(1, Ordering::SeqCst) as u64;
                let _ = stream.write_all(&response("200 OK", &[], live_window(first).as_bytes()));
                return;
            }
            let _ = stream.write_all(&response("404 Not Found", &[], b""));
        });
        Self {
            server,
            playlist_failures,
            segment_requests,
        }
    }

    fn attempts_of(&self, sequence: u64) -> usize {
        self.segment_requests
            .lock()
            .expect("request log")
            .iter()
            .filter(|seen| **seen == sequence)
            .count()
    }
}

/// Как срывается сломанный сегмент.
#[derive(Clone, Copy)]
enum SegmentFailure {
    /// Заголовки обещают всё тело, приходит половина, соединение рвётся.
    CutBody,
    /// Сервер временно недоступен.
    Unavailable,
}

impl SegmentFailure {
    fn reply(self, body: &[u8]) -> Vec<u8> {
        match self {
            Self::CutBody => {
                let mut full = response("200 OK", &[], body);
                full.truncate(full.len() - body.len() / 2);
                full
            }
            Self::Unavailable => response("503 Service Unavailable", &[], b""),
        }
    }
}

/// Все пакеты, прочитанные до `until` по шкале; ошибка эфира — провал теста.
fn read_until(demuxer: &mut dyn Demuxer, until: Duration) -> Vec<Packet> {
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut packets = Vec::new();
    loop {
        assert!(
            Instant::now() < deadline,
            "the live stream stalled at {:?}",
            packets.last().map(|packet: &Packet| packet.pts)
        );
        match demuxer
            .next_event()
            .expect("the live stream must survive the network outage")
        {
            DemuxReadEvent::Packet(packet) => {
                let reached = packet.pts >= until;
                packets.push(packet);
                if reached {
                    return packets;
                }
            }
            DemuxReadEvent::EndOfStream => panic!("a live stream without ENDLIST never ends"),
            _ => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

/// Читает, пока эфир не выдаст ошибку; возвращает её текст и когда она пришла.
fn read_until_failure(demuxer: &mut dyn Demuxer) -> (String, Instant) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        assert!(Instant::now() < deadline, "the stream never gave up");
        match demuxer.next_event() {
            Err(error) => return (format!("{error:#}"), Instant::now()),
            Ok(DemuxReadEvent::Packet(_)) => {}
            Ok(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

/// Метки каждой дорожки строго растут: ни одного пакета дважды.
fn assert_no_duplicates(packets: &[Packet]) {
    let mut last_by_track = HashMap::new();
    for packet in packets {
        if let Some(previous) = last_by_track.insert(packet.track_id, packet.pts) {
            assert!(
                previous < packet.pts,
                "track {:?} repeated or went back: {previous:?} then {:?}",
                packet.track_id,
                packet.pts
            );
        }
    }
}

/// Пакеты одной секунды шкалы `[second, second + 1)`.
fn packets_in_second(packets: &[Packet], second: u64) -> usize {
    let start = Duration::from_secs(second);
    let end = start + Duration::from_secs(1);
    packets
        .iter()
        .filter(|packet| packet.pts >= start && packet.pts < end)
        .count()
}

#[test]
fn a_playlist_outage_of_a_few_seconds_is_waited_out() {
    let live = OutageServer::start(PlaylistOutage::For(PLAYLIST_OUTAGE), None);
    let opened = prepare_hls_live(live_request(
        live.server.target("/live.m3u8"),
        HttpReconnectPolicy::with_budget(GENEROUS_BUDGET),
    ))
    .expect("prepare live stream");
    let (mut demuxer, _timeline_port, _) = opened.into_parts();

    // Старое окно кончается на 6 с; дальше эфир есть только после возвращения сети.
    let packets = read_until(demuxer.as_mut(), Duration::from_secs(WINDOW_SEGMENTS + 1));

    assert_no_duplicates(&packets);
    // Обрыв действительно был и пережит повторами, а не обойдён.
    assert!(
        live.playlist_failures.load(Ordering::SeqCst) >= 2,
        "the playlist outage was not exercised"
    );
    assert!(packets_in_second(&packets, WINDOW_SEGMENTS) > 0);
}

/// Пакеты эфира до `until` с заданным поведением сломанного сегмента.
fn read_with_broken_segment(
    broken_segment: (u64, usize, SegmentFailure),
    until: Duration,
) -> (Vec<Packet>, usize) {
    let live = OutageServer::start(PlaylistOutage::None, Some(broken_segment));
    let opened = prepare_hls_live(live_request(
        live.server.target("/live.m3u8"),
        HttpReconnectPolicy::with_budget(GENEROUS_BUDGET),
    ))
    .expect("prepare live stream");
    let (mut demuxer, _timeline_port, _) = opened.into_parts();
    let packets = read_until(demuxer.as_mut(), until);
    (packets, live.attempts_of(broken_segment.0))
}

#[test]
fn a_segment_cut_mid_body_is_retried_without_duplicates_or_holes() {
    let until = Duration::from_secs(WINDOW_SEGMENTS);
    // Контроль: тот же большой сегмент 4 без обрывов.
    let (control, _) = read_with_broken_segment((4, 0, SegmentFailure::CutBody), until);
    // Сегмент 4 дважды обрывается посреди тела, после уже отданных первых пакетов.
    let (packets, attempts) = read_with_broken_segment((4, 2, SegmentFailure::CutBody), until);

    assert!(attempts > 2, "the cut segment was never retried");
    assert_no_duplicates(&packets);
    // Сегмент 4 отдан ровно один раз и целиком: как в контроле, без дыр и дублей.
    assert!(packets_in_second(&control, 4) > 1);
    assert_eq!(
        packets_in_second(&packets, 4),
        packets_in_second(&control, 4)
    );
}

#[test]
fn a_segment_lost_for_longer_than_the_window_is_skipped_not_fatal() {
    // Сегмент 4 недоступен дольше, чем живёт окно: сервер сдвигает окно на сегмент с
    // каждым плейлистом, и за паузы 0,5 + 1 + 2 + 4 с сегмент уходит из окна. Эфир не
    // падает и продолжает с первого сегмента окна после прочитанного.
    let (packets, attempts) = read_with_broken_segment(
        (4, usize::MAX, SegmentFailure::Unavailable),
        Duration::from_secs(WINDOW_SEGMENTS + 1),
    );

    assert!(attempts > 1);
    assert_no_duplicates(&packets);
    assert_eq!(packets_in_second(&packets, 4), 0);
    assert!(
        packets
            .iter()
            .any(|packet| packet.pts >= Duration::from_secs(5))
    );
}

#[test]
fn a_segment_unavailable_longer_than_transport_retries_is_waited_for() {
    let live = OutageServer::start(
        PlaylistOutage::None,
        Some((4, 2, SegmentFailure::Unavailable)),
    );
    let opened = prepare_hls_live(live_request(
        live.server.target("/live.m3u8"),
        HttpReconnectPolicy::with_budget(GENEROUS_BUDGET),
    ))
    .expect("prepare live stream");
    let (mut demuxer, _timeline_port, _) = opened.into_parts();

    let packets = read_until(demuxer.as_mut(), Duration::from_secs(WINDOW_SEGMENTS));

    assert!(live.attempts_of(4) > 2);
    assert_no_duplicates(&packets);
    assert_eq!(
        packets_in_second(&packets, 4),
        packets_in_second(&packets, 3)
    );
}

#[test]
fn a_network_that_never_returns_ends_the_stream_after_the_budget() {
    let budget = Duration::from_millis(1_500);
    let live = OutageServer::start(PlaylistOutage::Forever, None);
    let opened = prepare_hls_live(live_request(
        live.server.target("/live.m3u8"),
        HttpReconnectPolicy::with_budget(budget),
    ))
    .expect("prepare live stream");
    let (mut demuxer, _timeline_port, _) = opened.into_parts();
    let opened_at = Instant::now();

    let (error, failed_at) = read_until_failure(demuxer.as_mut());

    assert!(
        error.contains("manifest refresh failed"),
        "unexpected error: {error}"
    );
    // Первый плейлист после окна ждётся через секунду, потом — весь бюджет ожидания.
    assert!(
        failed_at.duration_since(opened_at) >= budget,
        "the stream gave up before the network budget: {:?}",
        failed_at.duration_since(opened_at)
    );
    assert!(live.playlist_failures.load(Ordering::SeqCst) >= 2);
}

#[test]
fn a_disabled_network_wait_fails_on_the_first_outage_as_before() {
    let live = OutageServer::start(PlaylistOutage::Forever, None);
    let opened = prepare_hls_live(live_request(
        live.server.target("/live.m3u8"),
        HttpReconnectPolicy::disabled(),
    ))
    .expect("prepare live stream");
    let (mut demuxer, _timeline_port, _) = opened.into_parts();
    let opened_at = Instant::now();

    let (error, failed_at) = read_until_failure(demuxer.as_mut());

    assert!(
        error.contains("manifest refresh failed"),
        "unexpected error: {error}"
    );
    // Первая попытка обновления — через target duration (1 с), дальше без ожидания.
    assert!(failed_at.duration_since(opened_at) < Duration::from_millis(1_500));
}
