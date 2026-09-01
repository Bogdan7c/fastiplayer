//! S38 HDS VOD runtime: F4M hierarchy -> F4F ordered source -> S30 demux.
//!
//! Live/DVR специально не входят в этот crate: для них не создаётся скрытый
//! S31L/S35S path. VOD seek выполняется заменой ordered source на demux worker-е.

#![forbid(unsafe_code)]

mod catalog;
mod error;
mod policy;
mod request;
mod resolve;
mod runtime;

pub use catalog::{
    HdsCatalogDiscoveryRequest, HdsFetchedCatalogDiscoveryRequest, HdsNoPlayableRendition,
    HdsRenditionCapabilityProbe, HdsRenditionCapabilityRejection, HdsRenditionCatalog,
    discover_fetched_hds_renditions, discover_hds_renditions, prepare_discovered_hds_vod,
};
pub use error::{HdsPrepareFailureKind, classify_hds_prepare_error};
pub use policy::HdsVodOpenPolicy;
pub use request::HdsFetchedManifestInput;
pub use resolve::{HdsRenditionRejection, HdsRenditionRejectionReason, HdsRenditionSelection};
pub use runtime::{HdsVodOpenRequest, HdsVodOpenResult, HdsVodPresentationWindow, prepare_hds_vod};
