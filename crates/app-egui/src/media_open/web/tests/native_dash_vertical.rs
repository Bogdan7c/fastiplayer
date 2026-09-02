//! N09 vertical: direct static MPD -> exact fMP4/WebM rows -> production consumers.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use audio::{AudioDecodeCapabilityProvider, ProductionAudioDecoderFactory};
use base64::Engine as _;
use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, CURRENT_CAPABILITY_SCHEMA_VERSION,
    SupportedVideoOutput, SystemCapabilities,
};
use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, H264Profile, SupportedVideoDecodeFormat,
    VideoCodec as DecodeVideoCodec, VideoProfile, Vp9Profile,
};
use media_core::{DemuxSeekRequest, TrackKind};
use player_core::{PreparedDemuxSeekOutcome, PreparedDemuxSeekPort, PreparedDemuxSeekRequestId};
use rustiplayer_config::{AppConfig, VideoCodec};
use source_core::CancellationToken;
use video_frame_contract::VideoFrameContract;

use super::super::*;
use super::native_hls_vertical::{
    ControlledHlsServer, assert_decoder_render_audio_for_codec, decode_fixture,
};
use crate::media_open::{
    NativeDashOpenIntent, NativeDashSourceState, NativeDashUrl, SafeMediaLabel,
};
use crate::startup_media::native_dash::{
    NativeDashAttempt, NativeDashPreparationRequest, PreparedNativeDashMedia,
    prepare_native_dash_attempt,
};
use crate::web_media_open::content_probe_tests::direct_progressive::ZeroProcessSpy;
use crate::web_media_open::content_probe_tests::direct_progressive_webm::{
    MUXED_WEBM_BASE64, OffscreenWgpuHarness,
};
use crate::web_media_stream_model::component_variants::{
    ComponentVariantActionResolution, ComponentVariantSelectionAction,
    WebMediaComponentVariantAxisKind, WebMediaComponentVariantProjection,
    WebMediaInstalledComponentVariantPresentation,
};

/// Decoder, render, audio и seek receipts не должны зависеть от бесконечного polling-а.
const DASH_VERTICAL_DEADLINE: Duration = Duration::from_secs(10);
/// Seek остаётся внутри единственного finite segment-а обеих hermetic rows.
const DASH_SEEK_POSITION: Duration = Duration::from_millis(100);
/// Cluster EBML ID отделяет reusable WebM initialization от media payload-а.
const WEBM_CLUSTER_ID: [u8; 4] = [0x1f, 0x43, 0xb6, 0x75];
/// Segment EBML ID предшествует восьмибайтовому declared size generated fixture-а.
const WEBM_SEGMENT_ID: [u8; 4] = [0x18, 0x53, 0x80, 0x67];
/// Unknown-size VINT позволяет DASH initialization завершиться до первого Cluster.
const WEBM_UNKNOWN_SEGMENT_SIZE: [u8; 8] = [0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
/// Первый Opus SimpleBlock generated fixture-а до компенсации codec delay начинается с -7 ms.
const WEBM_FIRST_OPUS_BLOCK_HEADER: [u8; 6] = [0xa3, 0xa5, 0x82, 0x00, 0x00, 0x80];
/// Seven-millisecond relative timecode компенсирует Opus codec delay у media origin-а.
const WEBM_OPUS_PREROLL_COMPENSATION_MS: u8 = 7;
/// Первый VP9 block следует за compensated Opus anchor-ом и не должен precede media origin.
const WEBM_FIRST_VP9_BLOCK_HEADER: [u8; 7] = [0xa3, 0x41, 0x08, 0x81, 0x00, 0x00, 0x80];
/// Fourteen milliseconds сохраняют ordered nonnegative timestamps для обоих muxed tracks.
const WEBM_VP9_ORIGIN_COMPENSATION_MS: u8 = 14;
/// Root переставляет реальные rows, чтобы exact selection доказывал semantic rematch.
fn static_manifest(webm_first: bool) -> Vec<u8> {
    // Public MPD может содержать subtitle rows рядом с полностью playable A/V catalog.
    // У плеера нет text consumer-а, поэтому такая row не должна блокировать реальные consumers.
    let text_row = r#"
      <AdaptationSet id="captions" contentType="text" lang="en" subsegmentAlignment="true">
        <Representation id="captions-en" bandwidth="256" mimeType="application/mp4"
            codecs="wvtt">
          <BaseURL>captions.mp4</BaseURL>
          <SegmentBase indexRange="0-31">
            <Initialization range="0-15"/>
          </SegmentBase>
        </Representation>
      </AdaptationSet>
    "#;
    let fmp4_row = r#"
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
    "#;
    let webm_row = r#"
      <AdaptationSet id="webm" contentType="application" mimeType="video/webm"
          codecs="vp9,opus" width="16" height="16">
        <Representation id="unsupported-pixel-shape" bandwidth="50000" sar="2:1"
            width="16" height="16"/>
        <Representation id="vp9-opus" bandwidth="100000" width="16" height="16">
          <SegmentList timescale="1" duration="2">
            <Initialization sourceURL="webm-init.webm"/>
            <SegmentURL media="webm-0.webm"/>
          </SegmentList>
        </Representation>
      </AdaptationSet>
    "#;
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
            xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
            xsi:schemaLocation="urn:mpeg:dash:schema:mpd:2011 DASH-MPD.xsd" type="static"
            mediaPresentationDuration="PT2S">
          <Period id="p0" duration="PT2S">
            {}{}{}
          </Period>
        </MPD>"#,
        text_row,
        if webm_first { webm_row } else { fmp4_row },
        if webm_first { fmp4_row } else { webm_row },
    )
    .into_bytes()
}

