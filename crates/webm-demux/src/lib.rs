pub mod demuxer;
pub mod dual_stream_demuxer;
pub mod error;
mod matroska_metadata;
pub mod streaming_source;
pub mod symphonia_demuxer;

pub use demuxer::Demuxer;
pub use dual_stream_demuxer::DualStreamDemuxer;
pub use error::DemuxError;
pub use media_core::{Packet, TimeBase, TrackId, TrackInfo, TrackKind};
pub use streaming_source::{StreamingByteReader, StreamingByteWriter};
pub use symphonia_demuxer::SymphoniaDemuxer;
