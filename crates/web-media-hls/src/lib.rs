//! HLS VOD runtime owner поверх shared adaptive transport и existing demux backends.

#![forbid(unsafe_code)]

mod crypto;
mod epoch_demux;
mod key_state;
mod live;
mod manifest_profile;
mod open;
mod plan;
mod request;
mod seek;
mod selection;
mod source;
mod transactional_av;

pub use crypto::{Aes128CbcDecryptError, DecryptedBytes, decrypt_aes128_cbc_pkcs7};
pub use key_state::{
    ActiveAes128Key, Aes128InitializationVector, Aes128KeySource, ExtractorAesOverride,
    ExtractorAesOverrideError, ExtractorKeyUri, HlsKeyState, HlsKeyStateError, SecretAes128Key,
};
pub use live::{HlsLiveOpenError, HlsLiveOpenResult, prepare_hls_live};
pub use manifest_profile::ValidatedVodMediaPlaylist;
pub use open::{HlsVodOpenError, HlsVodOpenResult, prepare_hls_vod};
pub use plan::HlsPlanError as HlsVodPlanError;
pub use request::{
    HlsComponentContainerIntent, HlsContainerEvidence, HlsEndpointRefreshError,
    HlsEndpointRefreshPort, HlsEndpointRefreshReason, HlsEndpointRefreshReply,
    HlsEndpointRefreshRequest, HlsLiveOpenRequest, HlsManifestInput, HlsRequestOverrides,
    HlsRequiredContainer, HlsVodOpenPolicy, HlsVodOpenRequest, SecretInlineMediaPlaylist,
};
pub use selection::{
    HlsAudioLayoutIntent, HlsAudioRenditionEvidence, HlsMainTrackLayoutIntent,
    HlsSubtitleRenditionDescriptor, HlsVariantSelectionIntent,
};

#[cfg(test)]
mod tests;