/// Превращает generated progressive WebM в корректную DASH init/media пару по EBML boundaries.
fn split_webm_fixture() -> (Vec<u8>, Vec<u8>) {
    let mut webm = base64::engine::general_purpose::STANDARD
        .decode(MUXED_WEBM_BASE64)
        .expect("N09 VP9/Opus fixture должен быть валидным base64");
    let segment_offset = webm
        .windows(WEBM_SEGMENT_ID.len())
        .position(|window| window == WEBM_SEGMENT_ID)
        .expect("generated WebM должен содержать Segment");
    let segment_size_offset = segment_offset + WEBM_SEGMENT_ID.len();
    webm[segment_size_offset..segment_size_offset + WEBM_UNKNOWN_SEGMENT_SIZE.len()]
        .copy_from_slice(&WEBM_UNKNOWN_SEGMENT_SIZE);
    let cluster_offset = webm
        .windows(WEBM_CLUSTER_ID.len())
        .position(|window| window == WEBM_CLUSTER_ID)
        .expect("generated WebM должен содержать Cluster");
    let opus_relative_timestamp_offset = webm[cluster_offset..]
        .windows(WEBM_FIRST_OPUS_BLOCK_HEADER.len())
        .position(|window| window == WEBM_FIRST_OPUS_BLOCK_HEADER)
        .map(|relative_offset| cluster_offset + relative_offset + 4)
        .expect("generated WebM должен содержать первый Opus SimpleBlock");
    webm[opus_relative_timestamp_offset] = WEBM_OPUS_PREROLL_COMPENSATION_MS;
    let vp9_relative_timestamp_offset = webm[cluster_offset..]
        .windows(WEBM_FIRST_VP9_BLOCK_HEADER.len())
        .position(|window| window == WEBM_FIRST_VP9_BLOCK_HEADER)
        .map(|relative_offset| cluster_offset + relative_offset + 5)
        .expect("generated WebM должен содержать первый VP9 SimpleBlock");
    webm[vp9_relative_timestamp_offset] = WEBM_VP9_ORIGIN_COMPENSATION_MS;
    (
        webm[..cluster_offset].to_vec(),
        webm[cluster_offset..].to_vec(),
    )
}

/// Делит repository fMP4 по тем же проверенным atom boundaries, что DASH runtime tests.
fn split_fmp4_fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let fmp4 = decode_fixture(include_str!(
        "../../../../../web-media-hls/tests/fixtures/muxed-fmp4.base64"
    ));
    let media_fragment = fmp4[1_248..2_314].to_vec();
    (
        fmp4[..1_248].to_vec(),
        media_fragment.clone(),
        media_fragment,
    )
}

/// Собирает immutable MPD, fMP4 и WebM routes поверх loopback production HTTP transport-а.
pub(super) fn fixture_routes() -> HashMap<String, Vec<Vec<u8>>> {
    let (webm_initialization, webm_media) = split_webm_fixture();
    let (fmp4_initialization, fmp4_first, fmp4_second) = split_fmp4_fixture();
    HashMap::from([
        (
            "/manifest.mpd".to_owned(),
            vec![
                static_manifest(false),
                static_manifest(true),
                static_manifest(false),
            ],
        ),
        ("/fmp4-init.mp4".to_owned(), vec![fmp4_initialization]),
        ("/fmp4-0.m4s".to_owned(), vec![fmp4_first]),
        ("/fmp4-1.m4s".to_owned(), vec![fmp4_second]),
        ("/webm-init.webm".to_owned(), vec![webm_initialization]),
        ("/webm-0.webm".to_owned(), vec![webm_media]),
    ])
}

