//! Unit-тесты корня `web_media_open`: матрица extractor reason, capability
//! snapshot-ы transport/demux и allocator-ы generation (вынесены из корня,
//! чтобы production-модуль оставался в пределах лимита размера).

use super::*;

/// HTTP sniff chunk не должен становиться случайным лимитом encoded video packet-а.
#[test]
fn progressive_composite_packet_limit_is_independent_from_sniff_chunk() {
    let lead_policy =
        progressive_composite_lead_policy().expect("progressive composite policy валидна");

    assert_eq!(
        lead_policy.bootstrap_byte_limit(),
        COMPOSITE_MAX_PENDING_PACKET_BYTES
    );
    assert!(lead_policy.bootstrap_byte_limit() > 64 * 1024);
}
use demux_api::{
    DemuxContainerId, DemuxContainerRegistration, DemuxFactoryDescriptor, DemuxFactoryId,
    DemuxFixtureId, DemuxInputCapabilities, DemuxInputCapability,
};
use symphonia_demux::DemuxerOptions;
use web_media_core::{FtpScheme, TransportFamily};

#[test]
fn media_open_reason_matrix_rejects_topology_and_preserves_page_recovery_reason() {
    for reason in [
        web_media_core::ExtractorInvocationReason::PageMediaResolution,
        web_media_core::ExtractorInvocationReason::ExtractorOwnedAuthorizationMaterial,
        web_media_core::ExtractorInvocationReason::NativeProfileCompatibilityFallback,
        web_media_core::ExtractorInvocationReason::ExtractorBackedRecovery,
    ] {
        assert!(validate_extractor_reason(&YtDlpCandidateOpenIntent::BestPlayable, reason).is_ok());
    }
    assert!(
        validate_extractor_reason(
            &YtDlpCandidateOpenIntent::BestPlayable,
            web_media_core::ExtractorInvocationReason::CollectionTopologyResolution,
        )
        .is_err(),
        "collection/topology reason не должен достигать media-open extractor path"
    );
}

/// Shutdown cancellation завершается до запуска extractor/network side effects.
#[test]
fn cancellation_is_a_pre_barrier_failure() {
    let locator =
        service_ytdlp::parse_yt_dlp_media_locator("https://media.example.test/watch?id=secret")
            .expect("valid test locator");
    let result = prepare_yt_dlp_web_media(
        &locator,
        &NetworkConfig::default(),
        &WebMediaConfig::default(),
        &YtDlpConfig::default(),
        &service_ytdlp::YtDlpExtractorAdapter::default(),
        &PlayerDemuxConfig::default(),
        &[ConfigVideoCodec::Vp9],
        &capability_core::SystemCapabilities::empty(1),
        audio::AudioDecodeCapabilitySnapshot::empty(),
        YtDlpCandidateOpenIntent::BestPlayable,
        web_media_core::ExtractorInvocationReason::PageMediaResolution,
        CancellationToken::new(),
        || true,
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("cancelled preparation не должна запускать yt-dlp"),
    };
    let diagnostic = format!("{error:#}");
    assert!(diagnostic.contains("отменена"));
    assert!(!diagnostic.contains("secret"));
}

/// Concrete Symphonia descriptor и planner snapshot не расходятся по S22 containers.
#[test]
fn demux_capability_snapshot_is_derived_from_registered_factory() {
    use demux_api::DemuxFactory;

    let factory = symphonia_demux::SymphoniaDemuxFactory::new(DemuxerOptions::default())
        .expect("Symphonia factory");
    let capabilities =
        crate::web_media_demux_registry::capabilities_for_descriptors([factory.descriptor()])
            .expect("capability snapshot");
    let expected_inputs = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
        .with(DemuxInputCapability::StreamingBytes);
    for family in [
        ContainerFamily::IsoBmff,
        ContainerFamily::FragmentedIsoBmff,
        ContainerFamily::Matroska,
        ContainerFamily::WebM,
        ContainerFamily::Ogg,
        ContainerFamily::Flac,
        ContainerFamily::Wav,
        ContainerFamily::Aiff,
        ContainerFamily::Caf,
        ContainerFamily::MpegAudio,
    ] {
        let expected_family_inputs = if matches!(
            family,
            ContainerFamily::IsoBmff
                | ContainerFamily::FragmentedIsoBmff
                | ContainerFamily::Matroska
                | ContainerFamily::WebM
        ) {
            expected_inputs.with(DemuxInputCapability::OrderedSegments)
        } else {
            expected_inputs
        };
        assert_eq!(
            capabilities.input_capabilities_for(family),
            expected_family_inputs,
            "family {family:?} должна получить exact registered inputs"
        );
    }
}

