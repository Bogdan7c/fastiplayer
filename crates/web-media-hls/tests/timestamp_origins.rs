//! Hermetic evidence: опубликованный origin эпохи равен смещению, которое демуксер реально
//! вычел из пакетов (VOD, discontinuity, seek, live).

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

use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use media_core::{
    DemuxReadEvent, DemuxSeekRequest, Demuxer, DynamicMediaTimelineEpoch,
    DynamicMediaTimelinePortGeneration, Packet, TrackKind,
};
use source_core::CancellationToken;
use support::{
    TestQueries, TestServer, adaptive_context, demux_registry, muxed_ts, open_policy, response,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsEpochTimestampOrigin, HlsLiveOpenRequest,
    HlsMainTrackLayoutIntent, HlsManifestInput, HlsRequestOverrides, HlsRequiredContainer,
    HlsTimestampOrigins, HlsVariantSelectionIntent, HlsVodOpenRequest, SecretInlineMediaPlaylist,
    prepare_hls_live, prepare_hls_vod,
};
use web_media_transport_api::SourceGeneration;

const TEST_TIMEOUT: Duration = Duration::from_secs(4);
/// Допуск на округление тиков 90 кГц в наносекунды.
const TOLERANCE: Duration = Duration::from_micros(2);

/// Фикстура `muxed_ts(pts)`: видео PTS = pts, DTS = pts - 3000 тиков, аудио PTS = pts.
/// Первый пакет эпохи - видео, поэтому origin = его DTS.
const VIDEO_DTS_LEAD_TICKS: u64 = 3_000;

fn from_ticks(ticks: u64) -> Duration {
    Duration::from_nanos(ticks * 1_000_000_000 / 90_000)
}

fn assert_close(actual: Duration, expected: Duration, what: &str) {
    assert!(
        actual.abs_diff(expected) <= TOLERANCE,
        "{what}: {actual:?} differs from expected {expected:?}"
    );
}

/// Ожидаемое время плеера сырой метки при заданной паре origin.
fn player_time(raw: Duration, origin: HlsEpochTimestampOrigin) -> Duration {
    raw.saturating_sub(origin.media_origin()) + origin.timeline_start()
}

/// Сырой PTS пакета фикстуры `muxed_ts(fixture_pts_ticks)`: и видео, и аудио несут PTS фикстуры.
fn raw_pts(fixture_pts_ticks: u64) -> Duration {
    from_ticks(fixture_pts_ticks)
}

/// Сырой origin, который обязан зафиксировать демуксер: min(PTS, DTS) первого пакета эпохи.
/// Считается из сырых меток фикстуры по виду ПЕРВОГО демуксированного пакета, а не из опубликованных данных.
fn expected_origin_for_first_packet(first: &Packet, fixture_pts_ticks: u64) -> Duration {
    if first.kind == TrackKind::Video {
        from_ticks(fixture_pts_ticks - VIDEO_DTS_LEAD_TICKS)
    } else {
        from_ticks(fixture_pts_ticks)
    }
}

/// Проверяет, что демуксированные пакеты равны `raw - origin + timeline_start` для сырых меток фикстуры.
fn assert_packets_match_origin(
    packets: &[Packet],
    fixture_pts_ticks: u64,
    origin: HlsEpochTimestampOrigin,
) {
    assert!(!packets.is_empty());
    for packet in packets {
        let raw = raw_pts(fixture_pts_ticks);
        assert_close(
            packet.pts,
            player_time(raw, origin),
            "demuxed packet PTS must equal raw - published origin + timeline start",
        );
        if packet.kind == TrackKind::Video {
            let raw_dts = from_ticks(fixture_pts_ticks - VIDEO_DTS_LEAD_TICKS);
            assert_close(
                packet.dts.expect("video DTS"),
                player_time(raw_dts, origin),
                "demuxed video DTS",
            );
        }
    }
}

fn next_ready_event(demuxer: &mut dyn Demuxer) -> DemuxReadEvent {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match demuxer.next_event().expect("HLS stays readable") {
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                assert!(Instant::now() < deadline, "HLS worker timed out");
                std::thread::sleep(Duration::from_millis(2));
            }
            event => return event,
        }
    }
}

fn read_packets_until_eos(demuxer: &mut dyn Demuxer) -> Vec<Packet> {
    let mut packets = Vec::new();
    loop {
        match next_ready_event(demuxer) {
            DemuxReadEvent::Packet(packet) => packets.push(packet),
            DemuxReadEvent::EndOfStream => return packets,
            _ => {}
        }
    }
}

