//! F3: live HLS входит в эфир с hold-back (RFC 8216 §6.3.3), а не на самом краю плейлиста.
//! Через настоящий runtime проверяем оба связанных решения: позицию, которую получит
//! плеер (neutral timeline), и сегмент, с которого demux реально читает пакеты.

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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use media_core::{DemuxReadEvent, DynamicMediaTimelineEpoch, DynamicMediaTimelinePortGeneration};
use source_core::{CancellationToken, HttpRequestTarget};
use support::{
    TestQueries, TestServer, adaptive_context, demux_registry, muxed_ts, open_policy, response,
};
use web_media_hls::{
    HlsAudioLayoutIntent, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsEndpointRefreshError, HlsEndpointRefreshPort, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsLiveOpenRequest, HlsMainTrackLayoutIntent, HlsManifestInput,
    HlsRequestOverrides, HlsRequiredContainer, HlsVariantSelectionIntent, HlsVodOpenRequest,
    prepare_hls_live,
};
use web_media_transport_api::SourceGeneration;

const TEST_TIMEOUT: Duration = Duration::from_secs(4);

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

fn live_request(target: HttpRequestTarget) -> HlsLiveOpenRequest {
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
    }
}

/// Шесть сегментов по 1 с (target 1 с): край на 6 с, hold-back 3 с.
fn six_second_playlist() -> String {
    let mut text = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n".to_owned();
    for name in ["a", "b", "c", "d", "e", "f"] {
        text.push_str(&format!("#EXTINF:1,\n{name}.ts\n"));
    }
    text
}

#[test]
fn live_open_joins_three_target_durations_before_the_edge() {
    let requested_paths = Arc::new(Mutex::new(Vec::<String>::new()));
    let handler_paths = Arc::clone(&requested_paths);
    let server = TestServer::start(move |_, request| {
        handler_paths
            .lock()
            .expect("request log")
            .push(request.request_line.clone());
        if request.request_line.starts_with("GET /live.m3u8 ") {
            return response("200 OK", &[], six_second_playlist().as_bytes());
        }
        if request.request_line.contains(".ts ") {
            return response("200 OK", &[], &muxed_ts(90_000));
        }
        response("404 Not Found", &[], b"")
    });

    let opened = prepare_hls_live(live_request(server.target("/live.m3u8")))
        .expect("prepare live with hold-back");
    let (mut demuxer, timeline_port, _) = opened.into_parts();

    // 1. Плеер получает позицию входа на 3 target duration раньше края.
    let state = timeline_port.observe().snapshot.state;
    assert_eq!(state.live_edge().as_duration(), Duration::from_secs(6));
    assert_eq!(state.join_position().as_duration(), Duration::from_secs(3));

    // 2. Demux начинает именно с сегмента на этой позиции (d.ts), а не с края или с начала.
    let deadline = Instant::now() + TEST_TIMEOUT;
    let first_packet = loop {
        assert!(Instant::now() < deadline, "no packet before timeout");
        match demuxer.next_event().expect("live stays readable") {
            DemuxReadEvent::Packet(packet) => break packet,
            _ => std::thread::sleep(Duration::from_millis(2)),
        }
    };
    let first_pts = first_packet.pts;
    assert!(
        first_pts >= Duration::from_secs(3) && first_pts < Duration::from_secs(4),
        "the first packet {first_pts:?} must be in the segment d.ts [3 s, 4 s]"
    );
}
