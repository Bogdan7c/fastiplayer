//! N12 vertical: direct `.f4m` -> existing HDS/F4F runtime -> production consumers.

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
    VideoCodec as DecodeVideoCodec, VideoProfile,
};
use fastiplayer_config::{AppConfig, VideoCodec};
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer};
use player_core::{PreparedDemuxSeekOutcome, PreparedDemuxSeekPort, PreparedDemuxSeekRequestId};
use source_core::CancellationToken;
use video_frame_contract::VideoFrameContract;
use web_media_core::ComponentVariantSemanticSelectionRequest;

use super::super::*;
use super::native_hls_vertical::{ControlledHlsServer, assert_decoder_render_audio_for_codec};
use crate::media_open::{NativeHdsOpenIntent, NativeHdsSourceState, NativeHdsUrl, SafeMediaLabel};
use crate::startup_media::native_hds::{
    NativeHdsAttempt, NativeHdsPreparationRequest, PreparedNativeHdsMedia, native_hds_failure_kind,
    prepare_native_hds_attempt,
};
use crate::web_media_open::content_probe_tests::direct_progressive::ZeroProcessSpy;
use crate::web_media_open::content_probe_tests::direct_progressive_webm::OffscreenWgpuHarness;
use crate::web_media_stream_model::component_variants::{
    ComponentVariantActionResolution, ComponentVariantSelectionAction,
    WebMediaComponentVariantAxisKind, WebMediaComponentVariantProjection,
    WebMediaInstalledComponentVariantPresentation,
};

/// Decoder/render/audio и worker receipt не должны ждать бесконечно.
const HDS_VERTICAL_DEADLINE: Duration = Duration::from_secs(15);
/// Второй one-second fragment — authoritative preceding anchor для этого seek-а.
const HDS_SEEK_POSITION: Duration = Duration::from_millis(1_100);

/// FFmpeg-generated constrained-baseline Annex-B fixture; command documented в test body.
const H264_ANNEX_B_BASE64: &str = include_str!("fixtures/hds-h264.h264.base64");
/// FFmpeg-generated AAC-LC/44.1 kHz ADTS fixture с тремя non-silent frames.
const AAC_ADTS_BASE64: &str = include_str!("fixtures/hds-aac.aac.base64");