/// Один production-readable backend snapshot одновременно доказывает H.264 и VP9 rows.
fn dash_system_capabilities() -> SystemCapabilities {
    let backend_id = DecodeBackendId::new("nnine_sw").expect("валидный backend ID");
    let output = |codec, profile| SupportedVideoOutput {
        backend: backend_id.clone(),
        decode_format: SupportedVideoDecodeFormat {
            codec,
            profile,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            max_width: Some(16),
            max_height: Some(16),
            max_fps: Some(30.0),
            hdr_input: false,
        },
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
    };
    let h264 = output(
        DecodeVideoCodec::H264,
        VideoProfile::H264(H264Profile::Baseline),
    );
    let vp9 = output(
        DecodeVideoCodec::Vp9,
        VideoProfile::Vp9(Vp9Profile::Profile0),
    );
    SystemCapabilities {
        schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![BackendCapabilities {
            backend_id,
            display_name: "N09 software H.264/VP9 fixture backend".to_owned(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            raw_supported_outputs: vec![h264.clone(), vp9.clone()],
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            diagnostics: Vec::new(),
        }],
        render_backends: Vec::new(),
        playable_video_outputs: vec![h264, vp9],
    }
}

/// Settings запрещают extractor и сохраняют user codec preference только как policy input.
pub(super) fn native_settings() -> WebMediaOpenSettings {
    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    app_config.player.preferred_video_codec_order = vec![VideoCodec::H264, VideoCodec::Vp9];
    WebMediaOpenSettings::from_app_config(
        &app_config,
        &dash_system_capabilities(),
        ProductionAudioDecoderFactory::default().audio_decode_capability_snapshot(),
    )
}

/// Вызывает direct static DASH admission с optional installed semantic selection.
pub(super) fn prepare_native(
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
    .expect("native static DASH admission должен пройти")
    {
        NativeDashAttempt::Prepared(prepared) => prepared,
        NativeDashAttempt::RequiresExtractorFallback(trigger) => {
            panic!("валидный static MPD не имеет права требовать extractor: {trigger:?}")
        }
    }
}

/// Возвращает exact codec текущей muxed row до передачи packets consumer-ам.
fn selected_video_codec(prepared: &PreparedNativeDashMedia) -> DecodeVideoCodec {
    let codec_id = &prepared
        .demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .expect("native DASH row должна содержать video track")
        .codec_id;
    match codec_id.as_str() {
        "V_MPEG4/ISO/AVC" => DecodeVideoCodec::H264,
        "V_VP9" => DecodeVideoCodec::Vp9,
        unexpected => panic!("N09 получил неожиданный video codec: {unexpected}"),
    }
}

/// Catalog обязан содержать только две физически доказанные coupled rows.
fn alternate_component_selection(
    source_state: &NativeDashSourceState,
) -> web_media_core::ComponentVariantSemanticSelectionRequest {
    let stream_configuration = source_state.stream_configuration();
    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::Coupled {
            catalog_generation,
            coupled,
        },
    ) = stream_configuration.component_variant_projection()
    else {
        panic!("native DASH MPD должен публиковать coupled fMP4/WebM catalog");
    };
    assert_eq!(
        coupled.variants.len(),
        2,
        "catalog не должен синтезировать fake Cartesian combinations"
    );
    let alternate_index = usize::from(coupled.active_index == 0);
    let resolution = stream_configuration
        .resolve_component_variant_action(ComponentVariantSelectionAction {
            parent_generation: stream_configuration.generation(),
            catalog_generation,
            axis: WebMediaComponentVariantAxisKind::Coupled,
            variant_index: alternate_index,
        })
        .expect("fresh DASH component action должен разрешиться");
    let ComponentVariantActionResolution::SemanticReopen(selection) = resolution else {
        panic!("alternate DASH row не должна разрешаться как NoChange");
    };
    selection
}

