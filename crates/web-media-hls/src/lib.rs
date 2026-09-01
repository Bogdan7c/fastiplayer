//! HLS VOD runtime owner поверх shared adaptive transport и existing demux backends.

#![forbid(unsafe_code)]

mod active_read;
mod catalog;
mod crypto;
mod diagnostics;
mod epoch_demux;
mod initial_open;
mod initial_position_proof;
mod key_state;
mod live;
mod manifest_profile;
mod native_ingress;
mod open;
mod plan;
mod request;
mod seek;
mod selection;
mod source;
mod start;
mod transactional_av;

pub use catalog::{
    HlsCatalogAlignmentProof, HlsCatalogBuildError, HlsCatalogBuildPolicy, HlsCatalogBuildRequest,
    HlsCatalogCapabilityProofPort, HlsCatalogCapabilityRejection, HlsCatalogChildId,
    HlsCatalogChildProbe, HlsCatalogChildProof, HlsCatalogChildProofError,
    HlsCatalogChildProofPort, HlsCatalogChildRole, HlsCatalogDiscoveryError,
    HlsCatalogDiscoveryOutcome, HlsCatalogDiscoveryRequest, HlsCatalogPresentation,
    HlsCatalogReopenError, HlsCatalogReopenSelection, HlsCatalogSiblingRejection,
    HlsCatalogSiblingRejectionReason, HlsCatalogSnapshot, HlsCatalogTopologySeed,
    HlsCatalogTrackProof, HlsProviderDefaultAudioPolicy, build_hls_catalog,
    detect_hls_catalog_presentation, discover_hls_catalog, seed_hls_catalog_topology,
};
pub use crypto::{Aes128CbcDecryptError, DecryptedBytes, decrypt_aes128_cbc_pkcs7};
pub use initial_position_proof::{
    HlsInitialPositionProof, HlsInitialPositionProofCapability, HlsInitialPositionProofPort,
    HlsInitialPositionProofTakeOutcome,
};
pub use key_state::{
    ActiveAes128Key, Aes128InitializationVector, Aes128KeySource, ExtractorAesOverride,
    ExtractorAesOverrideError, ExtractorKeyUri, HlsKeyState, HlsKeyStateError, SecretAes128Key,
};
pub use live::{
    HlsLiveOpenError, HlsLiveOpenResult, prepare_hls_catalog_live_receipted, prepare_hls_live,
    prepare_hls_live_receipted,
};
pub use manifest_profile::ValidatedVodMediaPlaylist;
pub use native_ingress::{
    NativeHlsAdmissionError, NativeHlsCatalogAdmission, NativeHlsDynamicRangePolicy,
    NativeHlsPresentationEvidence, NativeHlsSelectionPolicy, NativeHlsSelectionPolicyError,
    NativeHlsSemanticSelection, admit_native_hls, admit_native_hls_catalog,
};
pub use open::{
    HlsInitialReadinessCapability, HlsVodOpenError, HlsVodOpenResult,
    prepare_hls_catalog_vod_receipted, prepare_hls_catalog_vod_receipted_at_start, prepare_hls_vod,
    prepare_hls_vod_receipted, prepare_hls_vod_receipted_at_start,
};
pub use plan::HlsPlanError as HlsVodPlanError;
pub use request::{
    HlsComponentContainerIntent, HlsContainerEvidence, HlsEndpointRefreshError,
    HlsEndpointRefreshPort, HlsEndpointRefreshReason, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsFetchedTopManifest, HlsLiveOpenRequest, HlsManifestInput,
    HlsRequestOverrides, HlsRequiredContainer, HlsVodOpenPolicy, HlsVodOpenRequest,
    HlsVodSeekLandingPolicy, SecretInlineMediaPlaylist,
};
pub use selection::{
    HlsAudioLayoutIntent, HlsAudioRenditionEvidence, HlsMainTrackLayoutIntent,
    HlsSubtitleRenditionDescriptor, HlsVariantSelectionIntent,
};
pub use start::{HlsVodRestoreFallbackReason, HlsVodStartDisposition, HlsVodStartIntent};

#[cfg(test)]
mod tests;
