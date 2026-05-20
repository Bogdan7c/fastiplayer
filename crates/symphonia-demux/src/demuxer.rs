//! Compatibility module для старого пути `*_demux::demuxer::*`.
//!
//! Neutral demux contract живёт в `media-core`, а `symphonia-demux`
//! остаётся concrete adapter-ом поверх Symphonia и этого contract-а.

pub use media_core::{
    DemuxReadEvent, DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability,
    DemuxTrackListUpdate, Demuxer, MediaDemuxError,
};
