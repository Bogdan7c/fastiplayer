//! N11 vertical: direct `/Manifest` -> existing Smooth runtime -> production consumers.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use audio::{AudioDecodeCapabilityProvider, ProductionAudioDecoderFactory};
use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, CURRENT_CAPABILITY_SCHEMA_VERSION,
    SupportedVideoOutput, SystemCapabilities,
};
use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, H264Profile, SupportedVideoDecodeFormat,
    VideoCodec as DecodeVideoCodec, VideoProfile,
};
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer};
use player_core::{PreparedDemuxSeekOutcome, PreparedDemuxSeekPort, PreparedDemuxSeekRequestId};
use rustiplayer_config::{AppConfig, VideoCodec};
use source_core::CancellationToken;
use video_frame_contract::VideoFrameContract;
use web_media_core::ComponentVariantSemanticSelectionRequest;

use super::super::*;
use super::native_hls_vertical::{ControlledHlsServer, assert_decoder_render_audio_for_codec};
use crate::media_open::{
    NativeSmoothOpenIntent, NativeSmoothSourceState, NativeSmoothUrl, SafeMediaLabel,
};
use crate::startup_media::native_smooth::{
    NativeSmoothAttempt, NativeSmoothPreparationRequest, PreparedNativeSmoothMedia,
    prepare_native_smooth_attempt,
};
use crate::web_media_open::content_probe_tests::direct_progressive::ZeroProcessSpy;
use crate::web_media_open::content_probe_tests::direct_progressive_webm::OffscreenWgpuHarness;
use crate::web_media_stream_model::component_variants::{
    ComponentVariantActionResolution, ComponentVariantSelectionAction,
    WebMediaComponentVariantAxisKind, WebMediaComponentVariantProjection,
    WebMediaInstalledComponentVariantPresentation,
};

/// Decoder/render/audio и worker receipt не должны ждать бесконечно.
const SMOOTH_VERTICAL_DEADLINE: Duration = Duration::from_secs(15);
/// Второй canonical fragment начинается в четыре секунды по video clock-у.
const SMOOTH_SEEK_POSITION: Duration = Duration::from_millis(4_100);
/// Authoritative canonical manifest из существующего S36 fixture corpus-а.
const SMOOTH_MANIFEST: &[u8] = include_bytes!(
    "../../../../../symphonia-format-isomp4-patch/fixtures/smooth-piff/tears-of-steel.ismc"
);
/// Baseline low-quality fragment первого four-second interval-а.
const VIDEO_LOW_FIRST: &[u8] = include_bytes!(
    "../../../../../symphonia-format-isomp4-patch/fixtures/smooth-piff/video-401000-0.bin"
);
/// High-quality fragment первого four-second interval-а.
const VIDEO_HIGH_FIRST: &[u8] = include_bytes!(
    "../../../../../symphonia-format-isomp4-patch/fixtures/smooth-piff/video-1501000-0.bin"
);
/// High-quality fragment после seek anchor-а 4 s.
const VIDEO_HIGH_SECOND: &[u8] = include_bytes!(
    "../../../../../symphonia-format-isomp4-patch/fixtures/smooth-piff/video-1501000-40000000.bin"
);
/// AAC-LC fragment первого exact audio interval-а.
const AUDIO_FIRST: &[u8] = include_bytes!(
    "../../../../../symphonia-format-isomp4-patch/fixtures/smooth-piff/audio-64008-0.bin"
);
/// AAC-LC fragment после independent 3.968 s audio anchor-а.
const AUDIO_SECOND: &[u8] = include_bytes!(
    "../../../../../symphonia-format-isomp4-patch/fixtures/smooth-piff/audio-64008-39680000.bin"
);

