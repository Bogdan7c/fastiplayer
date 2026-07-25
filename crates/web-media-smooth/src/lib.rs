//! Строгая подготовка Smooth Streaming VOD поверх нейтральных transport-контрактов.
//!
//! Подготовка выполняет ровно один manifest fetch, валидирует sealed S36D profile,
//! отображает каждое объявленное качество через S36F2/F1 и публикует нейтральный
//! C3 `VideoAndAudio` catalog. После exact C3 selection crate строит ленивые
//! finite video/audio fragment sources с bounded reconstruction и exact audio
//! presentation windows, injected stable A/V demux и transactional receipted
//! VOD seek. Concrete ISO backend, app composition и player lifecycle остаются
//! у downstream owners.

#![forbid(unsafe_code)]

mod catalog;
mod demux;
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
pub use error::{
    SmoothPrepareError, SmoothProfileError, SmoothSemanticKeyError, SmoothTransportProfileError,
};
pub use model::{SmoothAlignedSpan, SmoothPreparedCatalog};
pub use policy::{AggregateInitializationByteLimit, SmoothPreparationPolicy};
pub use prepare::prepare_smooth_vod;
pub use request::SmoothPrepareRequest;
pub use smooth_streaming_manifest_core::SmoothManifestLimits;
pub use source::{
    SmoothAudioFragmentSource, SmoothFragmentSourceBuildError, SmoothFragmentSourceParts,
    SmoothFragmentSourcePolicy, SmoothSelectedFragmentSources, SmoothVideoFragmentSource,
};
pub use symphonia_format_isomp4::{
    FragmentInitializationLimits, FragmentInspectionLimits, FragmentWriteLimits,
};
