//! S33 live runtime policy и segment-scoped temporal evidence.
//!
//! Модуль не знает `yt-dlp`, player или UI. App передаёт только уже
//! rematched transport generation через отдельную request/reply границу.

mod av;
mod demux;
mod hold_back;
mod network_outage;
mod open;
mod refresh;
mod rejoin;
mod reload_schedule;
mod segment_observer;
mod snapshot;
mod timeline;
mod timeline_publication;

pub(crate) use av::TransactionalHlsLiveAvDemuxer;
pub(crate) use demux::{HlsLiveComponentDemuxer, HlsLiveComponentFactory};
pub use open::{
    HlsLiveOpenError, HlsLiveOpenResult, prepare_hls_catalog_live_receipted, prepare_hls_live,
    prepare_hls_live_receipted,
};
#[allow(unused_imports)]
pub(crate) use snapshot::{
    HlsLiveComponentKind, HlsLiveComponentSnapshot, HlsLiveRandomAccessSource, HlsLiveRefreshError,
    HlsLiveSegmentIdentity, HlsLiveTimelineEvidence, HlsLiveVideoDecodeStartEvidence,
};
pub(crate) use timeline::HlsLiveTransportSnapshot;
pub(crate) use timeline::{HlsLiveRejoinError, HlsLiveTimelineCoordinator};
