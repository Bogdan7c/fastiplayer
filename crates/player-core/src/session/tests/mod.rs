use std::collections::VecDeque;
use std::sync::{
    Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use codec_core::{ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction};

use super::audio_runtime::SeekAudioGateStatus;
use super::seek_commit_gates::SeekProgressGateSnapshot;
use super::*;
use crate::seek_state::{
    AccuratePrerollDemuxEventKind, POST_SEEK_PACKET_TRACE_LIMIT, PlaybackResumeIntent,
    PostSeekPacketTraceDecision, SeekCommitState, SeekTraceState,
};
use crate::{
    AudioOutputFactory, AudioOutputSpec, DecodeBackpressureReason, DecodeSendError,
    DecodeThreadError, DecoderControlChannelPressureSnapshot, DecoderResourceSnapshot,
    MediaOpenRequest, MediaSource, MediaSummary, PendingAudioPacket, PendingVideoPacket,
    PipelineQueueDepthSnapshot, PlayerAudioClock, PlayerAudioOutput, PlayerCommand,
    PlayerDecodePacket, PlayerError, PlayerErrorKind, PlayerEvent, PlayerTickConfig,
    PlayerTickContext, PlayerTickResult, PlayerVideoFrameDrop, PreparedMedia,
    PresentFrameResourceProviderHandle, ScrubCommitOutcome, ScrubCommitPolicy,
    SeekBootstrapDiagnosticsSnapshot, SeekMode, SeekProgressBlocker, SeekTarget,
    VisibleScrubPreviewUnavailableReason,
};
use bytes::Bytes;
use capability_core::{
    BackendCapabilities, BackendDriverInfo, BackendProbeStatus, SupportedVideoOutput,
};
use codec_core::{
    BitDepth, ChromaSubsampling, DecodeBackendId, H264Profile, SupportedVideoDecodeFormat,
    VideoCodec, VideoDecodeRequirement, VideoProfile, Vp9Profile,
    video_requirement_needs_packet_refinement,
};
use media_core::{
    DemuxReadEvent, DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability,
    DemuxTrackListUpdate, Demuxer, PacketKeyframe, TimelineNotSeekableReason, TrackKind,
    VideoTrackMetadata,
};
use render_core::RenderCapabilities;
use video_core::{HostUploadResourceSnapshot, HostUploadResourceSnapshotStatus};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

mod test_support;

mod audio_runtime;
mod capability_selection;
mod decoder_boundary;
mod diagnostics_sink;
mod eof_drain;
mod exact_media_transport;
mod installed_media_restore;
mod media_lifecycle;
mod playback;
mod playback_rate;
mod playback_window;
mod scrub;
mod scrub_driver;
mod seek_commit_gates;
mod seek_completion;
mod seek_diagnostics;
mod seek_regressions;
mod seek_start;
mod seek_trace;
mod staged_media_install;
mod timeline_seek;