/// Loopback origin обслуживает только реально доказанные component rows.
pub(super) fn fixture_routes() -> HashMap<String, Vec<Vec<u8>>> {
    HashMap::from([
        ("/vod/Manifest".to_owned(), vec![SMOOTH_MANIFEST.to_vec()]),
        (
            "/vod/QualityLevels(401000)/Fragments(video_eng=0)".to_owned(),
            vec![VIDEO_LOW_FIRST.to_vec()],
        ),
        (
            "/vod/QualityLevels(1501000)/Fragments(video_eng=0)".to_owned(),
            vec![VIDEO_HIGH_FIRST.to_vec()],
        ),
        (
            "/vod/QualityLevels(1501000)/Fragments(video_eng=40000000)".to_owned(),
            vec![VIDEO_HIGH_SECOND.to_vec()],
        ),
        (
            "/vod/QualityLevels(64008)/Fragments(audio_eng=0)".to_owned(),
            vec![AUDIO_FIRST.to_vec()],
        ),
        (
            "/vod/QualityLevels(64008)/Fragments(audio_eng=39680000)".to_owned(),
            vec![AUDIO_SECOND.to_vec()],
        ),
        (
            "/vod/QualityLevels(128002)/Fragments(audio_eng=0)".to_owned(),
            vec![AUDIO_FIRST.to_vec()],
        ),
        (
            "/vod/QualityLevels(128002)/Fragments(audio_eng=39680000)".to_owned(),
            vec![AUDIO_SECOND.to_vec()],
        ),
    ])
}

/// Snapshot admits both canonical baseline и high H.264 rows.
fn h264_system_capabilities() -> SystemCapabilities {
    let backend_id = DecodeBackendId::new("nsmooth").expect("valid backend ID");
    let outputs = [H264Profile::Baseline, H264Profile::High]
        .into_iter()
        .map(|profile| SupportedVideoOutput {
            backend: backend_id.clone(),
            decode_format: SupportedVideoDecodeFormat {
                codec: DecodeVideoCodec::H264,
                profile: VideoProfile::H264(profile),
                bit_depth: BitDepth::Eight,
                chroma: ChromaSubsampling::Yuv420,
                max_width: Some(1_920),
                max_height: Some(1_080),
                max_fps: Some(60.0),
                hdr_input: false,
            },
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
        })
        .collect::<Vec<_>>();
    SystemCapabilities {
        schema_version: CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![BackendCapabilities {
            backend_id,
            display_name: "N11 software H.264 fixture backend".to_owned(),
            status: BackendProbeStatus::Available,
            driver: BackendDriverInfo::default(),
            raw_supported_outputs: outputs.clone(),
            raw_profiles: Vec::new(),
            raw_entrypoints: Vec::new(),
            raw_rt_formats: Vec::new(),
            quirks: Vec::new(),
            diagnostics: Vec::new(),
        }],
        render_backends: Vec::new(),
        playable_video_outputs: outputs,
    }
}

/// Settings запрещают extractor и используют production AAC capability probe.
pub(super) fn native_settings() -> WebMediaOpenSettings {
    let mut app_config = AppConfig::default();
    app_config.yt_dlp.enabled = false;
    app_config.web_media.preferred_video_height = Some(
        rustiplayer_config::PreferredVideoHeight::new(750).expect("valid Smooth fixture height"),
    );
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
pub(super) fn prepare_native(
    source: &NativeSmoothUrl,
    expected_selection: Option<&web_media_core::WebMediaSemanticSelectionRequest>,
    settings: &WebMediaOpenSettings,
) -> PreparedNativeSmoothMedia {
    match prepare_native_smooth_attempt(NativeSmoothPreparationRequest {
        source,
        expected_selection,
        network_config: &settings.network_config,
        web_media_config: &settings.web_media_config,
        demux_config: &settings.demux_config,
        system_capabilities: &settings.system_capabilities,
        audio_capabilities: settings.audio_capabilities,
        cancellation: CancellationToken::new(),
    })
    .expect("supported direct Smooth preparation")
    {
        NativeSmoothAttempt::Prepared(prepared) => prepared,
        NativeSmoothAttempt::RequiresExtractorFallback(trigger) => {
            panic!("valid Smooth VOD не имеет права требовать extractor: {trigger:?}")
        }
    }
}

/// Выбирает вторую playable audio row, сохраняя exact active video axis.
fn alternate_audio_selection(
    source_state: &NativeSmoothSourceState,
) -> ComponentVariantSemanticSelectionRequest {
    let stream_configuration = source_state.stream_configuration();
    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::VideoAndAudio {
            catalog_generation,
            video,
            audio,
        },
    ) = stream_configuration.component_variant_projection()
    else {
        panic!("native Smooth обязан публиковать independent VideoAndAudio catalog");
    };
    assert_eq!(
        video.variants.len(),
        1,
        "ровно одна video row имеет demux proof"
    );
    assert_eq!(
        audio.variants.len(),
        2,
        "ровно две audio rows имеют content proof"
    );
    let alternate_index = usize::from(audio.active_index == 0);
    let resolution = stream_configuration
        .resolve_component_variant_action(ComponentVariantSelectionAction {
            parent_generation: stream_configuration.generation(),
            catalog_generation,
            axis: WebMediaComponentVariantAxisKind::Audio,
            variant_index: alternate_index,
        })
        .expect("fresh Smooth video action должна разрешиться");
    let ComponentVariantActionResolution::SemanticReopen(selection) = resolution else {
        panic!("alternate Smooth row не должна быть NoChange");
    };
    selection
}

