//! N08 vertical: native sliding HLS live/DVR до decoder/render/audio без extractor process-а.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use media_core::DemuxReadEvent;
use web_media_hls::HlsVodStartIntent;

use super::super::*;
use super::native_hls_lifecycle_n14b::PersistentHlsConsumer;
use super::native_hls_vertical::{
    ControlledHlsServer, alternate_component_selection, assert_decoder_render_audio,
    assert_decoder_render_audio_movement, decode_fixture, native_request_parts, native_settings,
    prepare_native,
};
use crate::media_open::{NativeHlsUrl, SafeMediaLabel};
use crate::startup_media::native_hls::PreparedNativeHlsLifecycle;
use crate::web_media_open::content_probe_tests::direct_progressive::ZeroProcessSpy;
use crate::web_media_open::content_probe_tests::direct_progressive_webm::OffscreenWgpuHarness;

const LIVE_VERTICAL_DEADLINE: Duration = Duration::from_secs(10);

/// Один и тот же stable master ведёт к sliding media window без временных child identity в app.
fn live_master() -> Vec<u8> {
    concat!(
        "#EXTM3U\n",
        "#EXT-X-STREAM-INF:BANDWIDTH=200000,RESOLUTION=16x16,",
        "CODECS=\"avc1.42c00a,mp4a.40.2\"\n",
        "live.m3u8\n",
        "#EXT-X-STREAM-INF:BANDWIDTH=100000,RESOLUTION=16x16,",
        "CODECS=\"avc1.42c00a,mp4a.40.2\"\n",
        "fmp4-live.m3u8\n",
    )
    .as_bytes()
    .to_vec()
}

/// Initial snapshots удерживают sequence 0, затем refresh сдвигает availability к sequence 1.
fn sliding_live_manifests() -> Vec<Vec<u8>> {
    let initial = concat!(
        "#EXTM3U\n",
        "#EXT-X-TARGETDURATION:1\n",
        "#EXT-X-MEDIA-SEQUENCE:0\n",
        "#EXTINF:1,\n",
        "ts-0.ts\n",
        "#EXTINF:1,\n",
        "ts-1.ts\n",
    )
    .as_bytes()
    .to_vec();
    let shifted = concat!(
        "#EXTM3U\n",
        "#EXT-X-TARGETDURATION:1\n",
        "#EXT-X-MEDIA-SEQUENCE:1\n",
        "#EXTINF:1,\n",
        "ts-1.ts\n",
        "#EXTINF:1,\n",
        "ts-2.ts\n",
    )
    .as_bytes()
    .to_vec();
    vec![initial.clone(), initial.clone(), initial, shifted]
}

/// Второй supported row даёт semantic same-item switch на fMP4 без смены live lifecycle.
fn fmp4_live_manifest() -> Vec<u8> {
    concat!(
        "#EXTM3U\n",
        "#EXT-X-VERSION:7\n",
        "#EXT-X-TARGETDURATION:1\n",
        "#EXT-X-MEDIA-SEQUENCE:0\n",
        "#EXT-X-MAP:URI=\"fmp4-init.mp4\"\n",
        "#EXTINF:1,\n",
        "fmp4-0.m4s\n",
        "#EXTINF:1,\n",
        "fmp4-1.m4s\n",
    )
    .as_bytes()
    .to_vec()
}

/// Route snapshots используют реальные N07 H.264/AAC TS payloads, но live manifest обновляется.
fn live_routes() -> HashMap<String, Vec<Vec<u8>>> {
    let first_segment = decode_fixture(include_str!("fixtures/ts-0.ts.base64"));
    let second_segment = decode_fixture(include_str!("fixtures/ts-1.ts.base64"));
    HashMap::from([
        ("/master.m3u8".to_owned(), vec![live_master()]),
        ("/live.m3u8".to_owned(), sliding_live_manifests()),
        ("/fmp4-live.m3u8".to_owned(), vec![fmp4_live_manifest()]),
        ("/ts-0.ts".to_owned(), vec![first_segment]),
        ("/ts-1.ts".to_owned(), vec![second_segment.clone()]),
        ("/ts-2.ts".to_owned(), vec![second_segment]),
        (
            "/fmp4-init.mp4".to_owned(),
            vec![decode_fixture(include_str!(
                "fixtures/fmp4-init.mp4.base64"
            ))],
        ),
        (
            "/fmp4-0.m4s".to_owned(),
            vec![decode_fixture(include_str!("fixtures/fmp4-0.m4s.base64"))],
        ),
        (
            "/fmp4-1.m4s".to_owned(),
            vec![decode_fixture(include_str!("fixtures/fmp4-1.m4s.base64"))],
        ),
    ])
}

