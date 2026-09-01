//! N10 vertical: direct dynamic MPD -> S35 live/DVR -> production consumers.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use media_core::{DemuxReadEvent, DemuxSeekRequest};
use service_ytdlp::YtDlpExtractorAdapter;
use source_core::CancellationToken;

use super::super::*;
use super::native_dash_vertical::native_settings;
use super::native_hls_vertical::{
    ControlledHlsServer, assert_decoder_render_audio, decode_fixture,
};
use crate::media_open::{NativeDashSourceState, NativeDashUrl, SafeMediaLabel};
use crate::startup_media::native_dash::{
    NativeDashAttempt, NativeDashFallbackReason, NativeDashPreparationRequest,
    PreparedNativeDashLifecycle, PreparedNativeDashMedia, prepare_native_dash_attempt,
};
use crate::web_media_open::content_probe_tests::direct_progressive::ZeroProcessSpy;
use crate::web_media_open::content_probe_tests::direct_progressive_webm::OffscreenWgpuHarness;

/// Live refresh/seek/recovery не должны зависеть от бесконечного polling-а.
const LIVE_VERTICAL_DEADLINE: Duration = Duration::from_secs(15);

/// Строит direct-UTC dynamic revision с одним реальным muxed H.264/AAC lane.
fn dynamic_manifest(
    direct_utc_seconds: u8,
    publish_time_seconds: u8,
    first_segment_time: u64,
    segment_repeat: u8,
    representation_id: &str,
) -> Vec<u8> {
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic"
          profiles="urn:mpeg:dash:profile:isoff-live:2011"
          availabilityStartTime="1970-01-01T00:00:00Z"
          publishTime="1970-01-01T00:00:{publish_time_seconds:02}Z"
          minimumUpdatePeriod="PT0.05S" timeShiftBufferDepth="PT5S"
          suggestedPresentationDelay="PT1S">
          <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014"
            value="1970-01-01T00:00:{direct_utc_seconds:02}Z"/>
          <Period id="stable-period" start="PT0S">
            <AdaptationSet id="muxed" contentType="application"
              mimeType="application/mp4" codecs="avc1.42c00a,mp4a.40.2"
              width="16" height="16">
              <Representation id="{representation_id}" bandwidth="200000"
                width="16" height="16">
                <SegmentTemplate timescale="1000" initialization="fmp4-init.mp4"
                  media="fmp4-$Time$.m4s">
                  <SegmentTimeline>
                    <S t="{first_segment_time}" d="1000" r="{segment_repeat}"/>
                  </SegmentTimeline>
                </SegmentTemplate>
              </Representation>
            </AdaptationSet>
          </Period>
        </MPD>"#,
    )
    .into_bytes()
}

/// Equal publish snapshots предшествуют newer sliding revision для ordering проверки.
fn live_routes() -> HashMap<String, Vec<Vec<u8>>> {
    let manifest_responses = (0..60_u8)
        .map(|refresh_index| dynamic_manifest(5 + refresh_index / 20, 5, 0, 4, "initial-id"))
        .chain(std::iter::once(dynamic_manifest(
            8,
            8,
            2_000,
            6,
            "rotated-id",
        )))
        .collect::<Vec<_>>();
    let initialization = decode_fixture(include_str!("fixtures/fmp4-init.mp4.base64"));
    let first_fragment = decode_fixture(include_str!("fixtures/fmp4-0.m4s.base64"));
    let mut routes = HashMap::from([
        ("/manifest.mpd".to_owned(), manifest_responses),
        ("/fmp4-init.mp4".to_owned(), vec![initialization]),
    ]);
    for segment_time in [
        0_u64, 1_000, 2_000, 3_000, 4_000, 5_000, 6_000, 7_000, 8_000,
    ] {
        routes.insert(
            format!("/fmp4-{segment_time}.m4s"),
            vec![rebase_fmp4_decode_times(
                &first_fragment,
                segment_time / 1_000,
            )],
        );
    }
    routes
}