/// Planner projection не переносит capability между соседними container rows.
#[test]
fn demux_capability_snapshot_preserves_per_container_input_sets() {
    let iso_inputs = DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments);
    let webm_inputs = DemuxInputCapabilities::only(DemuxInputCapability::StreamingBytes);
    let descriptor = DemuxFactoryDescriptor::new(
        DemuxFactoryId::new("synthetic-per-container").expect("factory ID"),
        vec![
            DemuxContainerRegistration::new(
                DemuxContainerId::new("iso-bmff").expect("ISO BMFF container ID"),
                iso_inputs,
                vec![],
                vec![],
            ),
            DemuxContainerRegistration::new(
                DemuxContainerId::new("webm").expect("WebM container ID"),
                webm_inputs,
                vec![],
                vec![],
            ),
        ],
        vec![DemuxFixtureId::new("synthetic/per-container").expect("fixture ID")],
    );

    let capabilities = crate::web_media_demux_registry::capabilities_for_descriptors([&descriptor])
        .expect("capability snapshot");
    assert_eq!(
        capabilities.input_capabilities_for(ContainerFamily::IsoBmff),
        iso_inputs
    );
    assert_eq!(
        capabilities.input_capabilities_for(ContainerFamily::FragmentedIsoBmff),
        iso_inputs
    );
    assert_eq!(
        capabilities.input_capabilities_for(ContainerFamily::WebM),
        webm_inputs
    );
    assert!(
        !capabilities
            .input_capabilities_for(ContainerFamily::WebM)
            .contains(DemuxInputCapability::OrderedSegments),
        "WebM не должен наследовать synthetic ISO ordered capability"
    );
}

/// Component catalog generation монотонна и fail-closed при исчерпании.
#[test]
fn component_variant_catalog_generation_is_monotonic_and_overflow_checked() {
    let allocator = AtomicU64::new(41);

    assert_eq!(
        preparation::allocate_component_variant_catalog_generation(&allocator)
            .expect("first catalog generation")
            .value(),
        41
    );
    assert_eq!(
        preparation::allocate_component_variant_catalog_generation(&allocator)
            .expect("second catalog generation")
            .value(),
        42
    );

    allocator.store(u64::MAX, Ordering::Relaxed);
    assert!(
        preparation::allocate_component_variant_catalog_generation(&allocator).is_err(),
        "исчерпанный allocator не должен оборачивать catalog generation"
    );
}

/// Planner видит exact adaptive input shapes только после concrete runtimes.
#[test]
fn transport_capability_snapshot_advertises_dash_ordered_and_range_inputs() {
    let capabilities =
        progressive_transport_capabilities().expect("transport capability snapshot builds");
    let dash_inputs = capabilities.output_inputs_for(TransportFamily::Dash);

    assert_eq!(
        dash_inputs,
        DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments)
            .with(DemuxInputCapability::SeekableBytes)
    );
    assert_eq!(
        capabilities.output_inputs_for(TransportFamily::Hls),
        DemuxInputCapabilities::only(crate::web_media_hls_open::hls_transport_input()),
        "DASH registration не должна менять соседний HLS provider"
    );
    assert_eq!(
        capabilities.output_inputs_for(TransportFamily::SmoothStreaming),
        DemuxInputCapabilities::only(DemuxInputCapability::OrderedSegments),
        "S36 Smooth runtime должен рекламировать только реально используемый ordered input"
    );
    let progressive_outputs = DemuxInputCapabilities::only(DemuxInputCapability::SeekableBytes)
        .with(DemuxInputCapability::StreamingBytes);
    assert_eq!(
        capabilities.output_inputs_for(TransportFamily::ProgressiveFtp(FtpScheme::Ftp)),
        progressive_outputs,
        "S37 FTP runtime должен рекламировать seekable и streaming byte inputs"
    );
    assert_eq!(
        capabilities.output_inputs_for(TransportFamily::ProgressiveFtp(FtpScheme::Ftps)),
        progressive_outputs,
        "S37 FTPS runtime должен рекламировать seekable и streaming byte inputs"
    );
}