/// Snapshot admits exact constrained-baseline H.264 fixture rows.
fn h264_system_capabilities() -> SystemCapabilities {
    let backend_id = DecodeBackendId::new("nhds").expect("valid backend ID");
    let output = SupportedVideoOutput {
        backend: backend_id.clone(),
        decode_format: SupportedVideoDecodeFormat {
            codec: DecodeVideoCodec::H264,
            profile: VideoProfile::H264(H264Profile::ConstrainedBaseline),
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            max_width: Some(1_920),
            max_height: Some(1_080),
            max_fps: Some(60.0),
            hdr_input: false,
        },
        frame_contract: VideoFrameContract::host_yuv420_planar8(),
    };
    SystemCapabilities {
        schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![BackendCapabilities {
            backend_id,
            display_name: "N12 software H.264 fixture backend".to_owned(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            raw_supported_outputs: vec![output.clone()],
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            diagnostics: Vec::new(),
        }],
        render_backends: Vec::new(),
        playable_video_outputs: vec![output],
    }
}

/// Settings запрещают extractor и используют production AAC capability probe.
fn native_settings() -> WebMediaOpenSettings {
    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    app_config.web_media.preferred_video_height =
        Some(fastiplayer_config::PreferredVideoHeight::new(1_080).expect("valid HDS height"));
    app_config.player.preferred_video_codec_order = vec![VideoCodec::H264];
    let audio_capabilities =
        ProductionAudioDecoderFactory::default().audio_decode_capability_snapshot();
    WebMediaOpenSettings::from_app_config(
        &app_config,
        &h264_system_capabilities(),
        audio_capabilities,
    )
}

/// Выполняет production native preparation и запрещает fallback для valid fixture-а.
fn prepare_native(
    source: &NativeHdsUrl,
    expected_selection: Option<&web_media_core::WebMediaSemanticSelectionRequest>,
    settings: &WebMediaOpenSettings,
) -> PreparedNativeHdsMedia {
    match prepare_native_hds_attempt(NativeHdsPreparationRequest {
        source,
        expected_selection,
        network_config: &settings.network_config,
        web_media_config: &settings.web_media_config,
        demux_config: &settings.demux_config,
        system_capabilities: &settings.system_capabilities,
        audio_capabilities: settings.audio_capabilities,
        cancellation: CancellationToken::new(),
    })
    .expect("supported direct HDS preparation")
    {
        NativeHdsAttempt::Prepared(prepared) => prepared,
        NativeHdsAttempt::RequiresExtractorFallback(trigger) => {
            panic!("valid HDS VOD не имеет права требовать extractor: {trigger:?}")
        }
    }
}

/// Выбирает вторую playable coupled rendition одной semantic action-операцией.
fn alternate_coupled_selection(
    source_state: &NativeHdsSourceState,
) -> ComponentVariantSemanticSelectionRequest {
    let stream_configuration = source_state.stream_configuration();
    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::Coupled {
            catalog_generation,
            coupled,
        },
    ) = stream_configuration.component_variant_projection()
    else {
        panic!("native HDS обязан публиковать coupled rendition catalog");
    };
    assert_eq!(coupled.variants.len(), 2, "обе HDS rows должны иметь proof");
    let alternate_index = usize::from(coupled.active_index == 0);
    let resolution = stream_configuration
        .resolve_component_variant_action(ComponentVariantSelectionAction {
            parent_generation: stream_configuration.generation(),
            catalog_generation,
            axis: WebMediaComponentVariantAxisKind::Coupled,
            variant_index: alternate_index,
        })
        .expect("fresh HDS coupled action должна разрешиться");
    let ComponentVariantActionResolution::SemanticReopen(selection) = resolution else {
        panic!("alternate HDS row не должна быть NoChange");
    };
    selection
}

/// Извлекает source/semantic intent/settings из neutral same-item request-а.
fn native_request_parts(
    request: WebMediaOpenRequest,
) -> (
    NativeHdsUrl,
    web_media_core::WebMediaSemanticSelectionRequest,
    WebMediaOpenSettings,
) {
    let WebMediaOpenAdapterView::NativeHds {
        source,
        intent: NativeHdsOpenIntent::SemanticSelection(selection),
        settings,
    } = request.into_adapter()
    else {
        panic!("HDS switch/reopen обязан сохранить native semantic adapter intent");
    };
    (source, selection, settings)
}

/// Ждёт authoritative transactional seek receipt от existing S38 worker-а.
fn assert_vod_seek(
    seek_port: &dyn PreparedDemuxSeekPort,
    request_id: u64,
    requested_position: Duration,
) {
    let request_id = PreparedDemuxSeekRequestId::new(request_id);
    seek_port
        .enqueue_seek(request_id, DemuxSeekRequest::accurate(requested_position))
        .expect("native HDS seek должен войти в worker");
    let deadline = Instant::now() + HDS_VERTICAL_DEADLINE;
    loop {
        if let Some(receipt) = seek_port.poll_seek_receipt() {
            assert_eq!(receipt.request_id, request_id);
            let PreparedDemuxSeekOutcome::Succeeded(result) = receipt.outcome else {
                panic!(
                    "native HDS seek завершился неуспешно: {:?}",
                    receipt.outcome
                );
            };
            assert_eq!(result.requested_position.as_duration(), requested_position);
            return;
        }
        assert!(Instant::now() < deadline, "native HDS seek receipt timeout");
        thread::sleep(Duration::from_millis(1));
    }
}

