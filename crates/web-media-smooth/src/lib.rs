//! Строгая подготовка Smooth Streaming VOD поверх нейтральных transport-контрактов.
//!
//! Fast preparation выполняет один manifest fetch и materializes только
//! provider-default A/V seed. Отдельный bounded discovery доказывает siblings
//! через transport/content/demux/track/capability evidence и атомарно публикует
//! additive C3 `AllPairs` catalog с private exact/semantic reopen mapping.
//! Selected runtime по-прежнему строит lazy fragment sources, injected stable
//! A/V demux и transactional receipted VOD seek. Concrete ISO backend, app
//! composition и player lifecycle остаются у downstream owners.

#![forbid(unsafe_code)]

mod catalog;
mod demux;
mod discovery;
mod error;
mod model;
mod policy;
mod prepare;
mod request;
mod source;
#[cfg(test)]
mod test_support;

pub use demux::{
    SmoothAudioDemuxOpenParts, SmoothAudioDemuxOpenRequest, SmoothIsoBmffDemuxFactory,
    SmoothVideoDemuxOpenParts, SmoothVideoDemuxOpenRequest, SmoothVodDemuxBuildError,
    SmoothVodDemuxPolicy, SmoothVodOpenResult, SmoothVodSeekError,
};
pub use discovery::{
    SmoothCatalogDiscoveryError, SmoothCatalogDiscoveryPolicy, SmoothCatalogDiscoveryRequest,
    SmoothCatalogReopenError, SmoothComponentCapabilityProbe, SmoothComponentCapabilityRejection,
    SmoothDiscoveredCatalog, discover_smooth_vod_catalog,
};
pub use error::{
    SmoothPrepareError, SmoothPrepareFailureKind, SmoothProfileError, SmoothSemanticKeyError,
    SmoothSiblingRejection, SmoothSiblingRejectionReason, SmoothTransportProfileError,
};
pub use model::{SmoothAlignedSpan, SmoothPreparedCatalog};
pub use policy::{AggregateInitializationByteLimit, SmoothPreparationPolicy};
pub use prepare::prepare_smooth_vod;
pub use request::{SmoothFetchedManifestInput, SmoothPrepareRequest};
pub use smooth_streaming_manifest_core::SmoothManifestLimits;
pub use source::{
    SmoothAudioFragmentSource, SmoothFragmentSourceBuildError, SmoothFragmentSourceParts,
    SmoothFragmentSourcePolicy, SmoothSelectedFragmentSources, SmoothVideoFragmentSource,
};
pub use symphonia_format_isomp4::{
    FragmentInitializationLimits, FragmentInspectionLimits, FragmentWriteLimits,
};
