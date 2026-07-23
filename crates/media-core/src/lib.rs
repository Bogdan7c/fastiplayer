//! Общие media-типы без привязки к контейнеру, декодеру или UI.
//!
//! `media-core` задаёт контракт между demuxer-ами, audio/video pipeline и
//! будущим `player-core`. Контейнерные crate'ы могут только заполнять эти
//! структуры, но не должны владеть их определениями.

#![forbid(unsafe_code)]

mod demux;
mod dynamic_timeline;
mod metadata;
mod packet;
mod time;
mod track;

pub use demux::{
    DemuxReadEvent, DemuxRetryHint, DemuxRetryHintError, DemuxSeekMode, DemuxSeekRequest,
    DemuxSeekResult, DemuxSeekability, DemuxTrackListUpdate, Demuxer, MediaDemuxError,
    finite_packet_read_event,
};
pub use dynamic_timeline::{
    DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial, DynamicMediaTimelineObservation,
    DynamicMediaTimelinePort, DynamicMediaTimelinePortGeneration, DynamicMediaTimelinePublishError,
    DynamicMediaTimelinePublishOutcome, DynamicMediaTimelinePublisher,
    DynamicMediaTimelineRevision, DynamicMediaTimelineSnapshot, DynamicMediaTimelineState,
    DynamicMediaTimelineValidationError, dynamic_media_timeline,
};
pub use metadata::{
    DiscNumber, MediaContainerMetadata, MediaMetadata, MediaTagMetadata, TrackNumber,
    TvEpisodeNumber, TvSeasonNumber,
};
pub use packet::{Packet, PacketKeyframe};
pub use time::{
    MediaDuration, MediaTime, TimeBase, TimelineMode, TimelineNotSeekableReason,
    TimelinePreviewState, TimelineRange, TimelineSnapshot, TrackDuration, TrackDurationUnits,
    TrackTimestamp, TrackTimestampUnits,
};
pub use track::{TrackId, TrackInfo, TrackKind, VideoPacketFraming, VideoTrackMetadata};