/// Дожидается refresh publication, продолжая обслуживать production demux consumer boundary.
fn wait_for_shifted_window(
    prepared: &mut crate::startup_media::native_hls::PreparedNativeHlsMedia,
    initial_start: media_core::MediaTime,
    server: &ControlledHlsServer,
) -> media_core::TimelineRange {
    let deadline = Instant::now() + LIVE_VERTICAL_DEADLINE;
    loop {
        let PreparedNativeHlsLifecycle::Live { timeline_port } = &prepared.lifecycle else {
            panic!("supported sliding manifest обязан создать live timeline port");
        };
        if let Some(range) = timeline_port.observe().snapshot.state.availability_range()
            && range.start > initial_start
            && server.request_count("/live.m3u8") >= 4
        {
            return range;
        }
        match prepared.demuxer.next_event() {
            Ok(DemuxReadEvent::TemporarilyUnavailable(_))
            | Ok(DemuxReadEvent::Packet(_))
            | Ok(DemuxReadEvent::TracksChanged(_))
            | Ok(DemuxReadEvent::MediaMetadataChanged(_)) => {}
            Ok(DemuxReadEvent::EndOfStream) => panic!("active live завершился до window shift"),
            Err(error) => panic!("live demux failed before window shift: {error}"),
        }
        assert!(Instant::now() < deadline, "live window shift timeout");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Первый ts-2 получает 410; native refresh обязан повторно fetch-нуть stable root и продолжить.
fn wait_for_native_endpoint_recovery(
    prepared: &mut crate::startup_media::native_hls::PreparedNativeHlsMedia,
    server: &ControlledHlsServer,
) {
    let deadline = Instant::now() + LIVE_VERTICAL_DEADLINE;
    loop {
        match prepared.demuxer.next_event() {
            Ok(DemuxReadEvent::Packet(_)) if server.request_count("/master.m3u8") >= 2 => return,
            Ok(DemuxReadEvent::TemporarilyUnavailable(_))
            | Ok(DemuxReadEvent::Packet(_))
            | Ok(DemuxReadEvent::TracksChanged(_))
            | Ok(DemuxReadEvent::MediaMetadataChanged(_)) => {}
            Ok(DemuxReadEvent::EndOfStream) => {
                panic!("native endpoint recovery завершил active live")
            }
            Err(error) => panic!("native endpoint recovery failed: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "native endpoint recovery timeout"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// Worker-receipted seek внутри retained DVR обязан succeed до проверки expired old target-а.
fn assert_retained_dvr_seek(prepared: &crate::startup_media::native_hls::PreparedNativeHlsMedia) {
    let PreparedNativeHlsLifecycle::Live { timeline_port } = &prepared.lifecycle else {
        panic!("retained DVR seek требует live timeline");
    };
    let retained_target = timeline_port
        .observe()
        .snapshot
        .state
        .seekable_range()
        .expect("consumer packets должны доказать seekable DVR range")
        .start
        .as_duration();
    let request_id = player_core::PreparedDemuxSeekRequestId::new(1);
    prepared
        .seek_port
        .enqueue_seek(
            request_id,
            media_core::DemuxSeekRequest::accurate(retained_target),
        )
        .expect("retained DVR seek должен войти в worker");
    let deadline = Instant::now() + LIVE_VERTICAL_DEADLINE;
    loop {
        if let Some(receipt) = prepared.seek_port.poll_seek_receipt() {
            assert_eq!(receipt.request_id, request_id);
            assert!(matches!(
                receipt.outcome,
                player_core::PreparedDemuxSeekOutcome::Succeeded(_)
            ));
            return;
        }
        assert!(Instant::now() < deadline, "retained DVR receipt timeout");
        thread::sleep(Duration::from_millis(10));
    }
}

/// N14A: sliding HLS live initial window даёт moving render/readback и PCM/clock без switch/restart.
#[cfg(feature = "ffmpeg")]
#[test]
fn n14a_consumer_hls_sliding_live_reaches_consumers_with_exact_accounting() {
    let server = ControlledHlsServer::start(live_routes());
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let mut settings = native_settings();
    process_spy.install_as_attempt_owner(&mut settings);
    let source = NativeHlsUrl::new(
        server.target("/master.m3u8"),
        SafeMediaLabel::from_service_safe_label("N14A native HLS sliding live"),
    );
    assert_eq!(server.request_count("/master.m3u8"), 0);
    assert_eq!(server.response_body_bytes("/master.m3u8"), 0);

    let mut prepared = prepare_native(&source, None, &settings, HlsVodStartIntent::Beginning);
    assert!(matches!(
        prepared.lifecycle,
        PreparedNativeHlsLifecycle::Live { .. }
    ));
    let mut wgpu_harness = OffscreenWgpuHarness::new();
    assert_decoder_render_audio_movement(prepared.demuxer.as_mut(), &mut wgpu_harness);

    assert_eq!(server.request_count("/master.m3u8"), 1);
    assert_eq!(
        server.response_body_bytes("/master.m3u8"),
        live_master().len()
    );
    assert_eq!(process_spy.invocation_count(), 0);
}

/// Проверяет moving consumers, packet-proven window shift, expired target и process spy 0.
#[cfg(feature = "ffmpeg")]
#[test]
fn n14b_lifecycle_hls_live_dvr_expiry_recovery_switch_has_no_false_eof() {
    let server = ControlledHlsServer::start_with_initial_failures(
        live_routes(),
        HashMap::from([("/ts-2.ts".to_owned(), 1)]),
    );
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let mut settings = native_settings();
    process_spy.install_as_attempt_owner(&mut settings);
    let source = NativeHlsUrl::new(
        server.target("/master.m3u8"),
        SafeMediaLabel::from_service_safe_label("controlled native HLS live"),
    );
    assert_eq!(
        server.request_count("/master.m3u8"),
        0,
        "syntactic live-HLS classifier не должен fetch-ить root до open"
    );
    let mut prepared = prepare_native(&source, None, &settings, HlsVodStartIntent::Beginning);
    assert_eq!(server.request_count("/master.m3u8"), 1);
    assert_eq!(
        prepared.duration(),
        None,
        "live duration обязана остаться unknown"
    );

    let mut wgpu_harness = OffscreenWgpuHarness::new();
    let mut persistent_consumer =
        PersistentHlsConsumer::new(prepared.demuxer.as_ref(), &wgpu_harness);
    persistent_consumer.consume(prepared.demuxer.as_mut(), &mut wgpu_harness);
    persistent_consumer.consume(prepared.demuxer.as_mut(), &mut wgpu_harness);
    let initial_range = match &prepared.lifecycle {
        PreparedNativeHlsLifecycle::Live { timeline_port } => timeline_port
            .observe()
            .snapshot
            .state
            .availability_range()
            .expect("consumer packets должны доказать initial availability"),
        PreparedNativeHlsLifecycle::Vod { .. } => {
            panic!("sliding manifest ошибочно открыт как VOD")
        }
    };
    assert_retained_dvr_seek(&prepared);
    persistent_consumer.flush_for_seek();
    persistent_consumer.consume(prepared.demuxer.as_mut(), &mut wgpu_harness);

    let shifted_range = wait_for_shifted_window(&mut prepared, initial_range.start, &server);
    assert!(shifted_range.start > initial_range.start);
    wait_for_native_endpoint_recovery(&mut prepared, &server);
    assert_eq!(
        server.request_count("/master.m3u8"),
        2,
        "endpoint expiry должен сделать один bounded stable-root refresh"
    );
    assert!(
        prepared.demuxer.seek(Duration::ZERO).is_err(),
        "expired DVR target нельзя clamp-ить либо принимать после window shift"
    );

    let (_, _, alternate_selection) = alternate_component_selection(&prepared.source_state);
    let source_intent = WebMediaSourceIntent::native_hls(
        source.clone(),
        web_media_core::WebMediaPresentationKind::Live,
        prepared.source_state,
    );
    assert_eq!(
        source_intent.presentation(),
        web_media_core::WebMediaPresentationKind::Live
    );
    let WebMediaSelectionSwitchResolution::Ready(switch_request) = source_intent
        .selection_switch_request(
            WebMediaSelectionSwitchIntent::ComponentSemantic(alternate_selection),
            settings.clone(),
        )
    else {
        panic!("native live component switch обязан создать same-item reopen request");
    };
    let (switch_source, switch_selection, switch_settings) = native_request_parts(switch_request);
    let mut switched = prepare_native(
        &switch_source,
        Some(&switch_selection),
        &switch_settings,
        HlsVodStartIntent::Beginning,
    );
    assert!(matches!(
        switched.lifecycle,
        PreparedNativeHlsLifecycle::Live { .. }
    ));
    assert_decoder_render_audio(switched.demuxer.as_mut(), &mut wgpu_harness);
    assert_eq!(
        server.request_count("/master.m3u8"),
        3,
        "initial + endpoint recovery + semantic switch делают по одному root GET"
    );
    assert_eq!(
        process_spy.invocation_count(),
        0,
        "supported native live open/refresh/DVR/switch не запускают extractor"
    );
}