/// Сдвигает exact `tfdt` обоих tracks, чтобы hermetic fragments соответствовали `$Time$`.
fn rebase_fmp4_decode_times(fragment: &[u8], segment_index: u64) -> Vec<u8> {
    let mut rebased = fragment.to_vec();
    let decode_times = [
        segment_index
            .checked_mul(10_240)
            .expect("video decode time fixture overflow"),
        segment_index
            .checked_mul(49_152)
            .expect("audio decode time fixture overflow"),
    ];
    let tfdt_offsets = fragment
        .windows(4)
        .enumerate()
        .filter_map(|(offset, window)| (window == b"tfdt").then_some(offset))
        .collect::<Vec<_>>();
    assert_eq!(tfdt_offsets.len(), decode_times.len());
    for (tfdt_offset, decode_time) in tfdt_offsets.into_iter().zip(decode_times) {
        let value_start = tfdt_offset + 8;
        let value_end = value_start + 8;
        rebased[value_start..value_end].copy_from_slice(&decode_time.to_be_bytes());
    }
    rebased
}

/// Открывает direct live attempt с optional installed semantic selection.
fn prepare_native_live(
    source: &NativeDashUrl,
    expected_selection: Option<&web_media_core::WebMediaSemanticSelectionRequest>,
    settings: &WebMediaOpenSettings,
) -> PreparedNativeDashMedia {
    match prepare_native_dash_attempt(NativeDashPreparationRequest {
        source,
        expected_selection,
        network_config: &settings.network_config,
        web_media_config: &settings.web_media_config,
        demux_config: &settings.demux_config,
        system_capabilities: &settings.system_capabilities,
        audio_capabilities: settings.audio_capabilities,
        cancellation: CancellationToken::new(),
    })
    .expect("native dynamic DASH admission должен пройти")
    {
        NativeDashAttempt::Prepared(prepared) => prepared,
        NativeDashAttempt::RequiresYtDlpFallback(reason) => {
            panic!("supported dynamic MPD не имеет права требовать extractor: {reason:?}")
        }
    }
}

