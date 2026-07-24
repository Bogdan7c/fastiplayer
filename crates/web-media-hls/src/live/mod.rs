//! S33 live runtime policy и segment-scoped temporal evidence.
//!
//! Модуль не знает `yt-dlp`, player или UI. App передаёт только уже
//! rematched transport generation через отдельную request/reply границу.

mod av;
mod demux;
mod open;
mod refresh;
mod snapshot;
mod timeline;

pub(crate) use av::TransactionalHlsLiveAvDemuxer;
pub(crate) use demux::{HlsLiveComponentDemuxer, HlsLiveComponentFactory};
pub use open::{HlsLiveOpenError, HlsLiveOpenResult, prepare_hls_live};
#[allow(unused_imports)]
pub(crate) use snapshot::{
    HlsLiveComponentKind, HlsLiveComponentSnapshot, HlsLiveRefreshError, HlsLiveSegmentIdentity,
    HlsLiveTimelineEvidence,
};
pub(crate) use timeline::HlsLiveTimelineCoordinator;
pub(crate) use timeline::HlsLiveTransportSnapshot;