/// Probed demuxer переносит initial TracksChanged event в transactional runtime.
fn wait_for_tracks_changed(demuxer: &mut dyn Demuxer) {
    let deadline = Instant::now() + HDS_VERTICAL_DEADLINE;
    loop {
        assert!(
            Instant::now() < deadline,
            "native HDS track readiness timeout"
        );
        match demuxer.next_event().expect("native HDS readiness event") {
            DemuxReadEvent::TracksChanged(_) => return,
            DemuxReadEvent::TemporarilyUnavailable(hint) => thread::sleep(hint.retry_after()),
            other => panic!("tracks должны быть первой HDS publication: {other:?}"),
        }
    }
}

/// N14A: HDS VOD initial row достигает render/readback и PCM/clock без switch/reopen.
#[cfg(feature = "ffmpeg")]
#[test]
fn n14a_consumer_hds_vod_reaches_consumers_with_exact_accounting() {
    let server = ControlledHlsServer::start(fixture_routes());
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let mut settings = native_settings();
    process_spy.install_as_attempt_owner(&mut settings);
    let root_path = "/vod/root.f4m?token=n12-secret";
    let source = NativeHdsUrl::new(
        server.target(root_path),
        SafeMediaLabel::from_service_safe_label("N14A native HDS VOD"),
    );
    assert_eq!(server.request_count(root_path), 0);
    assert_eq!(server.response_body_bytes(root_path), 0);

    let mut prepared = prepare_native(&source, None, &settings);
    assert_exact_probe_accounting(&server, 1);
    assert_eq!(
        server.response_body_bytes(root_path),
        vod_manifest([("high", 1_080, 6_000), ("low", 720, 3_000)]).len()
    );
    wait_for_tracks_changed(prepared.demuxer.as_mut());
    let mut wgpu_harness = OffscreenWgpuHarness::new();
    assert_decoder_render_audio_for_codec(
        prepared.demuxer.as_mut(),
        &mut wgpu_harness,
        DecodeVideoCodec::H264,
    );
    assert_eq!(process_spy.invocation_count(), 0);
}

