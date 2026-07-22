//! Bounded first-party MPEG-TS demuxer для reusable local/stream/segment путей.
//!
//! Crate владеет только MPEG-TS framing, PSI/PES/timestamp lifecycle и не знает
//! ни HLS manifest-ов, ни сети, ни player/UI. Codec semantics H.264/H.265
//! делегируются `codec-core`.

#![forbid(unsafe_code)]

mod demuxer;
mod elementary;
mod error;
mod factory;
mod framing;
mod options;
mod pes;
mod psi;
mod timestamps;
mod video_assembler;

pub use demuxer::MpegTsDemuxer;
pub use error::{MpegTsDemuxError, MpegTsOptionsError};
pub use factory::MpegTsDemuxFactory;
pub use options::{MpegTsDemuxOptions, MpegTsLimit};

#[cfg(test)]
mod tests;