fn muxed_selection() -> HlsVariantSelectionIntent {
    HlsVariantSelectionIntent {
        resolution: None,
        codecs: None,
        audio: HlsAudioLayoutIntent::Muxed,
        main_track_layout: HlsMainTrackLayoutIntent::MuxedAv,
    }
}

fn vod_inline_request(server: &TestServer, playlist: &str) -> HlsVodOpenRequest {
    let generation = SourceGeneration::new(1);
    let target = server.target("/inline.m3u8");
    HlsVodOpenRequest {
        http: adaptive_context(
            &target,
            CancellationToken::new(),
            generation,
            TestQueries::default(),
        ),
        generation,
        manifest: HlsManifestInput::InlineMedia {
            selected_url: target,
            playlist: SecretInlineMediaPlaylist::new(playlist),
        },
        selection: muxed_selection(),
        overrides: HlsRequestOverrides::new(None),
        containers: HlsComponentContainerIntent {
            main: HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream),
            alternate_audio: None,
        },
        demux_registry: demux_registry(),
        policy: open_policy(),
    }
}

fn segment_server(segments: Vec<(&'static str, Vec<u8>)>) -> TestServer {
    let segments = Arc::new(segments);
    TestServer::start(move |_, request| {
        segments
            .iter()
            .find_map(|(name, bytes)| {
                request
                    .request_line
                    .contains(name)
                    .then(|| response("200 OK", &[], bytes))
            })
            .unwrap_or_else(|| response("404 Not Found", &[], b""))
    })
}

#[test]
fn vod_with_nonzero_first_pts_publishes_the_offset_it_subtracts() {
    const FIRST_PTS: u64 = 900_000; // 10 s: сырые метки НЕ начинаются с нуля
    let server = segment_server(vec![("/seg.ts", muxed_ts(FIRST_PTS))]);
    let opened = prepare_hls_vod(vod_inline_request(
        &server,
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\nseg.ts\n#EXT-X-ENDLIST\n",
    ))
    .expect("prepare TS VOD");
    let origins = opened.timestamp_origins();
    assert_eq!(
        origins.epoch_origin(0),
        None,
        "nothing published before demux"
    );
    let mut demuxer = opened.into_demuxer();
    let packets = read_packets_until_eos(&mut *demuxer);

    let origin = origins
        .epoch_origin(0)
        .expect("origin of epoch 0 published");
    // Сырой origin - реальный DTS первого пакета (9.9667 с), а не ноль и не PTS.
    assert_close(
        origin.media_origin(),
        expected_origin_for_first_packet(&packets[0], FIRST_PTS),
        "published media origin",
    );
    assert!(
        origin.media_origin() > Duration::from_secs(9),
        "origin is the raw container time"
    );
    assert_eq!(origin.timeline_start(), Duration::ZERO);
    assert_packets_match_origin(&packets, FIRST_PTS, origin);
    assert_eq!(origins.known_epoch_count(), 1);
}

#[test]
fn each_discontinuity_epoch_gets_its_own_origin_and_seek_keeps_it_consistent() {
    const FIRST_PTS: u64 = 900_000; // 10 s
    const SECOND_PTS: u64 = 4_500_000; // 50 s после разрыва
    let server = segment_server(vec![
        ("/one.ts", muxed_ts(FIRST_PTS)),
        ("/two.ts", muxed_ts(SECOND_PTS)),
    ]);
    let opened = prepare_hls_vod(vod_inline_request(
        &server,
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\none.ts\n\
         #EXT-X-DISCONTINUITY\n#EXTINF:1,\ntwo.ts\n#EXT-X-ENDLIST\n",
    ))
    .expect("prepare discontinuous TS VOD");
    let origins = opened.timestamp_origins();
    let mut demuxer = opened.into_demuxer();
    let packets = read_packets_until_eos(&mut *demuxer);

    let first = origins.epoch_origin(0).expect("epoch 0 origin");
    let second = origins.epoch_origin(1).expect("epoch 1 origin");
    assert_ne!(first.media_origin(), second.media_origin());
    let first_epoch_packet = packets
        .iter()
        .find(|packet| packet.pts < Duration::from_secs(1))
        .expect("epoch 0 packet");
    let second_epoch_packet = packets
        .iter()
        .find(|packet| packet.pts >= Duration::from_secs(1))
        .expect("epoch 1 packet");
    assert_close(
        first.media_origin(),
        expected_origin_for_first_packet(first_epoch_packet, FIRST_PTS),
        "epoch 0 raw origin",
    );
    assert_close(
        second.media_origin(),
        expected_origin_for_first_packet(second_epoch_packet, SECOND_PTS),
        "epoch 1 raw origin",
    );
    // Вторая эпоха начинается на шкале плеера после первой секунды.
    assert_eq!(first.timeline_start(), Duration::ZERO);
    assert_eq!(second.timeline_start(), Duration::from_secs(1));

    let (epoch_one, epoch_two): (Vec<_>, Vec<_>) = packets
        .into_iter()
        .partition(|packet| packet.pts < Duration::from_secs(1));
    assert_packets_match_origin(&epoch_one, FIRST_PTS, first);
    assert_packets_match_origin(&epoch_two, SECOND_PTS, second);
    let known = origins.known_epoch_count();

    // Seek обратно в эпоху 2: пара не меняется, пакеты после seek считаются по той же паре.
    demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(1_500),
        ))
        .expect("seek into epoch 2");
    assert_eq!(
        origins.epoch_origin(1),
        Some(second),
        "seek must not move the origin"
    );
    let after_seek = read_packets_until_eos(&mut *demuxer);
    assert!(!after_seek.is_empty());
    assert_packets_match_origin(&after_seek, SECOND_PTS, second);
    assert_eq!(origins.epoch_origin(1), Some(second));
    assert_eq!(origins.epoch_origin(0), Some(first));
    assert_eq!(origins.known_epoch_count(), known);
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

