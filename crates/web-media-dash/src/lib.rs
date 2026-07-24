//! Строгий static VOD и dynamic live/DVR DASH runtime поверх neutral boundaries.
//!
//! Crate владеет MPD/serialized planning, exact Representation selection,
//! multi-period lifecycle, transactional seek и S35 timing/refresh policy.
//! HTTP wire mechanics остаются в `source-core`, request policy — в
//! `web-media-adaptive`, container parsing — в injected existing demux factories.

#![forbid(unsafe_code)]

mod component;
// Dynamic timing/refresh policy не раздувает finite S34 modules.
mod live;
mod open;
mod plan;
mod request;
mod selection;
mod source;
#[cfg(test)]
mod tests;
mod transactional_av;

pub use live::{
    DashEndpointRefreshError, DashEndpointRefreshPort, DashEndpointRefreshReply,
    DashEndpointRefreshRequest, DashLiveAvailability, DashLiveClockError, DashLiveOpenError,
    DashLiveOpenRequest, DashLiveOpenResult, DashLiveProfileExclusion, DashLiveRefreshError,
    DashLiveRefreshOutcome, DashLiveSnapshot, DashSynchronizedClock, DashWallClock,
    build_dash_live_snapshot, prepare_dash_live, refresh_dash_live_snapshot,
};
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