/// Neutral app request не имеет права превратиться в extractor DTO на switch/reopen.
fn native_request_parts(
    request: WebMediaOpenRequest,
) -> (
    NativeDashUrl,
    web_media_core::WebMediaSemanticSelectionRequest,
    WebMediaOpenSettings,
) {
    let WebMediaOpenAdapterView::NativeDash {
        source,
        intent: NativeDashOpenIntent::SemanticSelection(selection),
        settings,
    } = request.into_adapter()
    else {
        panic!("native DASH action обязан сохранить native semantic request");
    };
    (source, selection, settings)
}

/// N14A: DASH fMP4 и WebM rows доходят до consumers без reopen/queue orchestration.
#[test]
fn n14a_consumer_dash_vod_fmp4_and_webm_reach_consumers_with_exact_accounting() {
    let server = ControlledHlsServer::start(fixture_routes());
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let mut settings = native_settings();
    process_spy.install_as_attempt_owner(&mut settings);
    let source = NativeDashUrl::new(
        server.target("/manifest.mpd"),
        SafeMediaLabel::from_service_safe_label("N14A native DASH VOD"),
    );
    assert_eq!(server.request_count("/manifest.mpd"), 0);
    assert_eq!(server.response_body_bytes("/manifest.mpd"), 0);
    let mut wgpu_harness = OffscreenWgpuHarness::new();

    let mut fmp4_media = prepare_native(&source, None, &settings);
    assert_eq!(selected_video_codec(&fmp4_media), DecodeVideoCodec::H264);
    assert_decoder_render_audio_for_codec(
        fmp4_media.demuxer.as_mut(),
        &mut wgpu_harness,
        DecodeVideoCodec::H264,
    );
    let webm_selection = alternate_component_selection(&fmp4_media.source_state);
    let source_intent = WebMediaSourceIntent::native_dash(
        source.clone(),
        web_media_core::WebMediaPresentationKind::Vod,
        fmp4_media.source_state,
    );
    let WebMediaSelectionSwitchResolution::Ready(webm_request) = source_intent
        .selection_switch_request(
            WebMediaSelectionSwitchIntent::ComponentSemantic(webm_selection),
            settings.clone(),
        )
    else {
        panic!("N14A WebM row должна создать exact native selection request");
    };
    let (webm_source, webm_selection, webm_settings) = native_request_parts(webm_request);
    let mut webm_media = prepare_native(&webm_source, Some(&webm_selection), &webm_settings);
    assert_eq!(selected_video_codec(&webm_media), DecodeVideoCodec::Vp9);
    assert_decoder_render_audio_for_codec(
        webm_media.demuxer.as_mut(),
        &mut wgpu_harness,
        DecodeVideoCodec::Vp9,
    );

    assert_eq!(server.request_count("/manifest.mpd"), 2);
    assert_eq!(
        server.response_body_bytes("/manifest.mpd"),
        static_manifest(false).len() + static_manifest(true).len()
    );
    assert_eq!(process_spy.invocation_count(), 0);
}

