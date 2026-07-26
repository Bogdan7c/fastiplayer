//! S38 HDS VOD runtime: F4M hierarchy -> F4F ordered source -> S30 demux.
//!
//! Live/DVR специально не входят в этот crate: для них не создаётся скрытый
//! S31L/S35S path. VOD seek выполняется заменой ordered source на demux worker-е.

#![forbid(unsafe_code)]

mod catalog;
mod policy;
mod resolve;
mod runtime;

pub use catalog::{
    HdsCatalogDiscoveryRequest, HdsRenditionCapabilityProbe, HdsRenditionCapabilityRejection,
    HdsRenditionCatalog, discover_hds_renditions, prepare_discovered_hds_vod,
};
pub use policy::HdsVodOpenPolicy;
pub use resolve::{HdsRenditionRejection, HdsRenditionRejectionReason, HdsRenditionSelection};
pub use runtime::{HdsVodOpenRequest, HdsVodOpenResult, HdsVodPresentationWindow, prepare_hds_vod};