/// Извлекает source/semantic intent/settings из neutral same-item request-а.
fn native_request_parts(
    request: WebMediaOpenRequest,
) -> (
    NativeSmoothUrl,
    web_media_core::WebMediaSemanticSelectionRequest,
    WebMediaOpenSettings,
) {
    let WebMediaOpenAdapterView::NativeSmooth {
        source,
        intent: NativeSmoothOpenIntent::SemanticSelection(selection),
        settings,
    } = request.into_adapter()
    else {
        panic!("Smooth switch/reopen обязан сохранить native semantic adapter intent");
    };
    (source, selection, settings)
}

/// Ждёт authoritative transactional seek receipt от existing S36 worker-а.
fn assert_vod_seek(
    seek_port: &dyn PreparedDemuxSeekPort,
    request_id: u64,
    requested_position: Duration,
) {
    let request_id = PreparedDemuxSeekRequestId::new(request_id);
    seek_port
        .enqueue_seek(request_id, DemuxSeekRequest::accurate(requested_position))
        .expect("native Smooth seek должен войти в worker");
    let deadline = Instant::now() + SMOOTH_VERTICAL_DEADLINE;
    loop {
        if let Some(receipt) = seek_port.poll_seek_receipt() {
            assert_eq!(receipt.request_id, request_id);
            let PreparedDemuxSeekOutcome::Succeeded(result) = receipt.outcome else {
                panic!(
                    "native Smooth seek завершился неуспешно: {:?}",
                    receipt.outcome
                );
            };
            assert_eq!(result.requested_position.as_duration(), requested_position);
            return;
        }
        assert!(
            Instant::now() < deadline,
            "native Smooth seek receipt timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

/// Existing Smooth runtime публикует stable A/V tracks асинхронно до seek admission.
pub(super) fn wait_for_tracks_changed(demuxer: &mut dyn Demuxer) {
    let deadline = Instant::now() + SMOOTH_VERTICAL_DEADLINE;
    loop {
        assert!(
            Instant::now() < deadline,
            "native Smooth track readiness timeout"
        );
        match demuxer.next_event().expect("native Smooth readiness event") {
            DemuxReadEvent::TracksChanged(_) => return,
            DemuxReadEvent::TemporarilyUnavailable(hint) => {
                thread::sleep(hint.retry_after());
            }
            other => panic!("tracks должны быть первой Smooth publication: {other:?}"),
        }
    }
}

/// N14A: Smooth VOD initial row достигает render/readback и PCM/clock без switch/reopen.
#[cfg(feature = "ffmpeg")]
#[test]
fn n14a_consumer_smooth_vod_reaches_consumers_with_exact_accounting() {
    let server = ControlledHlsServer::start(fixture_routes());
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let mut settings = native_settings();
    process_spy.install_as_attempt_owner(&mut settings);
    let source = NativeSmoothUrl::new(
        server.target("/vod/Manifest"),
        SafeMediaLabel::from_service_safe_label("N14A native Smooth VOD"),
    );
    assert_eq!(server.request_count("/vod/Manifest"), 0);
    assert_eq!(server.response_body_bytes("/vod/Manifest"), 0);

    let mut prepared = prepare_native(&source, None, &settings);
    assert_eq!(server.request_count("/vod/Manifest"), 1);
    assert_eq!(
        server.response_body_bytes("/vod/Manifest"),
        SMOOTH_MANIFEST.len()
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

/// Доказывает single root fetch, render/audio, seek, switch/reopen и process spy 0.
#[cfg(feature = "ffmpeg")]
#[test]
fn n14b_lifecycle_smooth_vod_seek_forward_back_switch_and_reopen_reaches_consumers() {
    let server = ControlledHlsServer::start(fixture_routes());
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let mut settings = native_settings();
    process_spy.install_as_attempt_owner(&mut settings);
    let source = NativeSmoothUrl::new(
        server.target("/vod/Manifest"),
        SafeMediaLabel::from_service_safe_label("controlled native Smooth Manifest"),
    );
    assert_eq!(
        server.request_count("/vod/Manifest"),
        0,
        "syntactic Smooth classifier не должен fetch-ить root до open"
    );
    let stable_source_identity = source.source_identity();
    let mut wgpu_harness = OffscreenWgpuHarness::new();

    let mut initial = prepare_native(&source, None, &settings);
    assert_eq!(server.request_count("/vod/Manifest"), 1);
    wait_for_tracks_changed(initial.demuxer.as_mut());
    assert_vod_seek(initial.seek_port.as_ref(), 11, SMOOTH_SEEK_POSITION);
    assert_decoder_render_audio_for_codec(
        initial.demuxer.as_mut(),
        &mut wgpu_harness,
        DecodeVideoCodec::H264,
    );
    let mut backward_seek_attempt = prepare_native(&source, None, &settings);
    assert_eq!(server.request_count("/vod/Manifest"), 2);
    wait_for_tracks_changed(backward_seek_attempt.demuxer.as_mut());
    assert_vod_seek(
        backward_seek_attempt.seek_port.as_ref(),
        12,
        SMOOTH_SEEK_POSITION,
    );
    assert_vod_seek(backward_seek_attempt.seek_port.as_ref(), 13, Duration::ZERO);
    assert_decoder_render_audio_for_codec(
        backward_seek_attempt.demuxer.as_mut(),
        &mut wgpu_harness,
        DecodeVideoCodec::H264,
    );
    drop(backward_seek_attempt);
    let alternate_selection = alternate_audio_selection(&initial.source_state);
    let expected_alternate = alternate_selection.clone();
    let initial_intent = WebMediaSourceIntent::native_smooth(source.clone(), initial.source_state);
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
        panic!("fresh Smooth row action обязана запустить same-item switch");
    };
    let (switch_source, switch_selection, switch_settings) = native_request_parts(switch_request);
    let mut switched = prepare_native(&switch_source, Some(&switch_selection), &switch_settings);
    assert_eq!(server.request_count("/vod/Manifest"), 3);
    wait_for_tracks_changed(switched.demuxer.as_mut());
    assert_decoder_render_audio_for_codec(
        switched.demuxer.as_mut(),
        &mut wgpu_harness,
        DecodeVideoCodec::H264,
    );
    let web_media_core::WebMediaSelectionShape::Components(switched_components) =
        switched.source_state.neutral_selection().shape()
    else {
        panic!("switched Smooth selection должна сохранить component shape");
    };
    assert_eq!(
        switched_components.semantic_rematch_request(),
        expected_alternate,
        "fresh catalog не должен менять semantic video/audio selection"
    );

    let switched_intent =
        WebMediaSourceIntent::native_smooth(switch_source.clone(), switched.source_state);
    let reopen_request = switched_intent
        .controlled_reopen_request(
            switch_settings.network_config.clone(),
            switch_settings.demux_config,
            Some(switch_settings.clone()),
        )
        .expect("native Smooth controlled reopen требует semantic rematch");
    let (reopen_source, reopen_selection, reopen_settings) = native_request_parts(reopen_request);
    let mut reopened = prepare_native(&reopen_source, Some(&reopen_selection), &reopen_settings);
    assert_eq!(server.request_count("/vod/Manifest"), 4);
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
        "valid Smooth open, seek, switch и reopen не запускают extractor"
    );
}

/// Syntactic hint и Debug/persistence projections не раскрывают secret query.
#[test]
fn native_smooth_intent_debug_is_secret_safe() {
    let locator = service_ytdlp::parse_yt_dlp_media_locator(
        "https://media.example.test/vod/Manifest?token=do-not-log",
    )
    .expect("fallback locator");
    let source = NativeSmoothUrl::new(
        source_core::HttpRequestTarget::parse_exact(locator.expose_secret_for_open())
            .expect("native target"),
        SafeMediaLabel::from_service_safe_label(locator.safe_label()),
    );
    let intent = NativeSmoothOpenIntent::InitialWithYtDlpFallback {
        fallback_locator: locator.clone(),
    };

    let debug = format!("{source:?} {intent:?}");
    assert!(!debug.contains("do-not-log"));
    assert!(!debug.contains("token="));
    assert!(debug.contains("<redacted>"));
}

/// Находит typed Smooth admission category сквозь safe anyhow context chain.
fn smooth_failure_kind(
    error: &anyhow::Error,
) -> Option<web_media_smooth::SmoothPrepareFailureKind> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<web_media_smooth::SmoothPrepareError>()
            .map(web_media_smooth::SmoothPrepareError::failure_kind)
    })
}

