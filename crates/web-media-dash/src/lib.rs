//! Static DASH VOD runtime поверх S04X/S31/S28 neutral boundaries.
//!
//! Crate владеет MPD/serialized planning, exact Representation selection,
//! multi-period lifecycle и transactional seek. HTTP wire mechanics остаются в
//! `source-core`, request policy — в `web-media-adaptive`, container parsing —
//! в injected existing demux factories.

#![forbid(unsafe_code)]

mod component;
mod open;
mod plan;
mod request;
mod selection;
mod source;
#[cfg(test)]
mod tests;
mod transactional_av;

pub use open::{DashVodOpenError, DashVodOpenResult, prepare_dash_vod};
pub use plan::DashPlanError;
pub use request::{
    DashManifestInput, DashResourceReference, DashSerializedComponent, DashSerializedFragment,
    DashSerializedFragmentKind, DashSerializedPresentation, DashVodHttpContext, DashVodInput,
    DashVodOpenPolicy, DashVodOpenRequest,
};
pub use selection::{
    DashPresentationSelection, DashRepresentationEvidence, DashRepresentationSelectionError,
    DashVideoDimensions,
};