#[test]
fn live_publishes_the_origin_used_for_each_segment() {
    const FIRST_PTS: u64 = 900_000; // 10 s
    const SECOND_PTS: u64 = 1_800_000; // 20 s
    let first_segment = muxed_ts(FIRST_PTS);
    let second_segment = muxed_ts(SECOND_PTS);
    let server = TestServer::start(move |_, request| {
        let line = &request.request_line;
        if line.contains("/live.m3u8") {
            response(
                "200 OK",
                &[],
                b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
                  #EXTINF:1,\na.ts\n#EXTINF:1,\nb.ts\n",
            )
        } else if line.contains("/a.ts") {
            response("200 OK", &[], &first_segment)
        } else if line.contains("/b.ts") {
            response("200 OK", &[], &second_segment)
        } else {
            response("404 Not Found", &[], b"")
        }
    });
    let generation = SourceGeneration::new(1);
    let target = server.target("/live.m3u8");
    let request = HlsLiveOpenRequest {
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
            selection: muxed_selection(),
            overrides: HlsRequestOverrides::new(None),
            containers: HlsComponentContainerIntent {
                main: HlsContainerEvidence::Exact(HlsRequiredContainer::TransportStream),
                alternate_audio: None,
            },
            demux_registry: demux_registry(),
            policy: open_policy(),
        },
        endpoint_refresh: Arc::new(NoRefreshPort),
        timeline_port_generation: DynamicMediaTimelinePortGeneration::new(
            NonZeroU64::new(1).expect("non-zero test port generation"),
        ),
        initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
    };
    let opened = prepare_hls_live(request).expect("prepare live TS");
    let origins: HlsTimestampOrigins = opened.timestamp_origins();
    let (mut demuxer, _timeline_port, _) = opened.into_parts();

    // Читаем первые пакеты live (без привязки к границе сегмента).
    let deadline = Instant::now() + TEST_TIMEOUT;
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
    assert!(!packets.is_empty(), "live produced no packets");

    // Каждый пакет обязан совпасть с формулой хотя бы для одного из двух известных сегментов,
    // причём пара берётся из опубликованных данных по сырому времени сегмента.
    let candidates = [FIRST_PTS, SECOND_PTS];
    let matches_a_published_segment = |packet: &Packet| {
        candidates.iter().any(|ticks| {
            let raw = from_ticks(*ticks);
            let Some(origin) = origins.origin_for_media_time(0, raw) else {
                return false;
            };
            // Origin относится именно к сегменту этих сырых меток.
            origin.media_origin() <= raw
                && raw.saturating_sub(origin.media_origin()) <= Duration::from_millis(50)
                && packet.pts.abs_diff(player_time(raw, origin)) <= TOLERANCE
        })
    };
    for packet in &packets {
        assert!(
            matches_a_published_segment(packet),
            "live packet {:?} does not follow any published origin",
            packet.pts
        );
    }
    let published = origins.epoch_origin(0).expect("live origin published");
    assert!(
        published.media_origin() >= from_ticks(FIRST_PTS - VIDEO_DTS_LEAD_TICKS) - TOLERANCE,
        "live origin is a real container time, not zero"
    );
}