/// Строит parser-failure corpus из authoritative canonical manifest-а.
fn failure_routes() -> HashMap<String, Vec<Vec<u8>>> {
    let canonical = String::from_utf8(SMOOTH_MANIFEST.to_vec()).expect("canonical UTF-8 manifest");
    let live = canonical.replacen(
        "<SmoothStreamingMedia",
        "<SmoothStreamingMedia IsLive=\"TRUE\"",
        1,
    );
    let private = canonical.replacen(
        "</SmoothStreamingMedia>",
        "<x:Vendor xmlns:x=\"urn:private\"/></SmoothStreamingMedia>",
        1,
    );
    let unsupported_codec = canonical.replacen("FourCC=\"AVC1\"", "FourCC=\"WVC1\"", 1);
    HashMap::from([
        (
            "/foreign/Manifest".to_owned(),
            vec![b"<html><body>provider page</body></html>".to_vec()],
        ),
        ("/live/Manifest".to_owned(), vec![live.into_bytes()]),
        (
            "/drm/Manifest".to_owned(),
            vec![include_bytes!(
                "../../../../../smooth-streaming-manifest-core/tests/fixtures/drm_playready.ismc"
            )
            .to_vec()],
        ),
        ("/private/Manifest".to_owned(), vec![private.into_bytes()]),
        (
            "/codec/Manifest".to_owned(),
            vec![unsupported_codec.into_bytes()],
        ),
        (
            "/malformed/Manifest".to_owned(),
            vec![b"<SmoothStreamingMedia".to_vec()],
        ),
    ])
}

