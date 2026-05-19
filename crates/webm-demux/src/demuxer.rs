//! Compatibility module для старого пути `webm_demux::demuxer::*`.
//!
//! Neutral demux contract теперь живёт в `media-core`, а `webm-demux`
//! остаётся concrete контейнерной реализацией поверх этого contract-а.

pub use media_core::{
    DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer, MediaDemuxError,
};
