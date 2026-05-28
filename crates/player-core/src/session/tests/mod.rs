use std::collections::VecDeque;
use std::sync::{
    Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use codec_core::{ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction};

use super::*;
use crate::{
    AudioOutputFactory, AudioOutputSpec, DecodeBackpressureReason, DecodeSendError,
    DecodeThreadError, DecoderControlChannelPressureSnapshot, DecoderResourceSnapshot,
    MediaOpenRequest, MediaSource, MediaSummary, PendingAudioPacket, PendingVideoPacket,
    PlayerAudioClock, PlayerAudioOutput, PlayerCommand, PlayerDecodePacket, PlayerError,
    PlayerErrorKind, PlayerEvent, PlayerTickConfig, PlayerTickContext, PlayerTickResult,
    PlayerVideoFrameDrop, PreparedMedia, PresentFrameResourceProviderHandle, ScrubCommitPolicy,
    SeekMode, SeekTarget,
};
use bytes::Bytes;
use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, P010StorageLayout, VideoExportPath,
};
use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, SupportedVideoDecodeFormat, VideoProfile,
    Vp9Profile,
};
use media_core::{
    DemuxReadEvent, DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability,
    DemuxTrackListUpdate, Demuxer, PacketKeyframe,
};
use render_core::RenderCapabilities;

mod test_support;

mod audio_runtime;
mod capability_selection;
mod decoder_boundary;
mod diagnostics_sink;
mod eof_drain;
mod media_lifecycle;
mod playback;
mod scrub;
mod seek_regressions;
mod seek_trace;
mod seek_transaction;