/// Live/DRM/private/codec remain distinct; malformed/network/cancel never fallback.
#[test]
fn native_smooth_keeps_profile_and_terminal_failures_distinct_without_extractor() {
    let server = ControlledHlsServer::start(failure_routes());
    let process_spy = Arc::new(ZeroProcessSpy::default());
    let mut settings = native_settings();
    process_spy.install_as_attempt_owner(&mut settings);
    let source = |path: &str| {
        NativeSmoothUrl::new(
            server.target(path),
            SafeMediaLabel::from_service_safe_label("controlled Smooth failure"),
        )
    };
    let prepare = |source: &NativeSmoothUrl, cancellation: CancellationToken| {
        prepare_native_smooth_attempt(NativeSmoothPreparationRequest {
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
        prepare(&source("/foreign/Manifest"), CancellationToken::new())
            .expect("foreign root должен стать typed initial fallback"),
        NativeSmoothAttempt::RequiresExtractorFallback(
            web_media_core::WebMediaFallbackTrigger::ProviderDocument
        )
    ));
    for (path, expected_kind) in [
        (
            "/live/Manifest",
            web_media_smooth::SmoothPrepareFailureKind::LiveProfile,
        ),
        (
            "/drm/Manifest",
            web_media_smooth::SmoothPrepareFailureKind::DrmProtected,
        ),
        (
            "/private/Manifest",
            web_media_smooth::SmoothPrepareFailureKind::PrivateExtension,
        ),
        (
            "/codec/Manifest",
            web_media_smooth::SmoothPrepareFailureKind::UnsupportedCodecProfile,
        ),
        (
            "/malformed/Manifest",
            web_media_smooth::SmoothPrepareFailureKind::MalformedManifest,
        ),
    ] {
        let error = prepare(&source(path), CancellationToken::new())
            .err()
            .expect("profile/malformed failure не должна становиться fallback");
        assert_eq!(smooth_failure_kind(&error), Some(expected_kind), "{path}");
    }

    let network_error = prepare(&source("/missing/Manifest"), CancellationToken::new())
        .err()
        .expect("404 root fetch обязан остаться terminal network failure");
    assert!(smooth_failure_kind(&network_error).is_none());
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let cancellation_error = prepare(&source("/vod/Manifest"), cancelled)
        .err()
        .expect("pre-cancel не должен попасть в fallback");
    assert!(cancellation_error.to_string().contains("cancelled"));
    assert_eq!(process_spy.invocation_count(), 0);
}