/// Доказывает root handoff, eager fragment reuse, render/audio, seek и refresh.
#[cfg(feature = "ffmpeg")]
#[test]
fn n14b_lifecycle_hds_vod_seek_forward_back_switch_and_reopen_reaches_consumers() {
    let server = ControlledHlsServer::start(fixture_routes());
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let mut settings = native_settings();
    process_spy.install_as_attempt_owner(&mut settings);
    let source = NativeHdsUrl::new(
        server.target("/vod/root.f4m?token=n12-secret"),
        SafeMediaLabel::from_service_safe_label("controlled native HDS F4M"),
    );
    assert_eq!(
        server.request_count("/vod/root.f4m?token=n12-secret"),
        0,
        "syntactic HDS classifier не должен fetch-ить root до open"
    );
    let stable_source_identity = source.source_identity();
    let mut wgpu_harness = OffscreenWgpuHarness::new();

    let mut initial = prepare_native(&source, None, &settings);
    assert_exact_probe_accounting(&server, 1);
    wait_for_tracks_changed(initial.demuxer.as_mut());
    assert_vod_seek(initial.seek_port.as_ref(), 12, HDS_SEEK_POSITION);
    assert_decoder_render_audio_for_codec(
        initial.demuxer.as_mut(),
        &mut wgpu_harness,
        DecodeVideoCodec::H264,
    );
    assert_vod_seek(initial.seek_port.as_ref(), 13, Duration::ZERO);
    assert_decoder_render_audio_for_codec(
        initial.demuxer.as_mut(),
        &mut wgpu_harness,
        DecodeVideoCodec::H264,
    );
    let accounting_after_initial_lifecycle = HdsProbeAccounting::observe(&server);
    let alternate_selection = alternate_coupled_selection(&initial.source_state);
    let expected_alternate = alternate_selection.clone();
    let initial_intent = WebMediaSourceIntent::native_hds(source.clone(), initial.source_state);

    let WebMediaSelectionSwitchResolution::Ready(switch_request) = initial_intent
        .selection_switch_request(
            WebMediaSelectionSwitchIntent::ComponentSemantic(alternate_selection),
            settings.clone(),
        )
    else {
        panic!("fresh HDS row action обязана запустить same-item switch");
    };
    let (switch_source, switch_selection, switch_settings) = native_request_parts(switch_request);
    let mut switched = prepare_native(&switch_source, Some(&switch_selection), &switch_settings);
    accounting_after_initial_lifecycle.assert_one_open_attempt_added(&server);
    wait_for_tracks_changed(switched.demuxer.as_mut());
    assert_decoder_render_audio_for_codec(
        switched.demuxer.as_mut(),
        &mut wgpu_harness,
        DecodeVideoCodec::H264,
    );
    let web_media_core::WebMediaSelectionShape::Components(switched_components) =
        switched.source_state.neutral_selection().shape()
    else {
        panic!("switched HDS selection должна сохранить component shape");
    };
    assert_eq!(
        switched_components.semantic_rematch_request(),
        expected_alternate
    );
    let accounting_after_switch_lifecycle = HdsProbeAccounting::observe(&server);

    let switched_intent =
        WebMediaSourceIntent::native_hds(switch_source.clone(), switched.source_state);
    let reopen_request = switched_intent
        .controlled_reopen_request(
            switch_settings.network_config.clone(),
            switch_settings.demux_config,
            Some(switch_settings.clone()),
        )
        .expect("native HDS controlled reopen требует semantic rematch");
    let (reopen_source, reopen_selection, reopen_settings) = native_request_parts(reopen_request);
    let mut reopened = prepare_native(&reopen_source, Some(&reopen_selection), &reopen_settings);
    accounting_after_switch_lifecycle.assert_one_open_attempt_added(&server);
    wait_for_tracks_changed(reopened.demuxer.as_mut());
    assert_decoder_render_audio_for_codec(
        reopened.demuxer.as_mut(),
        &mut wgpu_harness,
        DecodeVideoCodec::H264,
    );
    assert_eq!(
        reopened
            .source_state
            .neutral_selection()
            .parent()
            .exact()
            .source(),
        stable_source_identity,
        "stable-root refresh/rematch обязан сохранить source lineage"
    );
    assert_eq!(
        process_spy.invocation_count(),
        0,
        "valid HDS open, seek, switch и reopen не запускают extractor"
    );
}

/// Root и каждый eager-probed Frag1 выполняются ровно один раз на attempt.
fn assert_exact_probe_accounting(server: &ControlledHlsServer, attempts: usize) {
    assert_eq!(
        server.request_count("/vod/root.f4m?token=n12-secret"),
        attempts
    );
    assert_eq!(server.request_count("/vod/media/highSeg1-Frag1"), attempts);
    assert_eq!(server.request_count("/vod/media/lowSeg1-Frag1"), attempts);
}

/// Snapshot отделяет обязательные open probes от дополнительных fragment reads после seek.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HdsProbeAccounting {
    root_requests: usize,
    high_fragment_requests: usize,
    low_fragment_requests: usize,
}

impl HdsProbeAccounting {
    /// Снимает exact counters у существующего N12 loopback owner-а.
    fn observe(server: &ControlledHlsServer) -> Self {
        Self {
            root_requests: server.request_count("/vod/root.f4m?token=n12-secret"),
            high_fragment_requests: server.request_count("/vod/media/highSeg1-Frag1"),
            low_fragment_requests: server.request_count("/vod/media/lowSeg1-Frag1"),
        }
    }

    /// Новый switch/reopen attempt обязан добавить один root и по одному eager probe на row.
    fn assert_one_open_attempt_added(self, server: &ControlledHlsServer) {
        let current = Self::observe(server);
        assert_eq!(current.root_requests, self.root_requests + 1);
        assert_eq!(
            current.high_fragment_requests,
            self.high_fragment_requests + 1
        );
        assert_eq!(
            current.low_fragment_requests,
            self.low_fragment_requests + 1
        );
    }
}

