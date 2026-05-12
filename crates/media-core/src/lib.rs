//! Общие media-типы без привязки к контейнеру, декодеру или UI.
//!
//! `media-core` задаёт контракт между demuxer-ами, audio/video pipeline и
//! будущим `player-core`. Контейнерные crate'ы могут только заполнять эти
//! структуры, но не должны владеть их определениями.

#![forbid(unsafe_code)]

mod packet;
mod time;
mod track;

pub use packet::Packet;
pub use time::{
    MediaDuration, MediaTime, TimeBase, TimelineNotSeekableReason, TimelineRange, TimelineSnapshot,
    TrackTimestamp,
};
pub use track::{TrackId, TrackInfo, TrackKind, VideoTrackMetadata};