/// Ждёт authoritative worker receipt и проверяет requested VOD position.
fn assert_vod_seek(
    seek_port: &dyn PreparedDemuxSeekPort,
    request_id: u64,
    requested_position: Duration,
) {
    let request_id = PreparedDemuxSeekRequestId::new(request_id);
    seek_port
        .enqueue_seek(request_id, DemuxSeekRequest::accurate(requested_position))
        .expect("native DASH VOD seek должен войти в worker");
    let deadline = Instant::now() + DASH_VERTICAL_DEADLINE;
    loop {
        if let Some(receipt) = seek_port.poll_seek_receipt() {
            assert_eq!(receipt.request_id, request_id);
            let PreparedDemuxSeekOutcome::Succeeded(result) = receipt.outcome else {
                panic!(
                    "native DASH seek должен завершиться успехом: {:?}",
                    receipt.outcome
                );
            };
            assert_eq!(result.requested_position.as_duration(), requested_position);
            return;
        }
        assert!(
            Instant::now() < deadline,
            "native DASH seek receipt timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

/// Доказывает one-fetch root, обе реальные rows, VOD seek, switch/reopen и process spy 0.
#[test]
fn n14b_lifecycle_dash_vod_seek_forward_back_switch_and_reopen_reaches_consumers() {
    let server = ControlledHlsServer::start(fixture_routes());
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let mut settings = native_settings();
    process_spy.install_as_attempt_owner(&mut settings);
    let source = NativeDashUrl::new(
        server.target("/manifest.mpd"),
        SafeMediaLabel::from_service_safe_label("controlled native DASH MPD"),
    );
    assert_eq!(
        server.request_count("/manifest.mpd"),
        0,
        "syntactic DASH classifier не должен fetch-ить root до open"
    );
    let stable_source_identity = source.source_identity();
    let mut wgpu_harness = OffscreenWgpuHarness::new();

    let mut initial = prepare_native(&source, None, &settings);
    assert_eq!(server.request_count("/manifest.mpd"), 1);
    let initial_codec = selected_video_codec(&initial);
    assert_eq!(initial_codec, DecodeVideoCodec::H264);
    assert_vod_seek(initial.seek_port.as_ref(), 1, DASH_SEEK_POSITION);
    assert_decoder_render_audio_for_codec(
        initial.demuxer.as_mut(),
        &mut wgpu_harness,
        initial_codec,
    );
    assert_vod_seek(initial.seek_port.as_ref(), 2, Duration::ZERO);
    assert_decoder_render_audio_for_codec(
        initial.demuxer.as_mut(),
        &mut wgpu_harness,
        initial_codec,
    );
    let alternate_selection = alternate_component_selection(&initial.source_state);
    let expected_alternate = alternate_selection.clone();
    let initial_intent = WebMediaSourceIntent::native_dash(
        source.clone(),
        web_media_core::WebMediaPresentationKind::Vod,
        initial.source_state,
    );
    assert_eq!(
        initial_intent.recovery(),
        web_media_core::WebMediaRecoveryStrategy::RefreshRootManifestAndRematch
    );

    let WebMediaSelectionSwitchResolution::Ready(switch_request) = initial_intent
        .selection_switch_request(
            WebMediaSelectionSwitchIntent::ComponentSemantic(alternate_selection),
            settings.clone(),
        )
    else {
        panic!("fresh DASH row action обязан запустить exact same-item switch");
    };
    let (switch_source, switch_selection, switch_settings) = native_request_parts(switch_request);
    let mut switched = prepare_native(&switch_source, Some(&switch_selection), &switch_settings);
    assert_eq!(server.request_count("/manifest.mpd"), 2);
    let switched_codec = selected_video_codec(&switched);
    assert_eq!(switched_codec, DecodeVideoCodec::Vp9);
    assert_decoder_render_audio_for_codec(
        switched.demuxer.as_mut(),
        &mut wgpu_harness,
        switched_codec,
    );
    let web_media_core::WebMediaSelectionShape::Components(switched_components) =
        switched.source_state.neutral_selection().shape()
    else {
        panic!("switched DASH selection должна сохранить component shape");
    };
    assert_eq!(
        switched_components.semantic_rematch_request(),
        expected_alternate,
        "row reorder не должен менять exact semantic selection"
    );
    assert_eq!(
        switched
            .source_state
            .neutral_selection()
            .parent()
            .exact()
            .source(),
        stable_source_identity,
        "switch обязан сохранить stable source lineage"
    );

    let switched_intent = WebMediaSourceIntent::native_dash(
        switch_source.clone(),
        web_media_core::WebMediaPresentationKind::Vod,
        switched.source_state,
    );
    let reopen_request = switched_intent
        .controlled_reopen_request(
            switch_settings.network_config.clone(),
            switch_settings.demux_config,
            Some(switch_settings.clone()),
        )
        .expect("native DASH controlled reopen требует semantic rematch");
    let (reopen_source, reopen_selection, reopen_settings) = native_request_parts(reopen_request);
    let mut reopened = prepare_native(&reopen_source, Some(&reopen_selection), &reopen_settings);
    assert_eq!(server.request_count("/manifest.mpd"), 3);
    let reopened_codec = selected_video_codec(&reopened);
    assert_eq!(reopened_codec, DecodeVideoCodec::Vp9);
    assert_decoder_render_audio_for_codec(
        reopened.demuxer.as_mut(),
        &mut wgpu_harness,
        reopened_codec,
    );
    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::Coupled { coupled, .. },
    ) = reopened
        .source_state
        .stream_configuration()
        .component_variant_projection()
    else {
        panic!("root refresh должен заново опубликовать coupled catalog");
    };
    assert_eq!(coupled.variants.len(), 2);
    assert_eq!(
        reopened
            .source_state
            .neutral_selection()
            .parent()
            .exact()
            .source(),
        stable_source_identity,
        "root refresh/rematch не должен менять stable source lineage"
    );
    assert_eq!(
        process_spy.invocation_count(),
        0,
        "valid DASH open, seek, switch и reopen не запускают extractor"
    );
}