/// Live/DRM/private/profile remain distinct; malformed/network/cancel never fallback.
#[test]
fn native_hds_keeps_profile_and_terminal_failures_distinct_without_extractor() {
    let server = ControlledHlsServer::start(failure_routes());
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let mut settings = native_settings();
    process_spy.install_as_attempt_owner(&mut settings);
    let source = |path: &str| {
        NativeHdsUrl::new(
            server.target(path),
            SafeMediaLabel::from_service_safe_label("controlled HDS failure"),
        )
    };
    let prepare = |source: &NativeHdsUrl, cancellation: CancellationToken| {
        prepare_native_hds_attempt(NativeHdsPreparationRequest {
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
        prepare(&source("/foreign.f4m"), CancellationToken::new())
            .expect("foreign root должен стать typed initial fallback"),
        NativeHdsAttempt::RequiresExtractorFallback(
            web_media_core::WebMediaFallbackTrigger::ProviderDocument
        )
    ));
    for (path, expected_kind) in [
        (
            "/live.f4m",
            web_media_hds::HdsPrepareFailureKind::LiveProfile,
        ),
        (
            "/drm.f4m",
            web_media_hds::HdsPrepareFailureKind::DrmProtected,
        ),
        (
            "/private.f4m",
            web_media_hds::HdsPrepareFailureKind::PrivateExtension,
        ),
        (
            "/profile.f4m",
            web_media_hds::HdsPrepareFailureKind::UnsupportedProfile,
        ),
        (
            "/malformed.f4m",
            web_media_hds::HdsPrepareFailureKind::MalformedManifest,
        ),
    ] {
        let error = prepare(&source(path), CancellationToken::new())
            .err()
            .expect("profile/malformed failure не должна становиться fallback");
        assert_eq!(native_hds_failure_kind(&error), expected_kind, "{path}");
    }

    let network_error = prepare(&source("/missing.f4m"), CancellationToken::new())
        .err()
        .expect("404 root fetch обязан остаться terminal network failure");
    assert_eq!(
        native_hds_failure_kind(&network_error),
        web_media_hds::HdsPrepareFailureKind::Network
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let cancellation_error = prepare(&source("/vod.f4m"), cancelled)
        .err()
        .expect("pre-cancel не должен попасть в fallback");
    assert_eq!(
        native_hds_failure_kind(&cancellation_error),
        web_media_hds::HdsPrepareFailureKind::Cancelled
    );
    assert_eq!(process_spy.invocation_count(), 0);
}

/// Один root snapshot меняет порядок rows, но не semantic identity.
fn fixture_routes() -> HashMap<String, Vec<Vec<u8>>> {
    let first_manifest = vod_manifest([("high", 1_080, 6_000), ("low", 720, 3_000)]);
    let reordered_manifest = vod_manifest([("low", 720, 3_000), ("high", 1_080, 6_000)]);
    let first_fragment = f4f_fragment(0);
    let second_fragment = f4f_fragment(1_000);
    HashMap::from([
        (
            "/vod/root.f4m?token=n12-secret".to_owned(),
            vec![
                first_manifest,
                reordered_manifest.clone(),
                reordered_manifest,
            ],
        ),
        (
            "/vod/media/highSeg1-Frag1".to_owned(),
            vec![first_fragment.clone()],
        ),
        (
            "/vod/media/highSeg1-Frag2".to_owned(),
            vec![second_fragment.clone()],
        ),
        ("/vod/media/lowSeg1-Frag1".to_owned(), vec![first_fragment]),
        ("/vod/media/lowSeg1-Frag2".to_owned(), vec![second_fragment]),
    ])
}

/// Parser failure corpus различает admission categories до runtime fallback-а.
fn failure_routes() -> HashMap<String, Vec<Vec<u8>>> {
    HashMap::from([
        ("/foreign.f4m".to_owned(), vec![b"<html/>".to_vec()]),
        (
            "/live.f4m".to_owned(),
            vec![br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><streamType>live</streamType><media url="video"/></manifest>"#.to_vec()],
        ),
        (
            "/drm.f4m".to_owned(),
            vec![br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><drmAdditionalHeader/><media url="video"/></manifest>"#.to_vec()],
        ),
        (
            "/private.f4m".to_owned(),
            vec![br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><x:media xmlns:x="urn:private" url="video"/></manifest>"#.to_vec()],
        ),
        (
            "/profile.f4m".to_owned(),
            vec![br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0"><cueInfo/><media url="video"/></manifest>"#.to_vec()],
        ),
        (
            "/malformed.f4m".to_owned(),
            vec![br#"<manifest xmlns="http://ns.adobe.com/f4m/1.0">"#.to_vec()],
        ),
    ])
}

/// Строит two-row F4M с inline bootstrap, поэтому accounting касается root/F4F.
fn vod_manifest(rows: [(&str, u32, u64); 2]) -> Vec<u8> {
    let bootstrap = base64::engine::general_purpose::STANDARD.encode(vod_bootstrap());
    let media = rows
        .into_iter()
        .map(|(url, height, bitrate)| {
            format!(
                "<media url=\"{url}\" bitrate=\"{bitrate}\" width=\"1920\" height=\"{height}\" bootstrapInfoId=\"boot\"/>"
            )
        })
        .collect::<String>();
    format!(
        "<manifest xmlns=\"http://ns.adobe.com/f4m/1.0\"><streamType>recorded</streamType><duration>2</duration><baseURL>media/</baseURL>{media}<bootstrapInfo id=\"boot\">{bootstrap}</bootstrapInfo></manifest>"
    )
    .into_bytes()
}

/// Строит VOD `abst/asrt/afrt` с двумя fragments и terminal marker-ом.
fn vod_bootstrap() -> Vec<u8> {
    let segment_table = segment_run_table(2);
    let fragment_table = fragment_run_table();
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.push(0);
    payload.extend_from_slice(&1_000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u64.to_be_bytes());
    payload.extend_from_slice(&0_u64.to_be_bytes());
    payload.extend_from_slice(b"n12\0");
    payload.push(0);
    payload.push(0);
    payload.extend_from_slice(b"\0\0");
    payload.push(1);
    payload.extend_from_slice(&segment_table);
    payload.push(1);
    payload.extend_from_slice(&fragment_table);
    iso_box(b"abst", &payload)
}

/// `asrt` сопоставляет оба fragments одному segment-у.
fn segment_run_table(fragments_per_segment: u32) -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0, 0];
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&fragments_per_segment.to_be_bytes());
    iso_box(b"asrt", &payload)
}

/// `afrt` задаёт два секундных fragments и конец presentation.
fn fragment_run_table() -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0];
    payload.extend_from_slice(&1_000_u32.to_be_bytes());
    payload.push(0);
    payload.extend_from_slice(&2_u32.to_be_bytes());
    append_fragment_run(&mut payload, 1, 0, 1_000, None);
    append_fragment_run(&mut payload, 0, 0, 0, Some(0));
    iso_box(b"afrt", &payload)
}

/// Кодирует одну wire-level Adobe FRAGMENTRUNENTRY.
fn append_fragment_run(
    payload: &mut Vec<u8>,
    first_fragment: u32,
    first_timestamp: u64,
    duration: u32,
    discontinuity: Option<u8>,
) {
    payload.extend_from_slice(&first_fragment.to_be_bytes());
    payload.extend_from_slice(&first_timestamp.to_be_bytes());
    payload.extend_from_slice(&duration.to_be_bytes());
    if let Some(indicator) = discontinuity {
        payload.push(indicator);
    }
}

/// Собирает F4F fragment из реальных decodable H.264/AAC access units.
fn f4f_fragment(timestamp: u32) -> Vec<u8> {
    let h264 = base64::engine::general_purpose::STANDARD
        .decode(H264_ANNEX_B_BASE64.trim())
        .expect("valid H.264 base64 fixture");
    let aac = base64::engine::general_purpose::STANDARD
        .decode(AAC_ADTS_BASE64.trim())
        .expect("valid AAC base64 fixture");
    let (avc_sequence, avc_access_unit) = flv_avc_payloads(&h264);
    let (aac_sequence, aac_frames) = flv_aac_payloads(&aac);

    let mut flv_tags = flv_tag(9, timestamp, &avc_sequence);
    flv_tags.extend_from_slice(&flv_tag(8, timestamp, &aac_sequence));
    for (index, frame) in aac_frames.iter().enumerate() {
        let frame_timestamp = timestamp + u32::try_from(index * 23).expect("AAC timestamp");
        flv_tags.extend_from_slice(&flv_tag(8, frame_timestamp, frame));
    }
    flv_tags.extend_from_slice(&flv_tag(9, timestamp + 40, &avc_access_unit));

    let mut fragment = f4f_afra();
    fragment.extend_from_slice(&f4f_moof());
    fragment.extend_from_slice(&iso_box(b"mdat", &flv_tags));
    fragment
}

/// Конвертирует Annex-B SPS/PPS/IDR в FLV AVC sequence + length-prefixed AU.
fn flv_avc_payloads(annex_b: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let nal_units = annex_b_nal_units(annex_b);
    let sps = nal_units
        .iter()
        .find(|nal| nal.first().is_some_and(|byte| byte & 0x1f == 7))
        .expect("fixture SPS");
    let pps = nal_units
        .iter()
        .find(|nal| nal.first().is_some_and(|byte| byte & 0x1f == 8))
        .expect("fixture PPS");
    let mut sequence = vec![0x17, 0, 0, 0, 0, 1, sps[1], sps[2], sps[3], 0xff, 0xe1];
    sequence.extend_from_slice(&u16::try_from(sps.len()).expect("SPS length").to_be_bytes());
    sequence.extend_from_slice(sps);
    sequence.push(1);
    sequence.extend_from_slice(&u16::try_from(pps.len()).expect("PPS length").to_be_bytes());
    sequence.extend_from_slice(pps);

    let mut access_unit = vec![0x17, 1, 0, 0, 0];
    for nal in nal_units.into_iter().filter(|nal| {
        nal.first()
            .is_some_and(|byte| !matches!(byte & 0x1f, 7 | 8))
    }) {
        access_unit.extend_from_slice(&u32::try_from(nal.len()).expect("NAL length").to_be_bytes());
        access_unit.extend_from_slice(nal);
    }
    (sequence, access_unit)
}

fn annex_b_nal_units(bytes: &[u8]) -> Vec<&[u8]> {
    let mut boundaries = Vec::new();
    let mut cursor = 0;
    while cursor + 3 <= bytes.len() {
        let start_code_length = if bytes[cursor..].starts_with(&[0, 0, 0, 1]) {
            Some(4)
        } else if bytes[cursor..].starts_with(&[0, 0, 1]) {
            Some(3)
        } else {
            None
        };
        if let Some(length) = start_code_length {
            boundaries.push((cursor, cursor + length));
            cursor += length;
        } else {
            cursor += 1;
        }
    }
    boundaries
        .iter()
        .enumerate()
        .map(|(index, (_, content_start))| {
            let content_end = boundaries
                .get(index + 1)
                .map_or(bytes.len(), |(next_start_code, _)| *next_start_code);
            &bytes[*content_start..content_end]
        })
        .filter(|nal| !nal.is_empty())
        .collect()
}

/// Конвертирует ADTS header в AudioSpecificConfig и raw AAC FLV frames.
fn flv_aac_payloads(adts: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut cursor = 0;
    let mut sequence = None;
    let mut frames = Vec::new();
    while cursor + 7 <= adts.len() {
        assert_eq!(&adts[cursor..cursor + 2], &[0xff, adts[cursor + 1]]);
        assert_eq!(adts[cursor + 1] & 0xf6, 0xf0, "ADTS sync/header");
        let frequency_index = (adts[cursor + 2] >> 2) & 0x0f;
        let channel_configuration = ((adts[cursor + 2] & 1) << 2) | (adts[cursor + 3] >> 6);
        let audio_object_type = ((adts[cursor + 2] >> 6) & 0x03) + 1;
        sequence.get_or_insert_with(|| {
            vec![
                0xaf,
                0,
                (audio_object_type << 3) | (frequency_index >> 1),
                (frequency_index << 7) | (channel_configuration << 3),
            ]
        });
        let frame_length = (usize::from(adts[cursor + 3] & 0x03) << 11)
            | (usize::from(adts[cursor + 4]) << 3)
            | usize::from(adts[cursor + 5] >> 5);
        let header_length = if adts[cursor + 1] & 1 == 0 { 9 } else { 7 };
        let frame_end = cursor + frame_length;
        assert!(frame_end <= adts.len(), "bounded ADTS frame");
        let mut payload = vec![0xaf, 1];
        payload.extend_from_slice(&adts[cursor + header_length..frame_end]);
        frames.push(payload);
        cursor = frame_end;
    }
    (sequence.expect("AAC sequence"), frames)
}

/// Минимальный valid `afra` для production F4F topology validator-а.
fn f4f_afra() -> Vec<u8> {
    let mut payload = vec![0, 0, 0, 0, 0];
    payload.extend_from_slice(&1_000_u32.to_be_bytes());
    payload.extend_from_slice(&0_u32.to_be_bytes());
    iso_box(b"afra", &payload)
}

/// Минимальный `moof` с одним declared track run.
fn f4f_moof() -> Vec<u8> {
    let mut movie_header = vec![0, 0, 0, 0];
    movie_header.extend_from_slice(&1_u32.to_be_bytes());
    let mut track_header = vec![0, 0, 0, 0];
    track_header.extend_from_slice(&1_u32.to_be_bytes());
    let mut track_run = vec![0, 0, 0, 0];
    track_run.extend_from_slice(&1_u32.to_be_bytes());
    let mut track_fragment = iso_box(b"tfhd", &track_header);
    track_fragment.extend_from_slice(&iso_box(b"trun", &track_run));
    let mut payload = iso_box(b"mfhd", &movie_header);
    payload.extend_from_slice(&iso_box(b"traf", &track_fragment));
    iso_box(b"moof", &payload)
}

/// Кодирует ISO BMFF box только для test fixture bytes.
fn iso_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(8 + payload.len()).expect("HDS fixture box fits u32");
    let mut bytes = Vec::with_capacity(8 + payload.len());
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(box_type);
    bytes.extend_from_slice(payload);
    bytes
}

/// Кодирует headerless FLV tag внутри F4F `mdat`.
fn flv_tag(tag_type: u8, timestamp: u32, payload: &[u8]) -> Vec<u8> {
    let payload_size = u32::try_from(payload.len()).expect("FLV fixture payload fits u32");
    let timestamp_bytes = timestamp.to_be_bytes();
    let mut bytes = Vec::new();
    bytes.push(tag_type);
    bytes.extend_from_slice(&payload_size.to_be_bytes()[1..]);
    bytes.extend_from_slice(&timestamp_bytes[1..]);
    bytes.push(timestamp_bytes[0]);
    bytes.extend_from_slice(&[0, 0, 0]);
    bytes.extend_from_slice(payload);
    let tag_size = u32::try_from(11 + payload.len()).expect("FLV fixture tag fits u32");
    bytes.extend_from_slice(&tag_size.to_be_bytes());
    bytes
}
