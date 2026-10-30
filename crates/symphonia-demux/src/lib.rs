mod attachments;
mod byte_source;
mod container_timestamp_shift;
pub mod demuxer;
pub mod dual_stream_demuxer;
pub mod error;
mod factory;
mod isomp4_source_offset;
mod local_probe;
mod matroska_metadata;
mod options;
mod ordered_segments;
mod packet_mapper;
mod presentation_window_ordered;
mod seek_mapper;
mod stream_probe_failure;
pub mod streaming_source;
mod symphonia_api;
pub mod symphonia_demuxer;
mod track_labels;
mod track_mapper;

pub use attachments::{
    AttachmentReadLimits, DEFAULT_MAX_ATTACHMENT_BYTES, DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES,
};
pub use demuxer::{
    DemuxReadEvent, DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability,
    DemuxTrackListUpdate, Demuxer, MediaDemuxError,
};
pub use dual_stream_demuxer::DualStreamDemuxer;
pub use error::DemuxError;
pub use factory::SymphoniaDemuxFactory;
pub use local_probe::{
    ContainerProbeError, ContainerProbeSnapshot, ContainerTrackTopology,
    probe_open_local_media_file,
};
pub use media_core::{MediaMetadata, Packet, TimeBase, TrackId, TrackInfo, TrackKind};
pub use options::{
    DEFAULT_DECODE_POINT_BEFORE_PREROLL, DEFAULT_DECODE_POINT_BEFORE_VERIFICATION_PACKET_LIMIT,
    DEFAULT_LATE_VIDEO_START_PROOF_PACKET_LIMIT, DEFAULT_MAX_CONSECUTIVE_CORRUPTED_PACKETS,
    DemuxerOptions,
};
pub use ordered_segments::OrderedSegmentLifecycleError;
pub use presentation_window_ordered::{
    PresentationWindowOrderedIsoMp4Demuxer, PresentationWindowOrderedIsoMp4Error,
    PresentationWindowOrderedTrackField,
};
pub use streaming_source::{StreamingByteReader, StreamingByteWriter};
pub use symphonia_demuxer::SymphoniaDemuxer;
