mod byte_source;
pub mod demuxer;
pub mod dual_stream_demuxer;
pub mod error;
mod matroska_metadata;
mod options;
pub mod streaming_source;
pub mod symphonia_demuxer;

pub use demuxer::{DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer};
pub use dual_stream_demuxer::DualStreamDemuxer;
pub use error::DemuxError;
pub use media_core::{Packet, TimeBase, TrackId, TrackInfo, TrackKind};
pub use options::{DEFAULT_MAX_CONSECUTIVE_CORRUPTED_PACKETS, DemuxerOptions};
pub use streaming_source::{StreamingByteReader, StreamingByteWriter};
pub use symphonia_demuxer::SymphoniaDemuxer;
