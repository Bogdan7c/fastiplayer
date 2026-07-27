//! Нейтральные value-контракты web-media.
//!
//! Crate намеренно не знает extractor, transport provider, HTTP, player, config
//! или UI. Он только проверяет и хранит значения, которыми будущие owners смогут
//! обмениваться без протекания runtime-реализаций через архитектурные границы.

mod compatibility;
mod descriptor;
mod identity;
mod layout;
mod normalized;
mod selection;
mod variant;

pub use compatibility::{
    ProfileExclusionReason, StaticCompatibilityRejection, StaticDescriptorField,
    StaticMetadataViolation,
};
pub use descriptor::{
    AudioTrackDescriptor, Bitrate, BitrateError, CandidateDescriptor, CandidateDescriptorError,
    ChannelCount, ChannelCountError, DescriptorTextField, DynamicRange, FrameRate, FrameRateError,
    LanguageTag, MAX_SUBTITLE_DESCRIPTORS, MAX_VIDEO_WIDTH, SampleRate, SampleRateError,
    SubtitleDescriptor, SubtitleFormatIdentity, VideoTrackDescriptor, VideoWidth, VideoWidthError,
};
pub use identity::{
    CandidateFormatIdentity, CandidateIdentity, ExtractionGeneration, IdentityBuildError,
    IdentityField, MAX_CANDIDATE_FORMAT_IDENTITY_UTF8_BYTES, MAX_SEMANTIC_IDENTITY_UTF8_BYTES,
    SemanticIdentity, SourceIdentity,
};
pub use layout::{
    AudioComponentDescriptor, HlsMuxedCodecDeferredBuildError, HlsMuxedCodecDeferredDescriptor,
    MuxedComponentDescriptor, StreamLayout, StreamLayoutKind, VideoComponentDescriptor,
};
pub use normalized::{
    CodecFamily, CodecKind, CodecMediaKind, ContainerFamily, ContainerHintConflict,
    ContainerIdentity, FtpScheme, HttpScheme, KnownExcludedTransport, MAX_RAW_IDENTITY_UTF8_BYTES,
    NormalizedCodec, NormalizedTransport, RawCodecIdentity, RawContainerIdentity,
    RawExtensionIdentity, RawIdentityBuildError, RawIdentityField, RawTransportIdentity,
    RtmpVariant, TransportFamily,
};
pub use selection::{
    ExactSelectionIdentity, ExactSelectionIdentityError, MAX_VIDEO_HEIGHT, PreferredHeightPolicy,
    PreferredHeightRank, PreferredVideoHeight, SelectionRequest, VideoHeight, VideoHeightError,
};
pub use variant::{
    AudioComponentVariant, ComponentKind, ComponentVariantCatalog, ComponentVariantCatalogEntries,
    ComponentVariantCatalogGeneration, ComponentVariantCatalogIdentity,
    ComponentVariantCatalogLimit, ComponentVariantCatalogLimitError, ComponentVariantCompatibility,
    ComponentVariantCompatibilityEdge, ComponentVariantCompatibilityEntries,
    ComponentVariantEdgeLimit, ComponentVariantEdgeLimitError, ComponentVariantError,
    ComponentVariantExactIdentity, ComponentVariantExactKey, ComponentVariantKeyError,
    ComponentVariantSelection, ComponentVariantSelectionRequest, ComponentVariantSemanticIdentity,
    ComponentVariantSemanticKey, ComponentVariantSemanticSelectionRequest, CoupledComponentVariant,
    CoupledVariantExactIdentity, CoupledVariantSemanticIdentity,
    MAX_COMPONENT_VARIANT_CATALOG_ENTRIES, MAX_COMPONENT_VARIANT_COMPATIBILITY_EDGES,
    MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES, VideoComponentVariant,
};
