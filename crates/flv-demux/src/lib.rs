//! Bounded first-party FLV/F4F demuxer для progressive и ordered media paths.
//!
//! Crate владеет container framing, timestamp/config/recovery lifecycle и VOD
//! индексом. HDS/RTMP transport, F4M manifest и player policy сюда не входят.

#![forbid(unsafe_code)]

mod codec;
mod demuxer;
mod error;
mod f4f;
mod factory;
mod framing;
mod input;
mod metadata;
mod options;
mod timestamp;

pub use demuxer::FlvDemuxer;
pub use error::{FlvDemuxError, FlvOptionsError};
pub use factory::FlvDemuxFactory;
pub use options::{FlvDemuxOptions, FlvLimit};

#[cfg(test)]
mod tests;
