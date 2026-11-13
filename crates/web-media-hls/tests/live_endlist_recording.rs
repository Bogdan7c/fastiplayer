//! Эфир HLS кончается (мост 16d1, решение 325): обновлённый плейлист получил
//! `EXT-X-ENDLIST`, и табло шкалы становится записью с окончательным краем, а перематывать
//! можно только по доказанному пакетами диапазону.
//!
//! Через настоящий runtime (admission top media playlist, как в production): сервер отдаёт
//! эфир, на обновлении — тот же плейлист с концом; demuxer дочитывает до конца потока, а
//! табло, которое увидит плеер, — запись. Звуковой эфир: у AAC-пакетов есть длительность,
//! поэтому пакеты доказывают диапазон перемотки (у одиночных кадров видео без длительности
//! доказанного диапазона нет).

// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

#[allow(dead_code)] // Shared fixture экспортирует больше builders, чем нужно этому binary.
mod support;

use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use hls_playlist_core::HlsParserLimits;
use media_core::{
    DemuxReadEvent, DynamicMediaTimelineEpoch, DynamicMediaTimelinePortGeneration,
    DynamicMediaTimelineViewKind,
};
use source_core::{CancellationToken, HttpRequestTarget};
use support::{
    TestQueries, TestServer, adaptive_context, demux_registry, long_audio_ts_segment, open_policy,
    response,
};
use web_media_core::{CodecFamily, PreferredHeightPolicy};
use web_media_hls::{
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsLiveOpenRequest, HlsManifestInput, HlsRequestOverrides,
    HlsVodOpenRequest, NativeHlsCatalogAdmission, NativeHlsSelectionPolicy,
    admit_native_hls_catalog, prepare_hls_live,
};
use web_media_transport_api::SourceGeneration;

const TEST_TIMEOUT: Duration = Duration::from_secs(6);
const PTS_PER_SECOND: u64 = 90_000;
/// Три сегмента звука по 2 с: край эфира на 6 с.
const SEGMENT_NAMES: [&str; 3] = ["a", "b", "c"];
const SEGMENT_SECONDS: u64 = 2;
const FINAL_EDGE: Duration = Duration::from_secs(6);

/// Endpoint здесь не протухает: любой вызов refresh — ошибка теста.
struct UnexpectedRefreshPort;

impl HlsEndpointRefreshPort for UnexpectedRefreshPort {
    fn refresh(
        &self,
        _request: HlsEndpointRefreshRequest,
    ) -> Result<HlsEndpointRefreshReply, HlsEndpointRefreshError> {
        Err(HlsEndpointRefreshError::AttemptsExhausted)
    }
}

fn playlist(ended: bool) -> String {
    let mut text =
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n".to_owned();
    for name in SEGMENT_NAMES {
        text.push_str(&format!("#EXTINF:{SEGMENT_SECONDS},\n{name}.ts\n"));
    }
    if ended {
        text.push_str("#EXT-X-ENDLIST\n");
    }
    text
}

/// Admission ровно так, как его делает production: top media playlist без master.
fn admit(target: &HttpRequestTarget) -> NativeHlsCatalogAdmission {
    let selection_policy = NativeHlsSelectionPolicy::new(
        PreferredHeightPolicy::NoPreference,
        vec![CodecFamily::H264, CodecFamily::Vp9, CodecFamily::Av1],
    )
    .expect("valid policy");
    admit_native_hls_catalog(
        playlist(false).as_bytes(),
        target,
        HlsParserLimits::default(),
        &selection_policy,
    )
    .expect("audio-only live playlist passes native admission")
}

fn live_request(target: HttpRequestTarget) -> HlsLiveOpenRequest {
    let admission = admit(&target);
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
            selection: admission.runtime_intent(),
            overrides: HlsRequestOverrides::new(None),
            containers: admission.container_intent(),
            demux_registry: demux_registry(),
            policy: open_policy(),
        },
        endpoint_refresh: Arc::new(UnexpectedRefreshPort),
        timeline_port_generation: DynamicMediaTimelinePortGeneration::new(
            NonZeroU64::new(1).expect("non-zero test port generation"),
        ),
        initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
    }
}

/// Сервер: первый запрос плейлиста — эфир, все следующие — тот же плейлист с концом.
fn ending_stream_server() -> TestServer {
    let playlist_requests = Arc::new(AtomicUsize::new(0));
    let segments: Vec<Vec<u8>> = (0..SEGMENT_NAMES.len() as u64)
        .map(|index| {
            let start_pts = (1 + index * SEGMENT_SECONDS) * PTS_PER_SECOND;
            long_audio_ts_segment(start_pts, SEGMENT_SECONDS)
        })
        .collect();
    TestServer::start(move |_, request| {
        if request.request_line.starts_with("GET /live.m3u8 ") {
            let previous_requests = playlist_requests.fetch_add(1, Ordering::SeqCst);
            let ended = previous_requests > 0;
            return response("200 OK", &[], playlist(ended).as_bytes());
        }
        for (name, segment) in SEGMENT_NAMES.iter().zip(&segments) {
            if request
                .request_line
                .starts_with(&format!("GET /{name}.ts "))
            {
                return response("200 OK", &[], segment);
            }
        }
        response("404 Not Found", &[], b"")
    })
}

#[test]
fn a_live_stream_that_got_endlist_is_read_to_its_end_and_becomes_a_recording() {
    let server = ending_stream_server();
    let opened =
        prepare_hls_live(live_request(server.target("/live.m3u8"))).expect("prepare live stream");
    let (mut demuxer, timeline_port, _) = opened.into_parts();
    assert_eq!(
        timeline_port.observe().snapshot.state.kind(),
        DynamicMediaTimelineViewKind::Live,
        "the stream opens on air"
    );

    // Дочитываем до конца: публикация записи на каждом пакете не должна уронить чтение.
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut packet_count = 0;
    loop {
        assert!(
            Instant::now() < deadline,
            "the ended stream was not drained"
        );
        match demuxer
            .next_event()
            .expect("the ended stream stays readable")
        {
            DemuxReadEvent::Packet(_) => packet_count += 1,
            DemuxReadEvent::EndOfStream => break,
            _ => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    assert!(packet_count > 0, "packets were read before the end");

    let state = timeline_port.observe().snapshot.state;
    let recording = state
        .as_recording()
        .unwrap_or_else(|| panic!("an ended stream is a recording, got {state:?}"));
    assert_eq!(recording.duration().as_duration(), FINAL_EDGE);
    // Перемотка — только по доказанному пакетами: последний кадр звука каждого сегмента
    // фикстуры кончается раньше конца сегмента, поэтому диапазон короче длины записи
    // (решение 325: длина окончательная, перемотка — докуда доказано).
    let seek_range = recording.available_range();
    assert!(seek_range.start < seek_range.end, "{seek_range:?}");
    assert!(
        seek_range.end.as_duration() > FINAL_EDGE - Duration::from_secs(SEGMENT_SECONDS)
            && seek_range.end.as_duration() <= FINAL_EDGE,
        "seeking reaches the last segment of the ended stream: {seek_range:?}"
    );
}
