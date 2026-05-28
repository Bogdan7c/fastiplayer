use std::collections::VecDeque;
use std::sync::{
    Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use codec_core::{ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction};

use super::audio_runtime::SeekAudioGateStatus;
use super::seek_transaction::SeekProgressGateSnapshot;
use super::*;
use crate::seek_state::{
    POST_SEEK_PACKET_TRACE_LIMIT, PlaybackResumeIntent, PostSeekPacketTraceDecision,
    SeekCommitState, SeekTraceState,
};
use crate::{
    AudioOutputFactory, AudioOutputSpec, DecodeBackpressureReason, DecodeSendError,
    DecodeThreadError, DecoderControlChannelPressureSnapshot, DecoderResourceSnapshot,
    MediaOpenRequest, MediaSource, MediaSummary, PendingAudioPacket, PendingVideoPacket,
    PipelineQueueDepthSnapshot, PlayerAudioClock, PlayerAudioOutput, PlayerCommand,
    PlayerDecodePacket, PlayerError, PlayerErrorKind, PlayerEvent, PlayerTickConfig,
    PlayerTickContext, PlayerTickResult, PlayerVideoFrameDrop, PreparedMedia,
    PresentFrameResourceProviderHandle, ScrubCommitPolicy, SeekBootstrapDiagnosticsSnapshot,
    SeekMode, SeekProgressBlocker, SeekTarget,
};
use bytes::Bytes;
use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, P010StorageLayout, VideoExportPath,
};
use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, SupportedVideoDecodeFormat, VideoCodec,
    VideoDecodeRequirement, VideoProfile, Vp9Profile, video_requirement_needs_packet_refinement,
};
use media_core::{
    DemuxReadEvent, DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability,
    DemuxTrackListUpdate, Demuxer, PacketKeyframe, TimelineNotSeekableReason, TrackKind,
    VideoTrackMetadata,
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
