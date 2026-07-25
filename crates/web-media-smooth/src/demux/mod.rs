//! P4 composition: selected Smooth sources -> neutral progressive A/V demux.

mod error;
mod factory;
mod policy;
mod runtime;
mod seek;
#[cfg(test)]
mod tests;

pub use error::{SmoothVodDemuxBuildError, SmoothVodSeekError};
pub use factory::{
    SmoothAudioDemuxOpenParts, SmoothAudioDemuxOpenRequest, SmoothIsoBmffDemuxFactory,
    SmoothVideoDemuxOpenParts, SmoothVideoDemuxOpenRequest,
};
pub use policy::SmoothVodDemuxPolicy;
pub use runtime::SmoothVodOpenResult;