/// Дожидается strictly-newer publish и packet-proven shifted DVR window.
fn wait_for_shifted_window(
    prepared: &mut PreparedNativeDashMedia,
    initial_start: media_core::MediaTime,
    server: &ControlledHlsServer,
) -> media_core::TimelineRange {
    let deadline = Instant::now() + LIVE_VERTICAL_DEADLINE;
    loop {
        let PreparedNativeDashLifecycle::Live { timeline_port } = &prepared.lifecycle else {
            panic!("supported dynamic MPD обязан создать live timeline port");
        };
        if let Some(range) = timeline_port.observe().snapshot.state.availability_range()
            && range.start > initial_start
            && server.request_count("/manifest.mpd") >= 61
        {
            return range;
        }
        match prepared.demuxer.next_event() {
            Ok(DemuxReadEvent::TemporarilyUnavailable(_))
            | Ok(DemuxReadEvent::Packet(_))
            | Ok(DemuxReadEvent::TracksChanged(_))
            | Ok(DemuxReadEvent::MediaMetadataChanged(_)) => {}
            Ok(DemuxReadEvent::EndOfStream) => {
                panic!("active DASH live завершился до window shift")
            }
            Err(error) => panic!("DASH live failed before window shift: {error}"),
        }
        assert!(Instant::now() < deadline, "DASH live window shift timeout");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Worker-receipted retained target обязан пройти до проверки expired target-а.
fn assert_retained_dvr_seek(prepared: &mut PreparedNativeDashMedia) {
    let deadline = Instant::now() + LIVE_VERTICAL_DEADLINE;
    let retained_target = loop {
        let PreparedNativeDashLifecycle::Live { timeline_port } = &prepared.lifecycle else {
            panic!("retained DVR seek требует live timeline");
        };
        if let Some(range) = timeline_port.observe().snapshot.state.seekable_range() {
            break range.start.as_duration();
        }
        match prepared.demuxer.next_event() {
            Ok(DemuxReadEvent::TemporarilyUnavailable(_))
            | Ok(DemuxReadEvent::Packet(_))
            | Ok(DemuxReadEvent::TracksChanged(_))
            | Ok(DemuxReadEvent::MediaMetadataChanged(_)) => {}
            Ok(DemuxReadEvent::EndOfStream) => panic!("DASH live завершился до seek evidence"),
            Err(error) => panic!("DASH live seek evidence failed: {error}"),
        }
        assert!(Instant::now() < deadline, "DASH seek evidence timeout");
        thread::sleep(Duration::from_millis(10));
    };
    let request_id = player_core::PreparedDemuxSeekRequestId::new(10);
    prepared
        .seek_port
        .enqueue_seek(request_id, DemuxSeekRequest::accurate(retained_target))
        .expect("retained DASH DVR seek должен войти в worker");
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
        assert!(Instant::now() < deadline, "DASH retained seek timeout");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Извлекает semantic reopen request из stable native source state.
fn semantic_reopen_request(
    source_state: &NativeDashSourceState,
) -> web_media_core::WebMediaSemanticSelectionRequest {
    source_state.neutral_selection().semantic_rematch_request()
}

/// Доказывает direct root reuse, publish ordering, DVR lifecycle и process spy 0.
#[test]
fn native_dynamic_dash_reaches_moving_presentation_audio_and_dvr_without_extractor() {
    let server = ControlledHlsServer::start_with_initial_failures(
        live_routes(),
        HashMap::from([("/fmp4-6000.m4s".to_owned(), 1)]),
    );
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let _extractor_adapter = YtDlpExtractorAdapter::with_process_launcher(process_spy.clone());
    let settings = native_settings();
    let source = NativeDashUrl::new(
        server.target("/manifest.mpd"),
        SafeMediaLabel::from_service_safe_label("controlled native DASH live"),
    );
    let stable_source_identity = source.source_identity();
    let mut prepared = prepare_native_live(&source, None, &settings);
    assert_eq!(server.request_count("/manifest.mpd"), 1);
    assert_eq!(prepared.demuxer.duration(), None);
    let initial_range = match &prepared.lifecycle {
        PreparedNativeDashLifecycle::Live { timeline_port } => timeline_port
            .observe()
            .snapshot
            .state
            .availability_range()
            .expect("initial dynamic MPD должен публиковать availability"),
        PreparedNativeDashLifecycle::Vod { .. } => {
            panic!("dynamic MPD ошибочно открыт как VOD")
        }
    };

    let mut wgpu_harness = OffscreenWgpuHarness::new();
    assert_decoder_render_audio(prepared.demuxer.as_mut(), &mut wgpu_harness);
    assert_retained_dvr_seek(&mut prepared);
    let shifted_range = wait_for_shifted_window(&mut prepared, initial_range.start, &server);
    assert!(shifted_range.start > initial_range.start);
    assert!(
        prepared.demuxer.seek(Duration::ZERO).is_err(),
        "expired DASH DVR target нельзя clamp-ить после window shift"
    );

    let semantic_selection = semantic_reopen_request(&prepared.source_state);
    let source_intent = WebMediaSourceIntent::native_dash(
        source.clone(),
        web_media_core::WebMediaPresentationKind::Live,
        prepared.source_state,
    );
    assert_eq!(
        source_intent.presentation(),
        web_media_core::WebMediaPresentationKind::Live
    );
    let mut reopened = prepare_native_live(&source, Some(&semantic_selection), &settings);
    assert!(matches!(
        reopened.lifecycle,
        PreparedNativeDashLifecycle::Live { .. }
    ));
    assert_eq!(
        reopened
            .source_state
            .neutral_selection()
            .parent()
            .exact()
            .source(),
        stable_source_identity,
        "semantic rematch обязан сохранить stable source lineage"
    );
    assert_decoder_render_audio(reopened.demuxer.as_mut(), &mut wgpu_harness);
    assert!(
        server.request_count("/manifest.mpd") >= 6,
        "periodic/endpoint/reopen refresh-и обязаны обращаться к stable root"
    );
    assert!(
        server.request_count("/fmp4-6000.m4s") >= 2,
        "первый expired fragment должен повториться только после stable-root endpoint recovery"
    );
    assert_eq!(
        process_spy.invocation_count(),
        0,
        "supported DASH live open/refresh/DVR/reopen не запускают extractor"
    );
}

/// Unsupported dynamic addressing остаётся deliberate profile exclusion до extractor gate-а.
#[test]
fn native_dynamic_dash_keeps_profile_network_malformed_and_cancel_failures_distinct() {
    let unsupported_manifest = br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic"
      profiles="urn:mpeg:dash:profile:isoff-live:2011"
      availabilityStartTime="1970-01-01T00:00:00Z"
      publishTime="1970-01-01T00:00:05Z" minimumUpdatePeriod="PT1S"
      suggestedPresentationDelay="PT1S">
      <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014"
        value="1970-01-01T00:00:05Z"/>
      <Period id="p0" start="PT0S">
        <AdaptationSet contentType="video" mimeType="video/mp4" codecs="avc1.42c00a">
          <Representation id="video" bandwidth="1000" width="16" height="16">
            <SegmentTemplate timescale="1" duration="1" initialization="init.mp4"
              media="$Number$.m4s"/>
          </Representation>
        </AdaptationSet>
      </Period>
    </MPD>"#
        .to_vec();
    let server = ControlledHlsServer::start(HashMap::from([
        ("/unsupported.mpd".to_owned(), vec![unsupported_manifest]),
        (
            "/malformed.mpd".to_owned(),
            vec![
                br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic"
              profiles="urn:mpeg:dash:profile:isoff-live:2011"
              availabilityStartTime="1970-01-01T00:00:00Z"
              publishTime="1970-01-01T00:00:05Z" minimumUpdatePeriod="PT1S"
              suggestedPresentationDelay="PT1S">
              <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014"
                value="1970-01-01T00:00:05Z"/>
              <Period id="p0" start="PT0S">"#
                    .to_vec(),
            ],
        ),
    ]));
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let _extractor_adapter = YtDlpExtractorAdapter::with_process_launcher(process_spy.clone());
    let settings = native_settings();
    let source = |path: &str| {
        NativeDashUrl::new(
            server.target(path),
            SafeMediaLabel::from_service_safe_label("controlled DASH failure"),
        )
    };
    let prepare = |source: &NativeDashUrl, cancellation: CancellationToken| {
        prepare_native_dash_attempt(NativeDashPreparationRequest {
            source,
            expected_selection: None,
            network_config: &settings.network_config,
            web_media_config: &settings.web_media_config,
            demux_config: &settings.demux_config,
            system_capabilities: &settings.system_capabilities,
            audio_capabilities: settings.audio_capabilities,
            cancellation,
        })
    };

    assert!(matches!(
        prepare(&source("/unsupported.mpd"), CancellationToken::new())
            .expect("unsupported addressing должен остаться typed profile result"),
        NativeDashAttempt::RequiresYtDlpFallback(
            NativeDashFallbackReason::UnsupportedNativeProfile
        )
    ));
    assert!(
        prepare(&source("/malformed.mpd"), CancellationToken::new()).is_err(),
        "malformed dynamic MPD не имеет права fallback-иться"
    );
    assert!(
        prepare(&source("/missing.mpd"), CancellationToken::new()).is_err(),
        "network/status failure не имеет права fallback-иться"
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        prepare(&source("/cancelled.mpd"), cancelled).is_err(),
        "cancelled attempt не имеет права fallback-иться"
    );
    assert_eq!(server.request_count("/cancelled.mpd"), 0);
    assert_eq!(
        process_spy.invocation_count(),
        0,
        "typed native admission outcomes сами не запускают extractor process"
    );
}
