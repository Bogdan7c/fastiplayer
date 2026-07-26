//! Общие test-only budgets и identity fixtures.

use std::num::{NonZeroU8, NonZeroUsize};
use std::time::Duration;

use bounded_xml_reader::XmlBudgets;
use smooth_streaming_manifest_core::{
    SmoothManifest, SmoothManifestLimits, SmoothManifestParseRequest, parse_vod_client_manifest,
};
use symphonia_format_isomp4::FragmentInitializationLimits;
use web_media_adaptive::{AdaptiveRetryPolicy, AdaptiveTransportLimits};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogIdentity, ComponentVariantCatalogLimit, ComponentVariantEdgeLimit,
    ExactSelectionIdentity, ExtractionGeneration, SemanticIdentity, SourceIdentity,
};

use crate::{AggregateInitializationByteLimit, SmoothPreparationPolicy};

pub(crate) const VALID_MANIFEST: &str =
    include_str!("../../smooth-streaming-manifest-core/tests/fixtures/valid_h264_aac_v20.ismc");
pub(crate) const DIFFERING_CLOCKS_MANIFEST: &str = include_str!(
    "../../smooth-streaming-manifest-core/tests/fixtures/differing_av_timescales_alignment.ismc"
);
pub(crate) const CANONICAL_PIFF_MANIFEST: &str =
    include_str!("../../symphonia-format-isomp4-patch/fixtures/smooth-piff/tears-of-steel.ismc");

pub(crate) fn parse(document: &str) -> SmoothManifest {
    parse_vod_client_manifest(SmoothManifestParseRequest {
        document_bytes: document.as_bytes(),
        xml_budgets: xml_budgets(),
        limits: manifest_limits(),
    })
    .expect("test Smooth manifest должен быть valid")
}

pub(crate) fn policy(aggregate_bytes: usize) -> SmoothPreparationPolicy {
    SmoothPreparationPolicy::new(
        AdaptiveTransportLimits::new(
            NonZeroUsize::new(64 * 1024).expect("manifest budget"),
            NonZeroUsize::new(64 * 1024).expect("segment budget"),
            NonZeroUsize::new(64).expect("snapshot budget"),
        ),
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(1).expect("retry attempts"),
            Duration::from_millis(1),
            Duration::from_millis(2),
        )
        .expect("retry policy"),
        xml_budgets(),
        manifest_limits(),
        FragmentInitializationLimits::builder()
            .maximum_output_bytes(16 * 1024)
            .maximum_codec_configuration_bytes(4 * 1024)
            .build()
            .expect("init budgets"),
        AggregateInitializationByteLimit::new(
            NonZeroUsize::new(aggregate_bytes).expect("aggregate budget"),
        ),
        ComponentVariantCatalogLimit::new(64).expect("catalog budget"),
        ComponentVariantEdgeLimit::new(1_024).expect("compatibility edge budget"),
    )
}

pub(crate) fn catalog_identity() -> (ComponentVariantCatalogIdentity, SemanticIdentity) {
    let source = SourceIdentity::new(37);
    let exact = CandidateIdentity::new(
        source,
        ExtractionGeneration::new(5),
        CandidateFormatIdentity::new("smooth-test").expect("format identity"),
    );
    let semantic = SemanticIdentity::new(source, "smooth-test").expect("semantic identity");
    let parent = ExactSelectionIdentity::new(exact, semantic.clone()).expect("same source lineage");
    (
        ComponentVariantCatalogIdentity::new(parent, ComponentVariantCatalogGeneration::new(11)),
        semantic,
    )
}

pub(crate) fn xml_budgets() -> XmlBudgets {
    XmlBudgets::builder()
        .maximum_document_bytes(64 * 1024)
        .maximum_depth(16)
        .maximum_tokens(2_048)
        .maximum_attributes_per_element(32)
        .maximum_attribute_count(1_024)
        .maximum_attribute_bytes(48 * 1024)
        .maximum_namespace_declarations_per_element(8)
        .maximum_namespace_declaration_count(32)
        .maximum_namespace_bytes(4 * 1024)
        .maximum_text_bytes(32 * 1024)
        .build()
        .expect("XML budgets")
}

pub(crate) fn manifest_limits() -> SmoothManifestLimits {
    SmoothManifestLimits::builder()
        .maximum_streams(8)
        .maximum_qualities_per_stream(16)
        .maximum_total_qualities(32)
        .maximum_timeline_entries_per_stream(256)
        .maximum_total_timeline_entries(512)
        .maximum_fragments_per_stream(1_024)
        .maximum_total_fragments(2_048)
        .maximum_template_bytes(512)
        .maximum_string_bytes(256)
        .maximum_codec_bytes(4_096)
        .maximum_custom_attributes_per_quality(8)
        .maximum_total_custom_attributes(32)
        .maximum_custom_attribute_name_bytes(64)
        .maximum_custom_attribute_value_bytes(128)
        .build()
        .expect("manifest budgets")
}
