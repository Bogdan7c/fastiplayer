//! FFmpeg send/receive decoder thread.
//!
//! Модуль держит FFmpeg decode state внутри `video-ffmpeg` и отдаёт наружу
//! только нейтральный `VideoDecoderThreadHandle`. AVFrame-backed HostPlanar
//! resource table остаётся внутренней частью этого backend-а.

#[cfg(feature = "ffmpeg")]
use std::collections::HashMap;
#[cfg(feature = "ffmpeg")]
use std::sync::TryLockError;
#[cfg(feature = "ffmpeg")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(test, feature = "ffmpeg"))]
use std::sync::{Arc, Mutex};
#[cfg(any(test, feature = "ffmpeg"))]
use std::time::Duration;
#[cfg(feature = "ffmpeg")]
use std::time::Instant;

#[cfg(feature = "ffmpeg")]
use codec_core::VideoDecodeRequirement;
#[cfg(any(test, feature = "ffmpeg"))]
use codec_core::{
    ColorPrimaries, ColorRange, HdrMetadata, MatrixCoefficients, TransferFunction,
    VideoColorMetadata,
};
#[cfg(feature = "ffmpeg")]
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded};
use thiserror::Error;
use video_backend_api::StartedVideoBackend;
#[cfg(feature = "ffmpeg")]
use video_backend_api::{
    PresentFrameResourceDescriptorLookup, PresentFrameResourceProvider,
    PresentFrameResourceProviderHandle, PresentFrameResourceProviderLookup,
};
use video_core::VideoDecoderThreadConfig;
#[cfg(feature = "ffmpeg")]
use video_core::VideoStreamConfigRejection;
#[cfg(feature = "ffmpeg")]
use video_core::{
    DecodeBackpressureReason, DecodeSendError, DecodedFrame, FrameResourceDescriptor,
    FrameResourceHandle, HostPlanarFrameDescriptor, HostPlanarFrameOwner, HostPlaneDescriptor,
    HostPlaneRole, HostUploadResourceSnapshot, HostUploadResourceSnapshotStatus,
    VideoDecoderActivitySnapshot, VideoDecoderActivitySubscription,
    VideoDecoderControlBackpressureReason, VideoDecoderControlChannelPressureSnapshot,
    VideoDecoderEndOfStreamDrainResult, VideoDecoderThreadHandle, VideoFrameDiagnostics,
    VideoStreamConfigResult, VideoStreamDecodeConfig,
    validate_resource_descriptor_against_contract,
};
#[cfg(any(test, feature = "ffmpeg"))]
use video_core::{
    DecodePacket, DecodeThreadError, VideoDecoderActivityNotifier,
    VideoDecoderEndOfStreamDrainState,
};
#[cfg(feature = "ffmpeg")]
use video_frame_contract::{
    FrameBitDepth, FrameChromaSubsampling, VideoFramePixelLayout, VideoFrameTransferPath,
};

use crate::ffi::error::FfmpegError;
#[cfg(feature = "ffmpeg")]
use crate::ffi::error::FfmpegErrorKind;
#[cfg(any(test, feature = "ffmpeg"))]
use crate::ffi::frame::FrameTimestamps;
#[cfg(feature = "ffmpeg")]
use crate::ffi::packet::PacketTimestamps;
#[cfg(test)]
use crate::ffi::packet::PaddedPacketBytes;

#[cfg(feature = "ffmpeg")]
use crate::codec_adapter::{color_metadata_plan_from_ffmpeg_frame, plan_ffmpeg_software_decode};
#[cfg(feature = "ffmpeg")]
use crate::ffi::codec_context::{CodecContext, FfmpegCodecContextRequest};
#[cfg(feature = "ffmpeg")]
use crate::ffi::frame::OwnedAvFrame;
#[cfg(feature = "ffmpeg")]
use crate::ffi::packet::OwnedAvPacket;
#[cfg(feature = "ffmpeg")]
use crate::ffi::pixel_format::SoftwarePixelFormat;

/// FFmpeg `AV_NOPTS_VALUE` без зависимости default build-а от headers/libs.
#[cfg(any(test, feature = "ffmpeg"))]
const NO_TIMESTAMP: i64 = i64::MIN;

/// Config object exists now so future fields do not leak through `player-core`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegDecoderThreadConfig {
    /// Backend-neutral channel limits and flush timeout.
    thread_config: VideoDecoderThreadConfig,
}

impl FfmpegDecoderThreadConfig {
    /// Создаёт FFmpeg config из уже выбранных neutral decoder-thread limits.
    #[must_use]
    pub fn from_thread_config(thread_config: VideoDecoderThreadConfig) -> Self {
        Self {
            thread_config: thread_config.normalized(),
        }
    }

    /// Возвращает normalized neutral runtime config.
    #[must_use]
    pub const fn thread_config(self) -> VideoDecoderThreadConfig {
        self.thread_config
    }
}

impl Default for FfmpegDecoderThreadConfig {
    fn default() -> Self {
        Self::from_thread_config(VideoDecoderThreadConfig::default())
    }
}

/// Startup/decode errors owned by the FFmpeg backend layer.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FfmpegDecoderThreadError {
    /// Build собран без feature `ffmpeg`, поэтому raw FFmpeg backend недоступен.
    #[error("FFmpeg decoder thread unavailable because feature `ffmpeg` is disabled")]
    FeatureDisabled,

    /// Crate scaffold уже существует, но конкретная часть backend-а ещё не подключена.
    #[error("FFmpeg decoder thread operation is not implemented yet")]
    DecodeNotImplemented,

    /// OS не смогла создать decoder thread.
    #[error("failed to spawn FFmpeg decoder thread: {reason}")]
    ThreadSpawn {
        /// Текст ошибки thread builder-а.
        reason: String,
    },

    /// Decoder получил packet до успешной stream configuration.
    #[error("FFmpeg decoder received a packet before stream configuration")]
    DecoderNotConfigured,

    /// Send/receive API нарушил documented progress contract.
    #[error("FFmpeg decoder protocol violation: {reason}")]
    ProtocolViolation {
        /// Человекочитаемая причина fail-closed остановки.
        reason: String,
    },

    /// FFI layer вернул typed FFmpeg error.
    #[error(transparent)]
    Ffi(#[from] FfmpegError),
}

/// Стартует playback-facing FFmpeg decoder thread.
pub fn start_decoder_thread(
    config: FfmpegDecoderThreadConfig,
) -> Result<StartedVideoBackend, FfmpegDecoderThreadError> {
    #[cfg(not(feature = "ffmpeg"))]
    {
        let _config = config;
        Err(FfmpegDecoderThreadError::FeatureDisabled)
    }

    #[cfg(feature = "ffmpeg")]
    {
        FfmpegVideoDecoderThread::spawn(config).map(StartedVideoBackend::from_decoder_thread)
    }
}

/// Playback-facing handle, который скрывает concrete FFmpeg worker thread.
#[cfg(feature = "ffmpeg")]
pub struct FfmpegVideoDecoderThread {
    /// Encoded packet queue from player/session to FFmpeg worker.
    packet_tx: Sender<DecodePacket>,

    /// Control queue for config/flush/EOF-drain lifecycle commands.
    control_tx: Sender<FfmpegDecoderControl>,

    /// Decoded-frame channel from FFmpeg worker to playback/session.
    frame_rx: Receiver<DecodedFrame>,

    /// Fatal decoder-thread errors reported to player-core.
    error_rx: Receiver<DecodeThreadError>,

    /// Packet-completion pulses for player in-flight accounting.
    packet_ack_rx: Receiver<usize>,

    /// Activity subscription exposed through neutral video-core contract.
    activity_subscription: VideoDecoderActivitySubscription,

    /// Renderer-facing provider handle over the AVFrame-backed resource table.
    resource_provider: PresentFrameResourceProviderHandle,

    /// Concrete provider is kept here for software host-upload accounting.
    host_resource_provider: FfmpegHostResourceProvider,

    /// Shared EOF/DPB drain state visible without peeking into worker internals.
    eof_drain_state: Arc<Mutex<VideoDecoderEndOfStreamDrainState>>,

    /// Normalized channel limits used by this handle.
    thread_config: VideoDecoderThreadConfig,

    /// Diagnostics counters for failed bounded control sends.
    control_pressure: Arc<FfmpegControlPressureCounters>,
}

#[cfg(feature = "ffmpeg")]
impl FfmpegVideoDecoderThread {
    /// Spawns the concrete FFmpeg worker. The codec is opened later by `configure_stream`.
    #[cfg(feature = "ffmpeg")]
    fn spawn(config: FfmpegDecoderThreadConfig) -> Result<Self, FfmpegDecoderThreadError> {
        let thread_config = config.thread_config().normalized();
        let (packet_tx, packet_rx) = bounded(thread_config.packet_channel_frames);
        let (control_tx, control_rx) = bounded(thread_config.control_channel_frames);
        let (frame_tx, frame_rx) = bounded(thread_config.frame_channel_frames);
        let (error_tx, error_rx) = bounded(1);
        let (packet_ack_tx, packet_ack_rx) = bounded(thread_config.packet_channel_frames);
        let (activity_notifier, activity_subscription) = VideoDecoderActivityNotifier::new();
        let eof_drain_state = Arc::new(Mutex::new(VideoDecoderEndOfStreamDrainState::Idle));
        let control_pressure = Arc::new(FfmpegControlPressureCounters::default());
        let host_resource_provider =
            FfmpegHostResourceProvider::new(thread_config.frame_channel_frames);
        let worker = FfmpegDecoderWorker {
            active_decoder: None,
            activity_notifier,
            eof_drain_state: eof_drain_state.clone(),
            frame_tx,
            resource_provider: host_resource_provider.clone(),
            packet_ack_tx,
            error_tx,
        };

        std::thread::Builder::new()
            .name("ffmpeg-video-decoder".to_owned())
            .spawn(move || worker.run(packet_rx, control_rx))
            .map_err(|error| FfmpegDecoderThreadError::ThreadSpawn {
                reason: error.to_string(),
            })?;

        Ok(Self {
            packet_tx,
            control_tx,
            frame_rx,
            error_rx,
            packet_ack_rx,
            activity_subscription,
            resource_provider: PresentFrameResourceProviderHandle::new(
                host_resource_provider.clone(),
            ),
            host_resource_provider,
            eof_drain_state,
            thread_config,
            control_pressure,
        })
    }

    /// Создаёт typed backpressure payload из текущего bounded control channel-а.
    fn control_backpressure(&self) -> VideoDecoderControlBackpressureReason {
        VideoDecoderControlBackpressureReason::ControlChannelFull {
            queued_messages: self.control_tx.len(),
            capacity: self
                .control_tx
                .capacity()
                .unwrap_or(self.thread_config.control_channel_frames),
        }
    }

    /// Единая fatal ошибка, если worker уже остановился или reply channel пропал.
    fn control_channel_stopped(operation: &'static str) -> DecodeThreadError {
        DecodeThreadError::new(format!(
            "FFmpeg decoder control channel stopped during {operation}"
        ))
    }

    /// Единая fatal ошибка, если control reply не пришёл за configured timeout.
    fn control_reply_timeout(operation: &'static str) -> DecodeThreadError {
        DecodeThreadError::new(format!(
            "FFmpeg decoder control command `{operation}` timed out"
        ))
    }

    /// Преобразует failed send control command в result для configure/clear/drain.
    fn control_send_failure_result<T>(
        &self,
        error: TrySendError<T>,
        operation: &'static str,
    ) -> Result<(), VideoStreamConfigResult> {
        match error {
            TrySendError::Full(_) => {
                self.control_pressure.record_full();
                Err(VideoStreamConfigResult::Backpressure(
                    self.control_backpressure(),
                ))
            }
            TrySendError::Disconnected(_) => Err(VideoStreamConfigResult::Fatal(
                Self::control_channel_stopped(operation),
            )),
        }
    }

    /// Преобразует failed send control command в EOF-drain result.
    fn control_send_failure_drain<T>(
        &self,
        error: TrySendError<T>,
        operation: &'static str,
    ) -> VideoDecoderEndOfStreamDrainResult {
        match error {
            TrySendError::Full(_) => {
                self.control_pressure.record_full();
                VideoDecoderEndOfStreamDrainResult::Backpressure(self.control_backpressure())
            }
            TrySendError::Disconnected(_) => {
                VideoDecoderEndOfStreamDrainResult::Fatal(Self::control_channel_stopped(operation))
            }
        }
    }
}

#[cfg(feature = "ffmpeg")]
impl VideoDecoderThreadHandle for FfmpegVideoDecoderThread {
    type ResourceProvider = PresentFrameResourceProviderHandle;

    fn backend_name(&self) -> &'static str {
        "ffmpeg-software"
    }

    fn send_packet(&self, packet: DecodePacket) -> Result<(), DecodeSendError> {
        self.packet_tx
            .try_send(packet)
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    DecodeSendError::Backpressure(DecodeBackpressureReason::PacketQueueFull {
                        queued_packets: self.packet_tx.len(),
                        capacity: self
                            .packet_tx
                            .capacity()
                            .unwrap_or(self.thread_config.packet_channel_frames),
                    })
                }
                TrySendError::Disconnected(_) => DecodeSendError::Fatal(DecodeThreadError::new(
                    "FFmpeg decoder packet channel is disconnected",
                )),
            })
    }

    fn configure_stream(&self, config: VideoStreamDecodeConfig) -> VideoStreamConfigResult {
        let (reply_tx, reply_rx) = bounded(1);
        let command = FfmpegDecoderControl::Configure { config, reply_tx };

        if let Err(error) = self.control_tx.try_send(command) {
            return match self.control_send_failure_result(error, "configure_stream") {
                Ok(()) => unreachable!("control_send_failure_result only returns Err"),
                Err(result) => result,
            };
        }

        match reply_rx.recv_timeout(self.thread_config.flush_timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                VideoStreamConfigResult::Fatal(Self::control_reply_timeout("configure_stream"))
            }
            Err(RecvTimeoutError::Disconnected) => {
                VideoStreamConfigResult::Fatal(Self::control_channel_stopped("configure_stream"))
            }
        }
    }

    fn clear_stream(&self) -> VideoStreamConfigResult {
        let (reply_tx, reply_rx) = bounded(1);
        let command = FfmpegDecoderControl::Clear { reply_tx };

        if let Err(error) = self.control_tx.try_send(command) {
            return match self.control_send_failure_result(error, "clear_stream") {
                Ok(()) => unreachable!("control_send_failure_result only returns Err"),
                Err(result) => result,
            };
        }

        match reply_rx.recv_timeout(self.thread_config.flush_timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                VideoStreamConfigResult::Fatal(Self::control_reply_timeout("clear_stream"))
            }
            Err(RecvTimeoutError::Disconnected) => {
                VideoStreamConfigResult::Fatal(Self::control_channel_stopped("clear_stream"))
            }
        }
    }

    fn begin_end_of_stream_drain(&self, generation: u64) -> VideoDecoderEndOfStreamDrainResult {
        let (reply_tx, reply_rx) = bounded(1);
        let command = FfmpegDecoderControl::BeginEndOfStreamDrain {
            generation,
            reply_tx,
        };

        if let Err(error) = self.control_tx.try_send(command) {
            return self.control_send_failure_drain(error, "begin_end_of_stream_drain");
        }

        match reply_rx.recv_timeout(self.thread_config.flush_timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => VideoDecoderEndOfStreamDrainResult::Fatal(
                Self::control_reply_timeout("begin_end_of_stream_drain"),
            ),
            Err(RecvTimeoutError::Disconnected) => VideoDecoderEndOfStreamDrainResult::Fatal(
                Self::control_channel_stopped("begin_end_of_stream_drain"),
            ),
        }
    }

    fn end_of_stream_drain_state(&self) -> VideoDecoderEndOfStreamDrainState {
        self.eof_drain_state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|_| VideoDecoderEndOfStreamDrainState::Fatal {
                generation: None,
                error: DecodeThreadError::new("FFmpeg EOF drain state lock is poisoned"),
            })
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        self.resource_provider.release_frame(handle);
    }

    fn try_recv_frame(&self) -> Option<DecodedFrame> {
        self.frame_rx.try_recv().ok()
    }

    fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
        None
    }

    fn try_recv_error(&self) -> Option<DecodeThreadError> {
        self.error_rx.try_recv().ok()
    }

    fn flush(&self) -> anyhow::Result<()> {
        let (reply_tx, reply_rx) = bounded(1);
        let command = FfmpegDecoderControl::Flush { reply_tx };

        if let Err(error) = self.control_tx.try_send(command) {
            return match error {
                TrySendError::Full(_) => {
                    self.control_pressure.record_full();
                    self.control_pressure.record_flush_send_fail();
                    Err(anyhow::anyhow!(
                        "FFmpeg decoder flush control channel is full: {:?}",
                        self.control_backpressure()
                    ))
                }
                TrySendError::Disconnected(_) => Err(anyhow::anyhow!(
                    "{}",
                    Self::control_channel_stopped("flush")
                )),
            };
        }

        match reply_rx.recv_timeout(self.thread_config.flush_timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(anyhow::anyhow!(error)),
            Err(RecvTimeoutError::Timeout) => {
                Err(anyhow::anyhow!("{}", Self::control_reply_timeout("flush")))
            }
            Err(RecvTimeoutError::Disconnected) => Err(anyhow::anyhow!(
                "{}",
                Self::control_channel_stopped("flush")
            )),
        }
    }

    fn resource_provider(&self) -> PresentFrameResourceProviderHandle {
        self.resource_provider.clone()
    }

    fn decoder_resource_snapshot(&self) -> Option<video_core::DecoderResourceSnapshot> {
        None
    }

    fn host_upload_resource_snapshot(&self) -> HostUploadResourceSnapshotStatus {
        HostUploadResourceSnapshotStatus::Available(
            self.host_resource_provider.snapshot(self.frame_rx.len()),
        )
    }

    fn decoder_control_channel_pressure(
        &self,
    ) -> Option<VideoDecoderControlChannelPressureSnapshot> {
        Some(VideoDecoderControlChannelPressureSnapshot {
            control_channel_len: self.control_tx.len(),
            control_channel_capacity: self
                .control_tx
                .capacity()
                .unwrap_or(self.thread_config.control_channel_frames),
            control_channel_full_count: self.control_pressure.full_count(),
            release_control_send_fail_count: self.control_pressure.release_send_fail_count(),
            flush_control_send_fail_count: self.control_pressure.flush_send_fail_count(),
        })
    }

    fn decoder_activity_snapshot(&self) -> VideoDecoderActivitySnapshot {
        self.activity_subscription.snapshot()
    }

    fn packet_queue_depth(&self) -> usize {
        self.packet_tx.len()
    }

    fn drain_completed_packet_count(&self) -> usize {
        let mut completed_packets = 0usize;

        while let Ok(count) = self.packet_ack_rx.try_recv() {
            completed_packets = completed_packets.saturating_add(count);
        }

        completed_packets
    }
}

/// Bounded control messages handled by the FFmpeg worker thread.
#[cfg(feature = "ffmpeg")]
enum FfmpegDecoderControl {
    /// Applies a new stream configuration without changing seek generation.
    Configure {
        /// Neutral stream configuration selected by player/capability layers.
        config: VideoStreamDecodeConfig,

        /// One-shot reply preserving typed configure outcomes.
        reply_tx: Sender<VideoStreamConfigResult>,
    },

    /// Clears active stream state on media/backend lifecycle reset.
    Clear {
        /// One-shot reply preserving no-op vs cleared.
        reply_tx: Sender<VideoStreamConfigResult>,
    },

    /// Seek flush; distinct from EOF drain.
    Flush {
        /// One-shot reply for fail-fast seek boundary.
        reply_tx: Sender<Result<(), DecodeThreadError>>,
    },

    /// Explicit EOF/DPB drain for current generation.
    BeginEndOfStreamDrain {
        /// Generation whose tail frames should be drained.
        generation: u64,

        /// One-shot reply preserving started/unchanged/fatal states.
        reply_tx: Sender<VideoDecoderEndOfStreamDrainResult>,
    },
}

/// Control pressure counters shared with the playback-facing handle.
#[derive(Debug, Default)]
#[cfg(feature = "ffmpeg")]
struct FfmpegControlPressureCounters {
    /// Any bounded control-channel full event.
    full_count: AtomicU64,

    /// FFmpeg release path is provider-local, so control-channel release sends stay zero.
    release_send_fail_count: AtomicU64,

    /// Flush command send failures are tracked separately for seek diagnostics.
    flush_send_fail_count: AtomicU64,
}

#[cfg(feature = "ffmpeg")]
impl FfmpegControlPressureCounters {
    fn record_full(&self) {
        self.full_count.fetch_add(1, Ordering::Relaxed);
    }

    fn record_flush_send_fail(&self) {
        self.flush_send_fail_count.fetch_add(1, Ordering::Relaxed);
    }

    fn full_count(&self) -> u64 {
        self.full_count.load(Ordering::Relaxed)
    }

    fn release_send_fail_count(&self) -> u64 {
        self.release_send_fail_count.load(Ordering::Relaxed)
    }

    fn flush_send_fail_count(&self) -> u64 {
        self.flush_send_fail_count.load(Ordering::Relaxed)
    }
}

/// Shared provider over AVFrame-backed host-planar resources.
#[derive(Debug, Clone)]
#[cfg(feature = "ffmpeg")]
struct FfmpegHostResourceProvider {
    /// Shared inner state is cloned into both decoder handle and worker thread.
    inner: Arc<FfmpegHostResourceProviderInner>,
}

/// Mutable resource table плюс counters, hidden behind the provider boundary.
#[derive(Debug)]
#[cfg(feature = "ffmpeg")]
struct FfmpegHostResourceProviderInner {
    /// Provider-owned resources that stay alive until renderer calls release.
    table: Mutex<FfmpegHostResourceTable>,

    /// Upper bound for simultaneously retained host frames.
    upload_slots_capacity: usize,

    /// Cumulative failures while creating/publishing host-upload resources.
    upload_failures: AtomicU64,
}

/// Resource table keyed by neutral opaque frame handles.
#[derive(Debug)]
#[cfg(feature = "ffmpeg")]
struct FfmpegHostResourceTable {
    /// Next never-reused handle value for this provider lifetime.
    next_handle: u64,

    /// Active resources still owned by the provider.
    entries: HashMap<FrameResourceHandle, FfmpegHostResourceEntry>,
}

/// One provider-owned resource entry.
#[derive(Debug)]
#[cfg(feature = "ffmpeg")]
struct FfmpegHostResourceEntry {
    /// Generation is diagnostic ownership context; release remains handle-based.
    _generation: u64,

    /// Neutral descriptor whose owner holds the refcounted AVFrame alive.
    descriptor: FrameResourceDescriptor,
}

/// Result of moving one received AVFrame into the provider table.
#[derive(Debug)]
#[cfg(feature = "ffmpeg")]
struct FfmpegHostResourcePublication {
    /// Opaque handle stored in `DecodedFrame`.
    handle: FrameResourceHandle,

    /// Actual coded width read from the received AVFrame.
    width: u32,

    /// Actual coded height read from the received AVFrame.
    height: u32,
}

#[cfg(feature = "ffmpeg")]
impl FfmpegHostResourceProvider {
    /// Создаёт provider с bounded числом host-upload slots.
    fn new(upload_slots_capacity: usize) -> Self {
        let upload_slots_capacity = upload_slots_capacity.max(1);

        Self {
            inner: Arc::new(FfmpegHostResourceProviderInner {
                table: Mutex::new(FfmpegHostResourceTable {
                    next_handle: 1,
                    entries: HashMap::new(),
                }),
                upload_slots_capacity,
                upload_failures: AtomicU64::new(0),
            }),
        }
    }

    /// Converts a refcounted AVFrame into a provider-owned host-planar resource.
    fn insert_frame(
        &self,
        generation: u64,
        frame: OwnedAvFrame,
        expected_contract: video_frame_contract::VideoFrameContract,
    ) -> Result<FfmpegHostResourcePublication, FfmpegDecoderThreadError> {
        let publication = self.insert_frame_inner(generation, frame, expected_contract);

        if publication.is_err() {
            self.record_upload_failure();
        }

        publication
    }

    /// Internal implementation split out so failure accounting stays in one place.
    fn insert_frame_inner(
        &self,
        generation: u64,
        frame: OwnedAvFrame,
        expected_contract: video_frame_contract::VideoFrameContract,
    ) -> Result<FfmpegHostResourcePublication, FfmpegDecoderThreadError> {
        let (descriptor, width, height) = avframe_host_planar_descriptor(frame, expected_contract)?;

        validate_resource_descriptor_against_contract(
            expected_contract,
            width,
            height,
            &descriptor,
        )
        .map_err(|error| {
            invalid_avframe_resource(
                "AVFrame HostPlanar descriptor validation",
                error.to_string(),
            )
        })?;

        let mut table = self.inner.table.lock().map_err(|_| {
            invalid_avframe_resource(
                "AVFrame HostPlanar resource table lock",
                "resource table mutex is poisoned".to_owned(),
            )
        })?;

        if table.entries.len() >= self.inner.upload_slots_capacity {
            return Err(FfmpegDecoderThreadError::ProtocolViolation {
                reason: format!(
                    "FFmpeg host-upload resource table is full: {}/{} slots are occupied",
                    table.entries.len(),
                    self.inner.upload_slots_capacity
                ),
            });
        }

        let handle = table.allocate_handle()?;
        table.entries.insert(
            handle,
            FfmpegHostResourceEntry {
                _generation: generation,
                descriptor,
            },
        );

        Ok(FfmpegHostResourcePublication {
            handle,
            width,
            height,
        })
    }

    /// Snapshot used by the neutral software host-upload backpressure boundary.
    fn snapshot(&self, host_frames_ready: usize) -> HostUploadResourceSnapshot {
        let host_frames_in_flight = self
            .inner
            .table
            .lock()
            .map(|table| table.entries.len())
            .unwrap_or(self.inner.upload_slots_capacity);
        let upload_slots_free = self
            .inner
            .upload_slots_capacity
            .saturating_sub(host_frames_in_flight);

        HostUploadResourceSnapshot {
            host_frames_ready,
            host_frames_in_flight,
            upload_slots_capacity: self.inner.upload_slots_capacity,
            upload_slots_free,
            upload_failures: self.inner.upload_failures.load(Ordering::Relaxed),
        }
    }

    /// Counts resource creation/publish failures without changing error semantics.
    fn record_upload_failure(&self) {
        self.inner.upload_failures.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(feature = "ffmpeg")]
impl FfmpegHostResourceTable {
    /// Allocates the next opaque handle without reusing released values.
    fn allocate_handle(&mut self) -> Result<FrameResourceHandle, FfmpegDecoderThreadError> {
        let handle = FrameResourceHandle(self.next_handle);
        self.next_handle = self.next_handle.checked_add(1).ok_or_else(|| {
            FfmpegDecoderThreadError::ProtocolViolation {
                reason: "FFmpeg host-upload resource handle counter overflowed".to_owned(),
            }
        })?;
        Ok(handle)
    }
}

#[cfg(feature = "ffmpeg")]
impl PresentFrameResourceProvider for FfmpegHostResourceProvider {
    fn resource_lookup(&self, handle: FrameResourceHandle) -> PresentFrameResourceProviderLookup {
        let lock_start = Instant::now();
        match self.inner.table.lock() {
            Ok(table) => resource_lookup_from_table(&table, handle, lock_start.elapsed()),
            Err(_) => PresentFrameResourceProviderLookup::Fatal {
                resource_pool_lock_wait: lock_start.elapsed(),
            },
        }
    }

    fn try_resource_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceProviderLookup {
        let lock_start = Instant::now();
        match self.inner.table.try_lock() {
            Ok(table) => resource_lookup_from_table(&table, handle, lock_start.elapsed()),
            Err(TryLockError::WouldBlock) => PresentFrameResourceProviderLookup::Busy {
                resource_pool_lock_wait: lock_start.elapsed(),
            },
            Err(TryLockError::Poisoned(_)) => PresentFrameResourceProviderLookup::Fatal {
                resource_pool_lock_wait: lock_start.elapsed(),
            },
        }
    }

    fn resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        let lock_start = Instant::now();
        match self.inner.table.lock() {
            Ok(table) => descriptor_lookup_from_table(&table, handle, lock_start.elapsed()),
            Err(_) => PresentFrameResourceDescriptorLookup::Fatal {
                resource_pool_lock_wait: lock_start.elapsed(),
            },
        }
    }

    fn try_resource_descriptor_lookup(
        &self,
        handle: FrameResourceHandle,
    ) -> PresentFrameResourceDescriptorLookup {
        let lock_start = Instant::now();
        match self.inner.table.try_lock() {
            Ok(table) => descriptor_lookup_from_table(&table, handle, lock_start.elapsed()),
            Err(TryLockError::WouldBlock) => PresentFrameResourceDescriptorLookup::Busy {
                resource_pool_lock_wait: lock_start.elapsed(),
            },
            Err(TryLockError::Poisoned(_)) => PresentFrameResourceDescriptorLookup::Fatal {
                resource_pool_lock_wait: lock_start.elapsed(),
            },
        }
    }

    fn release_frame(&self, handle: FrameResourceHandle) {
        if let Ok(mut table) = self.inner.table.lock() {
            table.entries.remove(&handle);
        }
    }
}

#[cfg(feature = "ffmpeg")]
fn resource_lookup_from_table(
    table: &FfmpegHostResourceTable,
    handle: FrameResourceHandle,
    resource_pool_lock_wait: Duration,
) -> PresentFrameResourceProviderLookup {
    if table.entries.contains_key(&handle) {
        PresentFrameResourceProviderLookup::Ready {
            resource_pool_lock_wait,
        }
    } else {
        PresentFrameResourceProviderLookup::Missing {
            resource_pool_lock_wait,
        }
    }
}

#[cfg(feature = "ffmpeg")]
fn descriptor_lookup_from_table(
    table: &FfmpegHostResourceTable,
    handle: FrameResourceHandle,
    resource_pool_lock_wait: Duration,
) -> PresentFrameResourceDescriptorLookup {
    let Some(entry) = table.entries.get(&handle) else {
        return PresentFrameResourceDescriptorLookup::Missing {
            resource_pool_lock_wait,
        };
    };

    match entry.descriptor.try_clone_for_lookup() {
        Ok(descriptor) => PresentFrameResourceDescriptorLookup::Ready {
            descriptor,
            resource_pool_lock_wait,
        },
        Err(_) => PresentFrameResourceDescriptorLookup::Fatal {
            resource_pool_lock_wait,
        },
    }
}

/// HostPlanar owner that keeps a refcounted AVFrame alive behind video-core API.
#[derive(Debug)]
#[cfg(feature = "ffmpeg")]
struct AvFrameHostPlanarOwner {
    /// Separate `av_frame_ref` owned by the resource table/descriptor clones.
    frame: OwnedAvFrame,
}

// SAFETY: after publication this owner never mutates the wrapped AVFrame. Its
// safe API exposes only immutable row slices, and Drop only releases FFmpeg's
// refcounted buffers when the last descriptor clone disappears.
#[cfg(feature = "ffmpeg")]
unsafe impl Sync for AvFrameHostPlanarOwner {}

#[cfg(feature = "ffmpeg")]
impl HostPlanarFrameOwner for AvFrameHostPlanarOwner {
    fn visible_row_bytes(
        &self,
        plane_index: usize,
        plane: &HostPlaneDescriptor,
        row_index: u32,
        visible_row_bytes: usize,
    ) -> anyhow::Result<&[u8]> {
        if plane.offset != 0 {
            return Err(anyhow::anyhow!(
                "AVFrame-backed host-planar {:?} plane uses non-zero owner offset {}",
                plane.role,
                plane.offset
            ));
        }

        let row_index = usize::try_from(row_index).map_err(|_| {
            anyhow::anyhow!(
                "AVFrame-backed host-planar {:?} row index {} does not fit usize",
                plane.role,
                row_index
            )
        })?;

        self.frame
            .plane_row_data(plane_index, row_index, visible_row_bytes)
            .map_err(|error| anyhow::anyhow!(error))?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "AVFrame-backed host-planar {:?} plane {} has null data pointer",
                    plane.role,
                    plane_index
                )
            })
    }
}

#[cfg(feature = "ffmpeg")]
fn avframe_host_planar_descriptor(
    frame: OwnedAvFrame,
    expected_contract: video_frame_contract::VideoFrameContract,
) -> Result<(FrameResourceDescriptor, u32, u32), FfmpegDecoderThreadError> {
    if expected_contract.transfer_path != VideoFrameTransferPath::SoftwareHostUpload {
        return Err(invalid_avframe_resource(
            "AVFrame HostPlanar descriptor",
            format!(
                "expected SoftwareHostUpload contract, got {}",
                expected_contract.transfer_path
            ),
        ));
    }

    let frame_format = frame.software_format().ok_or_else(|| {
        invalid_avframe_resource(
            "AVFrame pixel format",
            format!(
                "AVFrame format code {} is not a supported v1 software planar YUV format",
                frame.raw_format_code()
            ),
        )
    })?;
    ensure_frame_format_matches_contract(frame_format, expected_contract.pixel_layout)?;

    let width = positive_avframe_dimension("width", frame.width())?;
    let height = positive_avframe_dimension("height", frame.height())?;
    let planes =
        avframe_host_plane_descriptors(&frame, expected_contract.pixel_layout, width, height)?;
    let owner = Arc::new(AvFrameHostPlanarOwner { frame });
    let descriptor = HostPlanarFrameDescriptor::new(owner, planes);

    Ok((
        FrameResourceDescriptor::HostPlanar(descriptor),
        width,
        height,
    ))
}

#[cfg(feature = "ffmpeg")]
fn ensure_frame_format_matches_contract(
    frame_format: SoftwarePixelFormat,
    expected_layout: VideoFramePixelLayout,
) -> Result<(), FfmpegDecoderThreadError> {
    let actual_layout = frame_format.frame_pixel_layout();
    if actual_layout != expected_layout {
        return Err(invalid_avframe_resource(
            "AVFrame pixel format",
            format!(
                "decoded AVFrame layout {} does not match selected contract {}",
                actual_layout, expected_layout
            ),
        ));
    }

    Ok(())
}

#[cfg(feature = "ffmpeg")]
fn positive_avframe_dimension(
    name: &'static str,
    value: i32,
) -> Result<u32, FfmpegDecoderThreadError> {
    u32::try_from(value)
        .ok()
        .filter(|dimension| *dimension > 0)
        .ok_or_else(|| {
            invalid_avframe_resource(
                "AVFrame dimensions",
                format!("AVFrame {name} must be positive, got {value}"),
            )
        })
}

#[cfg(feature = "ffmpeg")]
fn avframe_host_plane_descriptors(
    frame: &OwnedAvFrame,
    pixel_layout: VideoFramePixelLayout,
    width: u32,
    height: u32,
) -> Result<Vec<HostPlaneDescriptor>, FfmpegDecoderThreadError> {
    let bytes_per_sample = host_planar_bytes_per_sample(pixel_layout)?;
    let (chroma_width, chroma_height) = host_planar_chroma_size(pixel_layout, width, height)?;
    let plane_specs = [
        (HostPlaneRole::Luma, width, height),
        (HostPlaneRole::ChromaU, chroma_width, chroma_height),
        (HostPlaneRole::ChromaV, chroma_width, chroma_height),
    ];

    plane_specs
        .into_iter()
        .enumerate()
        .map(|(plane_index, (role, visible_width, visible_height))| {
            let stride = positive_avframe_linesize(frame, plane_index, role)?;

            Ok(HostPlaneDescriptor {
                role,
                offset: 0,
                stride,
                visible_width,
                visible_height,
                bytes_per_sample,
            })
        })
        .collect()
}

#[cfg(feature = "ffmpeg")]
fn positive_avframe_linesize(
    frame: &OwnedAvFrame,
    plane_index: usize,
    role: HostPlaneRole,
) -> Result<usize, FfmpegDecoderThreadError> {
    let line_size = frame.linesize(plane_index).ok_or_else(|| {
        invalid_avframe_resource(
            "AVFrame linesize",
            format!(
                "AVFrame {:?} plane index {} is outside linesize table",
                role, plane_index
            ),
        )
    })?;

    usize::try_from(line_size)
        .ok()
        .filter(|line_size| *line_size > 0)
        .ok_or_else(|| {
            invalid_avframe_resource(
                "AVFrame linesize",
                format!(
                    "AVFrame {:?} plane {} has unsupported non-positive linesize {}",
                    role, plane_index, line_size
                ),
            )
        })
}

#[cfg(feature = "ffmpeg")]
fn host_planar_bytes_per_sample(
    pixel_layout: VideoFramePixelLayout,
) -> Result<usize, FfmpegDecoderThreadError> {
    match pixel_layout.bit_depth() {
        Some(FrameBitDepth::Eight) => Ok(1),
        Some(FrameBitDepth::Ten | FrameBitDepth::Twelve) => Ok(2),
        None => Err(invalid_avframe_resource(
            "AVFrame HostPlanar layout",
            format!("{pixel_layout} is not a host-planar YUV layout"),
        )),
    }
}

#[cfg(feature = "ffmpeg")]
fn host_planar_chroma_size(
    pixel_layout: VideoFramePixelLayout,
    width: u32,
    height: u32,
) -> Result<(u32, u32), FfmpegDecoderThreadError> {
    match pixel_layout.chroma() {
        Some(FrameChromaSubsampling::Yuv420) => {
            Ok((half_rounded_up(width), half_rounded_up(height)))
        }
        Some(FrameChromaSubsampling::Yuv422) => Ok((half_rounded_up(width), height)),
        Some(FrameChromaSubsampling::Yuv444) => Ok((width, height)),
        None => Err(invalid_avframe_resource(
            "AVFrame HostPlanar layout",
            format!("{pixel_layout} is not a planar YUV layout"),
        )),
    }
}

#[cfg(feature = "ffmpeg")]
const fn half_rounded_up(value: u32) -> u32 {
    (value / 2) + (value % 2)
}

#[cfg(feature = "ffmpeg")]
fn invalid_avframe_resource(operation: &'static str, details: String) -> FfmpegDecoderThreadError {
    FfmpegDecoderThreadError::Ffi(FfmpegError::InvalidInput { operation, details })
}

/// Worker thread state that owns the active FFmpeg codec instance.
#[cfg(feature = "ffmpeg")]
struct FfmpegDecoderWorker {
    /// Configured decoder loop, absent before `configure_stream`.
    active_decoder: Option<ConfiguredFfmpegDecoder>,

    /// Producer side of neutral decoder activity notifications.
    activity_notifier: VideoDecoderActivityNotifier,

    /// Shared EOF drain state read by the playback-facing handle.
    eof_drain_state: Arc<Mutex<VideoDecoderEndOfStreamDrainState>>,

    /// Decoded-frame publisher for playback/session.
    frame_tx: Sender<DecodedFrame>,

    /// AVFrame-backed resource table shared with renderer lookup/release path.
    resource_provider: FfmpegHostResourceProvider,

    /// Packet completion acknowledgements for player in-flight accounting.
    packet_ack_tx: Sender<usize>,

    /// Fatal errors surfaced through `try_recv_error`.
    error_tx: Sender<DecodeThreadError>,
}

#[cfg(feature = "ffmpeg")]
impl FfmpegDecoderWorker {
    fn run(
        mut self,
        packet_rx: Receiver<DecodePacket>,
        control_rx: Receiver<FfmpegDecoderControl>,
    ) {
        loop {
            crossbeam_channel::select! {
                recv(control_rx) -> control_message => {
                    match control_message {
                        Ok(control) => self.handle_control(control, &packet_rx),
                        Err(_) if packet_rx.is_empty() => break,
                        Err(_) => {}
                    }
                }
                recv(packet_rx) -> packet_message => {
                    match packet_message {
                        Ok(packet) => self.handle_packet(packet),
                        Err(_) if control_rx.is_empty() => break,
                        Err(_) => {}
                    }
                }
            }
        }
    }

    fn handle_control(
        &mut self,
        control: FfmpegDecoderControl,
        packet_rx: &Receiver<DecodePacket>,
    ) {
        match control {
            FfmpegDecoderControl::Configure { config, reply_tx } => {
                let result = self.configure_stream(config);
                let _ = reply_tx.try_send(result);
            }
            FfmpegDecoderControl::Clear { reply_tx } => {
                let result = self.clear_stream();
                let _ = reply_tx.try_send(result);
            }
            FfmpegDecoderControl::Flush { reply_tx } => {
                self.drop_queued_packets_after_seek_flush(packet_rx);
                let result = self
                    .flush_for_seek()
                    .map_err(decode_thread_error_from_ffmpeg);
                let _ = reply_tx.try_send(result);
            }
            FfmpegDecoderControl::BeginEndOfStreamDrain {
                generation,
                reply_tx,
            } => {
                let result = self.begin_end_of_stream_drain(generation);
                let _ = reply_tx.try_send(result);
            }
        }
    }

    fn configure_stream(&mut self, config: VideoStreamDecodeConfig) -> VideoStreamConfigResult {
        if self
            .active_decoder
            .as_ref()
            .is_some_and(|active_decoder| active_decoder.config == config)
        {
            return VideoStreamConfigResult::Unchanged;
        }

        let codec_api = match RealFfmpegDecodeApi::open(&config) {
            Ok(codec_api) => codec_api,
            Err(FfmpegOpenDecoderError::Unsupported(rejection)) => {
                return VideoStreamConfigResult::Unsupported(rejection);
            }
            Err(FfmpegOpenDecoderError::Fatal(error)) => {
                return VideoStreamConfigResult::Fatal(decode_thread_error_from_ffmpeg(error));
            }
        };

        let decode_loop = SendReceiveDecodeLoop::new(
            codec_api,
            self.activity_notifier.clone(),
            self.eof_drain_state.clone(),
        );

        self.active_decoder = Some(ConfiguredFfmpegDecoder {
            config,
            decode_loop,
        });
        let _ = self.activity_notifier.notify_activity();

        VideoStreamConfigResult::Configured
    }

    fn clear_stream(&mut self) -> VideoStreamConfigResult {
        if self.active_decoder.take().is_none() {
            return VideoStreamConfigResult::Unchanged;
        }

        if let Err(error) = set_eof_drain_state(
            &self.eof_drain_state,
            VideoDecoderEndOfStreamDrainState::Idle,
            &self.activity_notifier,
        ) {
            return VideoStreamConfigResult::Fatal(decode_thread_error_from_ffmpeg(error));
        }

        let _ = self.activity_notifier.notify_activity();

        VideoStreamConfigResult::Cleared
    }

    fn flush_for_seek(&mut self) -> Result<(), FfmpegDecoderThreadError> {
        if let Some(active_decoder) = self.active_decoder.as_mut() {
            active_decoder.decode_loop.flush_for_seek()?;
        } else {
            set_eof_drain_state(
                &self.eof_drain_state,
                VideoDecoderEndOfStreamDrainState::Idle,
                &self.activity_notifier,
            )?;
        }

        let _ = self.activity_notifier.notify_activity();
        Ok(())
    }

    fn begin_end_of_stream_drain(&mut self, generation: u64) -> VideoDecoderEndOfStreamDrainResult {
        let drain_result = {
            let Some(active_decoder) = self.active_decoder.as_mut() else {
                let drained = VideoDecoderEndOfStreamDrainState::Drained { generation };
                if let Err(error) = set_eof_drain_state(
                    &self.eof_drain_state,
                    drained.clone(),
                    &self.activity_notifier,
                ) {
                    return VideoDecoderEndOfStreamDrainResult::Fatal(
                        decode_thread_error_from_ffmpeg(error),
                    );
                }
                return VideoDecoderEndOfStreamDrainResult::Started(drained);
            };

            let current_state = active_decoder.decode_loop.end_of_stream_drain_state();
            if matches!(
                current_state,
                VideoDecoderEndOfStreamDrainState::Draining { generation: active_generation }
                    | VideoDecoderEndOfStreamDrainState::Drained { generation: active_generation }
                    if active_generation == generation
            ) {
                return VideoDecoderEndOfStreamDrainResult::Unchanged(current_state);
            }

            let config = active_decoder.config.clone();
            active_decoder
                .decode_loop
                .begin_end_of_stream_drain(generation)
                .map(|progress_report| (config, progress_report))
        };

        match drain_result {
            Ok((config, progress_report)) => {
                if let Err(error) = self.publish_decoded_frames(&config, progress_report.frames) {
                    let thread_error = decode_thread_error_from_ffmpeg(error.clone());
                    let _ = set_eof_drain_state(
                        &self.eof_drain_state,
                        VideoDecoderEndOfStreamDrainState::Fatal {
                            generation: Some(generation),
                            error: thread_error.clone(),
                        },
                        &self.activity_notifier,
                    );
                    VideoDecoderEndOfStreamDrainResult::Fatal(thread_error)
                } else {
                    VideoDecoderEndOfStreamDrainResult::Started(progress_report.state)
                }
            }
            Err(error) => {
                let thread_error = decode_thread_error_from_ffmpeg(error.clone());
                let _ = set_eof_drain_state(
                    &self.eof_drain_state,
                    VideoDecoderEndOfStreamDrainState::Fatal {
                        generation: Some(generation),
                        error: thread_error.clone(),
                    },
                    &self.activity_notifier,
                );
                VideoDecoderEndOfStreamDrainResult::Fatal(thread_error)
            }
        }
    }

    fn handle_packet(&mut self, packet: DecodePacket) {
        let decode_result = {
            let Some(active_decoder) = self.active_decoder.as_mut() else {
                self.report_fatal_error(FfmpegDecoderThreadError::DecoderNotConfigured);
                return;
            };

            let config = active_decoder.config.clone();
            active_decoder
                .decode_loop
                .send_packet(packet)
                .map(|progress_report| (config, progress_report))
        };

        match decode_result {
            Ok((config, progress_report)) => {
                if let Err(error) = self.publish_decoded_frames(&config, progress_report.frames) {
                    self.report_fatal_error(error);
                    return;
                }

                if progress_report.packet_completed {
                    let _ = self.packet_ack_tx.try_send(1);
                }
                let _ = self.activity_notifier.notify_activity();
            }
            Err(error) => self.report_fatal_error(error),
        }
    }

    fn publish_decoded_frames(
        &self,
        config: &VideoStreamDecodeConfig,
        frames: Vec<DecodedFrameRecord>,
    ) -> Result<(), FfmpegDecoderThreadError> {
        for frame_record in frames {
            let decoded_frame = self.decoded_frame_from_record(config, frame_record)?;
            let resource_handle = decoded_frame.resource_handle;

            match self.frame_tx.try_send(decoded_frame) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    self.resource_provider.release_frame(resource_handle);
                    self.resource_provider.record_upload_failure();
                    return Err(FfmpegDecoderThreadError::ProtocolViolation {
                        reason: format!(
                            "FFmpeg decoded-frame channel is full: {}/{} frames queued",
                            self.frame_tx.len(),
                            self.frame_tx.capacity().unwrap_or(0)
                        ),
                    });
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.resource_provider.release_frame(resource_handle);
                    self.resource_provider.record_upload_failure();
                    return Err(FfmpegDecoderThreadError::ProtocolViolation {
                        reason: "FFmpeg decoded-frame channel is disconnected".to_owned(),
                    });
                }
            }
        }

        Ok(())
    }

    fn decoded_frame_from_record(
        &self,
        config: &VideoStreamDecodeConfig,
        frame_record: DecodedFrameRecord,
    ) -> Result<DecodedFrame, FfmpegDecoderThreadError> {
        let frame_ref =
            frame_record
                .frame_ref
                .ok_or_else(|| FfmpegDecoderThreadError::ProtocolViolation {
                    reason: "FFmpeg receive loop produced metadata without an AVFrame reference"
                        .to_owned(),
                })?;
        let color = frame_record
            .color
            .unwrap_or_else(VideoColorMetadata::sdr_bt709_limited);
        let publication = self.resource_provider.insert_frame(
            frame_record.generation,
            frame_ref,
            config.frame_contract,
        )?;

        let decoded_frame = DecodedFrame {
            generation: frame_record.generation,
            pts: frame_record.pts,
            frame_contract: config.frame_contract,
            width: publication.width,
            height: publication.height,
            render_width: publication.width,
            render_height: publication.height,
            display_orientation: config.display_orientation,
            color,
            resource_handle: publication.handle,
            diagnostics: VideoFrameDiagnostics::default(),
        };

        decoded_frame
            .validate_against_expected_contract(config.frame_contract)
            .map_err(|error| {
                invalid_avframe_resource("FFmpeg decoded frame validation", error.to_string())
            })?;

        Ok(decoded_frame)
    }

    fn drop_queued_packets_after_seek_flush(&self, packet_rx: &Receiver<DecodePacket>) {
        while packet_rx.try_recv().is_ok() {}
    }

    fn report_fatal_error(&self, error: FfmpegDecoderThreadError) {
        let thread_error = decode_thread_error_from_ffmpeg(error);
        let _ = self.error_tx.try_send(thread_error);
        let _ = self.activity_notifier.notify_activity();
    }
}

/// Active stream config plus send/receive loop state.
#[cfg(feature = "ffmpeg")]
struct ConfiguredFfmpegDecoder {
    /// Neutral stream config that produced this FFmpeg context.
    config: VideoStreamDecodeConfig,

    /// Testable send/receive state machine.
    decode_loop: SendReceiveDecodeLoop<RealFfmpegDecodeApi>,
}

/// Error returned while opening a configured FFmpeg decoder.
#[cfg(feature = "ffmpeg")]
enum FfmpegOpenDecoderError {
    /// Stream config is not supported by FFmpeg software policy.
    Unsupported(VideoStreamConfigRejection),

    /// FFmpeg failed while allocating/opening the decoder.
    Fatal(FfmpegDecoderThreadError),
}

/// Real FFmpeg implementation of the testable send/receive API.
#[cfg(feature = "ffmpeg")]
struct RealFfmpegDecodeApi {
    /// Safe owner around `AVCodecContext`.
    codec_context: CodecContext,

    /// Reusable receive frame allocation.
    receive_frame: OwnedAvFrame,

    /// Last known stream time base for AVPacket timestamp conversion.
    stream_time_base: Option<StreamTimeBase>,
}

#[cfg(feature = "ffmpeg")]
impl RealFfmpegDecodeApi {
    fn open(config: &VideoStreamDecodeConfig) -> Result<Self, FfmpegOpenDecoderError> {
        let requirement = video_decode_requirement_from_stream_config(config);
        let adapter_plan = plan_ffmpeg_software_decode(&requirement, config.frame_contract)
            .map_err(|error| {
                FfmpegOpenDecoderError::Unsupported(
                    VideoStreamConfigRejection::BackendUnsupported {
                        reason: error.to_string(),
                    },
                )
            })?;
        let request = FfmpegCodecContextRequest::for_decoder_id(
            adapter_plan.decoder_id(),
            adapter_plan.accepted_pixel_formats().clone(),
        );
        let codec_context = CodecContext::open(&request)
            .map_err(FfmpegDecoderThreadError::from)
            .map_err(FfmpegOpenDecoderError::Fatal)?;
        let receive_frame = OwnedAvFrame::allocate_for_decode()
            .map_err(FfmpegDecoderThreadError::from)
            .map_err(FfmpegOpenDecoderError::Fatal)?;

        Ok(Self {
            codec_context,
            receive_frame,
            stream_time_base: None,
        })
    }
}

#[cfg(feature = "ffmpeg")]
impl SendReceiveCodecApi for RealFfmpegDecodeApi {
    type Packet = OwnedAvPacket;

    fn create_packet(
        &mut self,
        packet: &DecodePacket,
    ) -> Result<Self::Packet, FfmpegDecoderThreadError> {
        let mut av_packet = OwnedAvPacket::new(packet.encoded_bytes.as_ref())?;
        let packet_time_base = packet
            .track_dts
            .map(|track_dts| {
                StreamTimeBase::new(track_dts.time_base.numer, track_dts.time_base.denom)
            })
            .or(self.stream_time_base);
        let packet_timestamps = packet_time_base.map(|time_base| {
            let pts = duration_to_units_saturating(packet.pts, time_base);
            let dts = packet
                .track_dts
                .map(|track_dts| track_dts.units.get())
                .or_else(|| {
                    packet
                        .dts
                        .map(|dts| duration_to_units_saturating(dts, time_base))
                });

            PacketTimestamps {
                pts: Some(pts),
                dts,
                duration: None,
            }
        });

        if let Some(time_base) = packet_time_base {
            self.stream_time_base = Some(time_base);
        }

        av_packet.set_timestamps(packet_timestamps.unwrap_or_default());
        av_packet.set_keyframe(packet.keyframe);

        Ok(av_packet)
    }

    fn send_packet(&mut self, packet: &Self::Packet) -> Result<(), DecodeApiError> {
        self.codec_context
            .send_packet(packet)
            .map_err(decode_api_error_from_ffi)
    }

    fn send_end_of_stream(&mut self) -> Result<(), DecodeApiError> {
        self.codec_context
            .send_flush_packet()
            .map_err(decode_api_error_from_ffi)
    }

    fn receive_frame(&mut self) -> Result<ReceivedFrameMetadata, DecodeApiError> {
        self.receive_frame.unref();
        self.codec_context
            .receive_frame(&mut self.receive_frame)
            .map_err(decode_api_error_from_ffi)?;

        let timestamps = self.receive_frame.timestamps();
        let color = color_metadata_plan_from_ffmpeg_frame(self.receive_frame.color_metadata())
            .metadata()
            .cloned();
        let frame_ref = match self.receive_frame.try_clone_ref() {
            Ok(frame_ref) => frame_ref,
            Err(error) => {
                self.receive_frame.unref();
                return Err(DecodeApiError::Fatal(FfmpegDecoderThreadError::from(error)));
            }
        };
        self.receive_frame.unref();

        Ok(ReceivedFrameMetadata {
            timestamps,
            frame_ref: Some(frame_ref),
            color,
        })
    }

    fn flush_buffers(&mut self) -> Result<(), FfmpegDecoderThreadError> {
        self.codec_context.flush_buffers();
        self.receive_frame.unref();
        Ok(())
    }
}

#[cfg(feature = "ffmpeg")]
fn video_decode_requirement_from_stream_config(
    config: &VideoStreamDecodeConfig,
) -> VideoDecodeRequirement {
    let mut requirement = VideoDecodeRequirement::new(config.codec);

    if let Some(profile) = config.profile {
        requirement = requirement.with_profile(profile);
    }

    if let Some(bit_depth) = config.bit_depth {
        requirement = requirement.with_bit_depth(bit_depth);
    }

    if let Some(chroma) = config.chroma {
        requirement = requirement.with_chroma(chroma);
    }

    if let (Some(width), Some(height)) = (config.coded_width, config.coded_height) {
        requirement = requirement.with_resolution(width, height);
    }

    requirement
}

/// Testable state machine around FFmpeg send/receive semantics.
#[cfg(any(test, feature = "ffmpeg"))]
struct SendReceiveDecodeLoop<A: SendReceiveCodecApi> {
    /// Concrete or fake API implementation.
    codec_api: A,

    /// PTS policy state kept outside FFmpeg raw structs.
    pts_resolver: FramePtsResolver,

    /// Last generation whose packet was accepted by the decoder.
    current_generation: Option<u64>,

    /// Last stream/context color metadata accepted with a packet.
    current_context_color: Option<VideoColorMetadata>,

    /// Shared EOF/DPB drain state.
    eof_drain_state: Arc<Mutex<VideoDecoderEndOfStreamDrainState>>,

    /// Activity notifier for frame/packet/drain progress.
    activity_notifier: VideoDecoderActivityNotifier,

    /// Completed packet counter for focused tests.
    completed_packet_count: usize,
}

#[cfg(any(test, feature = "ffmpeg"))]
impl<A: SendReceiveCodecApi> SendReceiveDecodeLoop<A> {
    fn new(
        codec_api: A,
        activity_notifier: VideoDecoderActivityNotifier,
        eof_drain_state: Arc<Mutex<VideoDecoderEndOfStreamDrainState>>,
    ) -> Self {
        Self {
            codec_api,
            pts_resolver: FramePtsResolver::default(),
            current_generation: None,
            current_context_color: None,
            eof_drain_state,
            activity_notifier,
            completed_packet_count: 0,
        }
    }

    fn send_packet(
        &mut self,
        packet: DecodePacket,
    ) -> Result<DecodeProgressReport, FfmpegDecoderThreadError> {
        let prepared_packet = self.codec_api.create_packet(&packet)?;
        let mut progress_report = DecodeProgressReport::default();
        let mut eagain_without_progress_count = 0usize;

        loop {
            match self.codec_api.send_packet(&prepared_packet) {
                Ok(()) => {
                    self.current_generation = Some(packet.generation);
                    self.current_context_color = packet.resolved_color.clone();
                    self.pts_resolver.observe_accepted_packet(&packet);
                    let drain_report = self.receive_until_blocked(
                        packet.generation,
                        Some(packet.pts),
                        self.current_context_color.clone(),
                    )?;
                    progress_report.extend(drain_report);
                    progress_report.packet_completed = true;
                    self.completed_packet_count = self.completed_packet_count.saturating_add(1);
                    let _ = self.activity_notifier.notify_activity();
                    return Ok(progress_report);
                }
                Err(DecodeApiError::Again) => {
                    let drain_generation = self.current_generation.unwrap_or(packet.generation);
                    let drain_report = self.receive_until_blocked(
                        drain_generation,
                        None,
                        self.current_context_color.clone(),
                    )?;
                    let made_progress = drain_report.made_progress();
                    progress_report.extend(drain_report);

                    if !made_progress {
                        eagain_without_progress_count =
                            eagain_without_progress_count.saturating_add(1);
                        if eagain_without_progress_count > 1 {
                            return Err(FfmpegDecoderThreadError::ProtocolViolation {
                                reason: "avcodec_send_packet returned EAGAIN twice without receive-side progress".to_owned(),
                            });
                        }
                    } else {
                        eagain_without_progress_count = 0;
                    }
                }
                Err(DecodeApiError::EndOfFile) => {
                    return Err(FfmpegDecoderThreadError::ProtocolViolation {
                        reason: "avcodec_send_packet returned EOF for a normal packet; decoder must be flushed before reuse".to_owned(),
                    });
                }
                Err(DecodeApiError::Fatal(error)) => return Err(error),
            }
        }
    }

    fn flush_for_seek(&mut self) -> Result<(), FfmpegDecoderThreadError> {
        self.codec_api.flush_buffers()?;
        self.pts_resolver = FramePtsResolver::default();
        self.current_generation = None;
        self.current_context_color = None;
        set_eof_drain_state(
            &self.eof_drain_state,
            VideoDecoderEndOfStreamDrainState::Idle,
            &self.activity_notifier,
        )?;
        let _ = self.activity_notifier.notify_activity();
        Ok(())
    }

    fn begin_end_of_stream_drain(
        &mut self,
        generation: u64,
    ) -> Result<EofDrainProgressReport, FfmpegDecoderThreadError> {
        let mut eagain_without_progress_count = 0usize;
        let mut frames = Vec::new();

        loop {
            match self.codec_api.send_end_of_stream() {
                Ok(()) => {
                    set_eof_drain_state(
                        &self.eof_drain_state,
                        VideoDecoderEndOfStreamDrainState::Draining { generation },
                        &self.activity_notifier,
                    )?;
                    let drain_report = self.receive_until_blocked(
                        generation,
                        None,
                        self.current_context_color.clone(),
                    )?;
                    let stop_reason = drain_report.stop_reason;
                    frames.extend(drain_report.frames);

                    let state = match stop_reason {
                        ReceiveStopReason::EndOfFile => {
                            VideoDecoderEndOfStreamDrainState::Drained { generation }
                        }
                        ReceiveStopReason::NeedMoreInput => {
                            VideoDecoderEndOfStreamDrainState::Draining { generation }
                        }
                    };

                    return Ok(EofDrainProgressReport { state, frames });
                }
                Err(DecodeApiError::Again) => {
                    let drain_generation = self.current_generation.unwrap_or(generation);
                    let drain_report = self.receive_until_blocked(
                        drain_generation,
                        None,
                        self.current_context_color.clone(),
                    )?;
                    let made_progress = drain_report.made_progress();
                    frames.extend(drain_report.frames);

                    if !made_progress {
                        eagain_without_progress_count =
                            eagain_without_progress_count.saturating_add(1);
                        if eagain_without_progress_count > 1 {
                            return Err(FfmpegDecoderThreadError::ProtocolViolation {
                                reason: "avcodec_send_packet(NULL) returned EAGAIN twice without receive-side progress".to_owned(),
                            });
                        }
                    } else {
                        eagain_without_progress_count = 0;
                    }
                }
                Err(DecodeApiError::EndOfFile) => {
                    let drained = VideoDecoderEndOfStreamDrainState::Drained { generation };
                    set_eof_drain_state(
                        &self.eof_drain_state,
                        drained.clone(),
                        &self.activity_notifier,
                    )?;
                    return Ok(EofDrainProgressReport {
                        state: drained,
                        frames,
                    });
                }
                Err(DecodeApiError::Fatal(error)) => return Err(error),
            }
        }
    }

    fn end_of_stream_drain_state(&self) -> VideoDecoderEndOfStreamDrainState {
        self.eof_drain_state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|_| VideoDecoderEndOfStreamDrainState::Fatal {
                generation: None,
                error: DecodeThreadError::new("FFmpeg EOF drain state lock is poisoned"),
            })
    }

    fn receive_until_blocked(
        &mut self,
        generation: u64,
        packet_pts_seed: Option<Duration>,
        context_color: Option<VideoColorMetadata>,
    ) -> Result<ReceiveDrainReport, FfmpegDecoderThreadError> {
        let mut drain_report = ReceiveDrainReport::default();

        loop {
            match self.codec_api.receive_frame() {
                Ok(frame_metadata) => {
                    let pts = self
                        .pts_resolver
                        .resolve_frame_pts(frame_metadata.timestamps, packet_pts_seed);
                    let color = frame_color_or_context_color(&frame_metadata, &context_color);
                    drain_report.frames.push(DecodedFrameRecord {
                        generation,
                        pts,
                        #[cfg(feature = "ffmpeg")]
                        frame_ref: frame_metadata.frame_ref,
                        color,
                    });
                    let _ = self.activity_notifier.notify_activity();
                }
                Err(DecodeApiError::Again) => {
                    drain_report.stop_reason = ReceiveStopReason::NeedMoreInput;
                    return Ok(drain_report);
                }
                Err(DecodeApiError::EndOfFile) => {
                    let drained = VideoDecoderEndOfStreamDrainState::Drained { generation };
                    set_eof_drain_state(&self.eof_drain_state, drained, &self.activity_notifier)?;
                    drain_report.stop_reason = ReceiveStopReason::EndOfFile;
                    return Ok(drain_report);
                }
                Err(DecodeApiError::Fatal(error)) => return Err(error),
            }
        }
    }
}

/// Minimal API surface needed by the send/receive state machine.
#[cfg(any(test, feature = "ffmpeg"))]
trait SendReceiveCodecApi {
    /// Concrete packet owner type. Real implementation uses `OwnedAvPacket`.
    type Packet;

    /// Creates a padded input packet for one `DecodePacket`.
    fn create_packet(
        &mut self,
        packet: &DecodePacket,
    ) -> Result<Self::Packet, FfmpegDecoderThreadError>;

    /// Calls the FFmpeg-equivalent send-packet operation.
    fn send_packet(&mut self, packet: &Self::Packet) -> Result<(), DecodeApiError>;

    /// Calls the FFmpeg-equivalent NULL-packet EOF drain operation.
    fn send_end_of_stream(&mut self) -> Result<(), DecodeApiError>;

    /// Calls the FFmpeg-equivalent receive-frame operation.
    fn receive_frame(&mut self) -> Result<ReceivedFrameMetadata, DecodeApiError>;

    /// Clears decoder buffers for seek/lifecycle reset.
    fn flush_buffers(&mut self) -> Result<(), FfmpegDecoderThreadError>;
}

/// Normalized receive result used by tests and the real FFmpeg adapter.
#[derive(Debug)]
#[cfg(any(test, feature = "ffmpeg"))]
struct ReceivedFrameMetadata {
    /// Timestamp fields copied from the received frame.
    timestamps: FrameTimestamps,

    /// Refcounted frame reference kept alive for provider-owned HostPlanar access.
    #[cfg(feature = "ffmpeg")]
    frame_ref: Option<OwnedAvFrame>,

    /// Frame-level color metadata normalized from FFmpeg fields when available.
    #[cfg(feature = "ffmpeg")]
    color: Option<VideoColorMetadata>,
}

#[cfg(any(test, feature = "ffmpeg"))]
fn frame_color_or_context_color(
    frame_metadata: &ReceivedFrameMetadata,
    context_color: &Option<VideoColorMetadata>,
) -> Option<VideoColorMetadata> {
    #[cfg(feature = "ffmpeg")]
    {
        merge_frame_color_with_context_color(frame_metadata.color.clone(), context_color)
    }

    #[cfg(not(feature = "ffmpeg"))]
    {
        let _frame_metadata = frame_metadata;
        context_color.as_ref().cloned()
    }
}

#[cfg(any(test, feature = "ffmpeg"))]
fn merge_frame_color_with_context_color(
    frame_color: Option<VideoColorMetadata>,
    context_color: &Option<VideoColorMetadata>,
) -> Option<VideoColorMetadata> {
    match (frame_color, context_color.as_ref()) {
        (Some(frame_color), Some(context_color)) if color_core_is_unknown(&frame_color) => {
            let mut merged_color = context_color.clone();
            merged_color.hdr_metadata =
                align_hdr_metadata_to_color(frame_color.hdr_metadata, &merged_color)
                    .or(merged_color.hdr_metadata);
            Some(merged_color)
        }
        (Some(mut frame_color), Some(context_color)) => {
            fill_unknown_core_color_fields(&mut frame_color, context_color);
            let frame_hdr_metadata = frame_color.hdr_metadata.take();
            frame_color.hdr_metadata =
                align_hdr_metadata_to_color(frame_hdr_metadata, &frame_color)
                    .or_else(|| context_color.hdr_metadata.clone());
            Some(frame_color)
        }
        (Some(frame_color), None) => Some(frame_color),
        (None, Some(context_color)) => Some(context_color.clone()),
        (None, None) => None,
    }
}

#[cfg(any(test, feature = "ffmpeg"))]
fn color_core_is_unknown(color: &VideoColorMetadata) -> bool {
    color.range == ColorRange::Unknown
        && color.matrix == MatrixCoefficients::Unknown
        && color.primaries == ColorPrimaries::Unknown
        && color.transfer == TransferFunction::Unknown
}

#[cfg(any(test, feature = "ffmpeg"))]
fn fill_unknown_core_color_fields(
    frame_color: &mut VideoColorMetadata,
    context_color: &VideoColorMetadata,
) {
    if frame_color.range == ColorRange::Unknown {
        frame_color.range = context_color.range;
    }
    if frame_color.matrix == MatrixCoefficients::Unknown {
        frame_color.matrix = context_color.matrix;
    }
    if frame_color.primaries == ColorPrimaries::Unknown {
        frame_color.primaries = context_color.primaries;
    }
    if frame_color.transfer == TransferFunction::Unknown {
        frame_color.transfer = context_color.transfer;
    }
}

#[cfg(any(test, feature = "ffmpeg"))]
fn align_hdr_metadata_to_color(
    hdr_metadata: Option<HdrMetadata>,
    color: &VideoColorMetadata,
) -> Option<HdrMetadata> {
    hdr_metadata.map(|mut hdr_metadata| {
        if hdr_metadata.color_primaries == ColorPrimaries::Unknown {
            hdr_metadata.color_primaries = color.primaries;
        }
        if hdr_metadata.transfer_function == TransferFunction::Unknown {
            hdr_metadata.transfer_function = color.transfer;
        }
        hdr_metadata
    })
}

/// Internal send/receive status preserving EAGAIN vs EOF.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(any(test, feature = "ffmpeg"))]
enum DecodeApiError {
    /// Caller must receive output and retry the same input packet.
    Again,

    /// Decoder is drained and cannot accept more normal packets before flush.
    EndOfFile,

    /// Fatal backend error.
    Fatal(FfmpegDecoderThreadError),
}

/// Metadata report for decoded frames observed by the state machine.
#[derive(Debug)]
#[cfg(any(test, feature = "ffmpeg"))]
struct DecodedFrameRecord {
    /// Seek generation assigned by the decode loop.
    generation: u64,

    /// Resolved presentation timestamp.
    pts: Duration,

    /// Provider-owned AVFrame reference, present only in real FFmpeg builds.
    #[cfg(feature = "ffmpeg")]
    frame_ref: Option<OwnedAvFrame>,

    /// Frame-level color metadata or packet/context fallback metadata.
    color: Option<VideoColorMetadata>,
}

/// Report returned after send/drain progress.
#[derive(Debug)]
#[cfg(any(test, feature = "ffmpeg"))]
struct DecodeProgressReport {
    /// Frames produced while processing the operation.
    frames: Vec<DecodedFrameRecord>,

    /// True only after FFmpeg accepted the input packet.
    packet_completed: bool,

    /// Why receive loop stopped.
    stop_reason: ReceiveStopReason,
}

#[cfg(any(test, feature = "ffmpeg"))]
impl Default for DecodeProgressReport {
    fn default() -> Self {
        Self {
            frames: Vec::new(),
            packet_completed: false,
            stop_reason: ReceiveStopReason::NeedMoreInput,
        }
    }
}

#[cfg(any(test, feature = "ffmpeg"))]
impl DecodeProgressReport {
    fn extend(&mut self, drain_report: ReceiveDrainReport) {
        self.frames.extend(drain_report.frames);
        self.stop_reason = drain_report.stop_reason;
    }
}

/// Report returned by one receive loop.
#[derive(Debug)]
#[cfg(any(test, feature = "ffmpeg"))]
struct ReceiveDrainReport {
    /// Frames received before EAGAIN/EOF/fatal.
    frames: Vec<DecodedFrameRecord>,

    /// Non-fatal receive-loop stop reason.
    stop_reason: ReceiveStopReason,
}

/// Report returned after explicit EOF/DPB drain.
#[derive(Debug)]
#[cfg(any(test, feature = "ffmpeg"))]
struct EofDrainProgressReport {
    /// Publicly visible drain lifecycle state.
    state: VideoDecoderEndOfStreamDrainState,

    /// Tail frames produced while draining decoder buffers.
    frames: Vec<DecodedFrameRecord>,
}

#[cfg(any(test, feature = "ffmpeg"))]
impl Default for ReceiveDrainReport {
    fn default() -> Self {
        Self {
            frames: Vec::new(),
            stop_reason: ReceiveStopReason::NeedMoreInput,
        }
    }
}

#[cfg(any(test, feature = "ffmpeg"))]
impl ReceiveDrainReport {
    fn made_progress(&self) -> bool {
        !self.frames.is_empty() || self.stop_reason == ReceiveStopReason::EndOfFile
    }
}

/// Non-fatal reason why receive loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(any(test, feature = "ffmpeg"))]
enum ReceiveStopReason {
    /// FFmpeg returned EAGAIN; caller may send more input.
    NeedMoreInput,

    /// FFmpeg returned EOF; drain is complete.
    EndOfFile,
}

/// PTS resolver policy: best_effort -> pts -> interpolation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg(any(test, feature = "ffmpeg"))]
struct FramePtsResolver {
    /// Last known stream time base.
    time_base: Option<StreamTimeBase>,

    /// Next timestamp predicted from the previous frame duration.
    next_interpolated_pts: Option<Duration>,

    /// Last non-zero frame duration seen in frame metadata.
    last_frame_duration: Option<Duration>,
}

#[cfg(any(test, feature = "ffmpeg"))]
impl FramePtsResolver {
    fn observe_accepted_packet(&mut self, packet: &DecodePacket) {
        if let Some(track_dts) = packet.track_dts {
            self.time_base = Some(StreamTimeBase::new(
                track_dts.time_base.numer,
                track_dts.time_base.denom,
            ));
        }

        if self.next_interpolated_pts.is_none() {
            self.next_interpolated_pts = Some(packet.pts);
        }
    }

    fn resolve_frame_pts(
        &mut self,
        timestamps: FrameTimestamps,
        packet_pts_seed: Option<Duration>,
    ) -> Duration {
        let explicit_pts = self
            .timestamp_units_to_duration(timestamps.best_effort_timestamp)
            .or_else(|| self.timestamp_units_to_duration(timestamps.pts));
        let frame_duration = self.frame_duration(timestamps.duration);
        let resolved_pts = explicit_pts
            .or(self.next_interpolated_pts)
            .or(packet_pts_seed)
            .unwrap_or(Duration::ZERO);

        if let Some(frame_duration) = frame_duration {
            self.last_frame_duration = Some(frame_duration);
        }

        if let Some(duration_step) = frame_duration.or(self.last_frame_duration) {
            self.next_interpolated_pts = Some(resolved_pts.saturating_add(duration_step));
        } else {
            self.next_interpolated_pts = Some(resolved_pts);
        }

        resolved_pts
    }

    fn timestamp_units_to_duration(self, units: i64) -> Option<Duration> {
        if units == NO_TIMESTAMP {
            return None;
        }

        let time_base = self.time_base?;

        Some(units_to_duration_saturating(units, time_base))
    }

    fn frame_duration(self, units: i64) -> Option<Duration> {
        if units <= 0 {
            return None;
        }

        let time_base = self.time_base?;

        Some(units_to_duration_saturating(units, time_base))
    }
}

/// Compact copy of media-core time base fields, avoiding a new public dependency leak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(any(test, feature = "ffmpeg"))]
struct StreamTimeBase {
    /// Numerator from container track time base.
    numer: u32,

    /// Denominator from container track time base.
    denom: u32,
}

#[cfg(any(test, feature = "ffmpeg"))]
impl StreamTimeBase {
    fn new(numer: u32, denom: u32) -> Self {
        Self { numer, denom }
    }
}

#[cfg(any(test, feature = "ffmpeg"))]
fn units_to_duration_saturating(units: i64, time_base: StreamTimeBase) -> Duration {
    if units <= 0 || time_base.denom == 0 {
        return Duration::ZERO;
    }

    let total_nanoseconds = (units as u128)
        .saturating_mul(u128::from(time_base.numer))
        .saturating_mul(1_000_000_000)
        / u128::from(time_base.denom);
    let clamped_nanoseconds = total_nanoseconds.min(u128::from(u64::MAX));

    Duration::from_nanos(clamped_nanoseconds as u64)
}

#[cfg(feature = "ffmpeg")]
fn duration_to_units_saturating(duration: Duration, time_base: StreamTimeBase) -> i64 {
    if time_base.numer == 0 {
        return 0;
    }

    let duration_nanoseconds = u128::from(duration.as_secs())
        .saturating_mul(1_000_000_000)
        .saturating_add(u128::from(duration.subsec_nanos()));
    let units = duration_nanoseconds.saturating_mul(u128::from(time_base.denom))
        / u128::from(time_base.numer)
        / 1_000_000_000;

    if units > i64::MAX as u128 {
        i64::MAX
    } else {
        units as i64
    }
}

#[cfg(feature = "ffmpeg")]
fn decode_api_error_from_ffi(error: FfmpegError) -> DecodeApiError {
    match error.status_kind() {
        Some(FfmpegErrorKind::Again) => DecodeApiError::Again,
        Some(FfmpegErrorKind::EndOfFile) => DecodeApiError::EndOfFile,
        _ => DecodeApiError::Fatal(FfmpegDecoderThreadError::from(error)),
    }
}

#[cfg(feature = "ffmpeg")]
fn decode_thread_error_from_ffmpeg(error: FfmpegDecoderThreadError) -> DecodeThreadError {
    DecodeThreadError::new(error.to_string())
}

#[cfg(any(test, feature = "ffmpeg"))]
fn set_eof_drain_state(
    state: &Arc<Mutex<VideoDecoderEndOfStreamDrainState>>,
    next_state: VideoDecoderEndOfStreamDrainState,
    activity_notifier: &VideoDecoderActivityNotifier,
) -> Result<(), FfmpegDecoderThreadError> {
    let mut guard = state
        .lock()
        .map_err(|_| FfmpegDecoderThreadError::ProtocolViolation {
            reason: "EOF drain state lock is poisoned".to_owned(),
        })?;

    *guard = next_state;
    let _ = activity_notifier.notify_activity();

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use bytes::Bytes;
    use codec_core::{
        ColorPrimaries, ColorRange, HdrMetadata, MatrixCoefficients, TransferFunction,
        VideoColorMetadata,
    };
    use media_core::{TimeBase, TrackId, TrackTimestamp};
    use video_core::VideoDecoderActivityWaitOutcome;

    use super::*;

    #[test]
    fn start_decoder_thread_reports_feature_disabled_without_ffmpeg() {
        if cfg!(feature = "ffmpeg") {
            return;
        }

        let error = start_decoder_thread(FfmpegDecoderThreadConfig::default())
            .err()
            .expect("default build has no FFmpeg FFI");

        assert_eq!(error, FfmpegDecoderThreadError::FeatureDisabled);
    }

    #[test]
    fn send_packet_retries_same_padded_packet_after_eagain() {
        let mut fake_api = ScriptedDecodeApi::default()
            .with_send_results([FakeSendResult::Again, FakeSendResult::Accepted])
            .with_receive_results([
                FakeReceiveResult::Frame(frame_timestamps(10, 11, 1)),
                FakeReceiveResult::Again,
                FakeReceiveResult::Again,
            ]);
        let (activity_notifier, activity_subscription) = VideoDecoderActivityNotifier::new();
        let mut decode_loop = SendReceiveDecodeLoop::new(
            fake_api.clone(),
            activity_notifier,
            shared_idle_drain_state(),
        );
        let observed_epoch = activity_subscription.current_epoch();
        let progress = decode_loop
            .send_packet(decode_packet_with_pts(1, 100, Duration::from_millis(100)))
            .expect("EAGAIN drain should retry the same packet");

        fake_api = decode_loop.codec_api;

        assert_eq!(fake_api.created_packets.len(), 1);
        assert_eq!(fake_api.sent_packet_ids, vec![1, 1]);
        assert_eq!(fake_api.created_packets[0].payload(), &[1, 2, 3, 1]);
        assert!(
            fake_api.created_packets[0].padded_bytes()[4..]
                .iter()
                .all(|padding_byte| *padding_byte == 0)
        );
        assert!(progress.packet_completed);
        assert_eq!(progress.frames.len(), 1);
        assert_eq!(decode_loop.completed_packet_count, 1);
        assert!(matches!(
            activity_subscription.wait_for_activity_after(observed_epoch, Duration::from_millis(0)),
            VideoDecoderActivityWaitOutcome::ActivityReceived { .. }
                | VideoDecoderActivityWaitOutcome::NoNewActivityAfterEpoch { .. }
        ));
    }

    #[test]
    fn receive_loop_allows_zero_one_or_many_frames_per_packet() {
        let mut zero_frame_loop = fake_loop([FakeSendResult::Accepted], [FakeReceiveResult::Again]);
        let zero_frame_progress = zero_frame_loop
            .send_packet(decode_packet_with_pts(1, 0, Duration::ZERO))
            .expect("zero-frame packet should complete");
        assert!(zero_frame_progress.frames.is_empty());

        let mut one_frame_loop = fake_loop(
            [FakeSendResult::Accepted],
            [
                FakeReceiveResult::Frame(frame_timestamps(1, NO_TIMESTAMP, 1)),
                FakeReceiveResult::Again,
            ],
        );
        let one_frame_progress = one_frame_loop
            .send_packet(decode_packet_with_pts(1, 0, Duration::ZERO))
            .expect("one-frame packet should complete");
        assert_eq!(one_frame_progress.frames.len(), 1);

        let mut many_frame_loop = fake_loop(
            [FakeSendResult::Accepted],
            [
                FakeReceiveResult::Frame(frame_timestamps(1, NO_TIMESTAMP, 1)),
                FakeReceiveResult::Frame(frame_timestamps(2, NO_TIMESTAMP, 1)),
                FakeReceiveResult::Frame(frame_timestamps(3, NO_TIMESTAMP, 1)),
                FakeReceiveResult::Again,
            ],
        );
        let many_frame_progress = many_frame_loop
            .send_packet(decode_packet_with_pts(1, 0, Duration::ZERO))
            .expect("multi-frame packet should complete");
        assert_eq!(many_frame_progress.frames.len(), 3);
    }

    #[test]
    fn receive_loop_uses_packet_color_when_frame_metadata_is_missing() {
        let expected_context_color = VideoColorMetadata::container(
            ColorRange::Full,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            TransferFunction::Hlg,
            None,
        );
        let mut packet = decode_packet_with_pts(1, 0, Duration::ZERO);
        packet.resolved_color = Some(expected_context_color.clone());
        let mut decode_loop = fake_loop(
            [FakeSendResult::Accepted],
            [
                FakeReceiveResult::Frame(frame_timestamps(1, NO_TIMESTAMP, 1)),
                FakeReceiveResult::Again,
            ],
        );

        let progress = decode_loop
            .send_packet(packet)
            .expect("packet color should be copied into decoded frame record");

        assert_eq!(progress.frames.len(), 1);
        assert_eq!(progress.frames[0].color, Some(expected_context_color));
    }

    #[test]
    fn frame_hdr_side_data_merges_with_packet_core_colorimetry() {
        let context_color = VideoColorMetadata::container(
            ColorRange::Limited,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            TransferFunction::Pq,
            None,
        );
        let mut frame_side_data_color = VideoColorMetadata::bitstream(
            ColorRange::Unknown,
            MatrixCoefficients::Unknown,
            ColorPrimaries::Unknown,
            TransferFunction::Unknown,
        );
        frame_side_data_color.hdr_metadata = Some(HdrMetadata {
            color_primaries: ColorPrimaries::Unknown,
            transfer_function: TransferFunction::Unknown,
            max_luminance_nits: Some(1_000.0),
            min_luminance_nits: Some(0.005),
            max_content_light_level_nits: Some(1_000),
            max_frame_average_light_level_nits: Some(400),
        });

        let merged_color = merge_frame_color_with_context_color(
            Some(frame_side_data_color),
            &Some(context_color.clone()),
        )
        .expect("frame side data and packet color should merge");
        let hdr_metadata = merged_color
            .hdr_metadata
            .as_ref()
            .expect("HDR side data should be preserved");

        assert_eq!(merged_color.range, context_color.range);
        assert_eq!(merged_color.matrix, context_color.matrix);
        assert_eq!(merged_color.primaries, context_color.primaries);
        assert_eq!(merged_color.transfer, context_color.transfer);
        assert_eq!(merged_color.origin, context_color.origin);
        assert_eq!(merged_color.confidence, context_color.confidence);
        assert_eq!(hdr_metadata.color_primaries, ColorPrimaries::Bt2020);
        assert_eq!(hdr_metadata.transfer_function, TransferFunction::Pq);
        assert_eq!(hdr_metadata.max_luminance_nits, Some(1_000.0));
        assert!(merged_color.requires_hdr_processing());
    }

    #[test]
    fn flush_and_eof_drain_have_distinct_lifecycle_effects() {
        let mut decode_loop = fake_loop([FakeSendResult::Accepted], [FakeReceiveResult::Again]);
        decode_loop
            .send_packet(decode_packet_with_pts(7, 5, Duration::from_millis(5)))
            .expect("packet should seed generation");

        assert_eq!(decode_loop.current_generation, Some(7));

        decode_loop
            .flush_for_seek()
            .expect("seek flush should clear state");

        assert_eq!(decode_loop.current_generation, None);
        assert_eq!(
            decode_loop.end_of_stream_drain_state(),
            VideoDecoderEndOfStreamDrainState::Idle
        );
        assert_eq!(decode_loop.codec_api.flush_buffers_count, 1);

        decode_loop
            .codec_api
            .push_send_results([FakeSendResult::Accepted]);
        decode_loop.codec_api.push_receive_results([
            FakeReceiveResult::Frame(frame_timestamps(NO_TIMESTAMP, NO_TIMESTAMP, 4)),
            FakeReceiveResult::EndOfFile,
        ]);

        let drain_report = decode_loop
            .begin_end_of_stream_drain(9)
            .expect("EOF drain should send NULL packet and drain tail frames");

        assert_eq!(decode_loop.codec_api.flush_buffers_count, 1);
        assert_eq!(decode_loop.codec_api.end_of_stream_send_count, 1);
        assert_eq!(drain_report.frames.len(), 1);
        assert_eq!(
            drain_report.state,
            VideoDecoderEndOfStreamDrainState::Drained { generation: 9 }
        );
    }

    #[test]
    fn eof_from_normal_packet_send_is_protocol_violation() {
        let mut decode_loop = fake_loop([FakeSendResult::EndOfFile], [FakeReceiveResult::Again]);
        let error = decode_loop
            .send_packet(decode_packet_with_pts(1, 0, Duration::ZERO))
            .expect_err("normal packet send must not be treated as EOF drain");

        assert!(matches!(
            error,
            FfmpegDecoderThreadError::ProtocolViolation { .. }
        ));
        assert_eq!(decode_loop.completed_packet_count, 0);
    }

    #[test]
    fn fatal_receive_error_propagates_without_packet_completion() {
        let mut decode_loop = fake_loop(
            [FakeSendResult::Accepted],
            [FakeReceiveResult::Fatal("fake receive failed")],
        );
        let error = decode_loop
            .send_packet(decode_packet_with_pts(1, 0, Duration::ZERO))
            .expect_err("fatal receive should stop the decode loop");

        assert_eq!(
            error,
            FfmpegDecoderThreadError::ProtocolViolation {
                reason: "fake receive failed".to_owned()
            }
        );
        assert_eq!(decode_loop.completed_packet_count, 0);
    }

    #[test]
    fn pts_policy_uses_best_effort_then_pts_then_interpolation() {
        let mut decode_loop = fake_loop(
            [
                FakeSendResult::Accepted,
                FakeSendResult::Accepted,
                FakeSendResult::Accepted,
            ],
            [
                FakeReceiveResult::Frame(frame_timestamps(5, 6, 2)),
                FakeReceiveResult::Again,
                FakeReceiveResult::Frame(frame_timestamps(NO_TIMESTAMP, 8, 2)),
                FakeReceiveResult::Again,
                FakeReceiveResult::Frame(frame_timestamps(NO_TIMESTAMP, NO_TIMESTAMP, 2)),
                FakeReceiveResult::Again,
            ],
        );

        let first = decode_loop
            .send_packet(decode_packet_with_pts(1, 0, Duration::ZERO))
            .expect("best effort frame should decode");
        let second = decode_loop
            .send_packet(decode_packet_with_pts(1, 1, Duration::from_millis(1)))
            .expect("pts fallback frame should decode");
        let third = decode_loop
            .send_packet(decode_packet_with_pts(1, 2, Duration::from_millis(2)))
            .expect("interpolated frame should decode");

        assert_eq!(first.frames[0].pts, Duration::from_millis(5));
        assert_eq!(first.frames[0].generation, 1);
        assert_eq!(second.frames[0].pts, Duration::from_millis(8));
        assert_eq!(third.frames[0].pts, Duration::from_millis(10));
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn avframe_resource_remains_readable_until_release() {
        let provider = FfmpegHostResourceProvider::new(4);
        let mut frame = test_yuv420_frame(2, 2, 32);
        frame
            .write_test_plane_row(0, 0, &[10, 11])
            .expect("Y row 0 should be writable");
        frame
            .write_test_plane_row(0, 1, &[20, 21])
            .expect("Y row 1 should be writable");

        let publication = provider
            .insert_frame(
                3,
                frame,
                host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
            )
            .expect("valid AVFrame should become a host resource");
        let descriptor = lookup_host_planar_descriptor(&provider, publication.handle);

        assert_eq!(
            descriptor
                .visible_plane_row_bytes(0, 1)
                .expect("Y row remains readable"),
            &[20, 21]
        );
        assert!(matches!(
            provider.resource_lookup(publication.handle),
            PresentFrameResourceProviderLookup::Ready { .. }
        ));

        provider.release_frame(publication.handle);

        assert!(matches!(
            provider.resource_lookup(publication.handle),
            PresentFrameResourceProviderLookup::Missing { .. }
        ));
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn release_drops_resource_entry_and_stale_release_is_noop() {
        let provider = FfmpegHostResourceProvider::new(2);
        let frame = test_yuv420_frame(2, 2, 32);
        let publication = provider
            .insert_frame(
                1,
                frame,
                host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
            )
            .expect("valid AVFrame should be inserted");

        provider.release_frame(publication.handle);
        provider.release_frame(publication.handle);

        assert!(matches!(
            provider.resource_descriptor_lookup(publication.handle),
            PresentFrameResourceDescriptorLookup::Missing { .. }
        ));
        assert_eq!(
            provider.snapshot(0).host_frames_in_flight,
            0,
            "release should remove the provider-owned entry"
        );
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn descriptor_clone_keeps_avframe_owner_without_copying_planes() {
        let provider = FfmpegHostResourceProvider::new(2);
        let mut frame = test_yuv420_frame(2, 2, 32);
        frame
            .write_test_plane_row(0, 0, &[7, 8])
            .expect("Y row should be writable");
        let publication = provider
            .insert_frame(
                1,
                frame,
                host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
            )
            .expect("valid AVFrame should be inserted");
        let descriptor = match provider.resource_descriptor_lookup(publication.handle) {
            PresentFrameResourceDescriptorLookup::Ready { descriptor, .. } => descriptor,
            other => panic!("expected ready descriptor lookup, got {other:?}"),
        };
        let cloned_descriptor = descriptor
            .try_clone_for_lookup()
            .expect("host-planar descriptor clone should not duplicate plane bytes");

        provider.release_frame(publication.handle);

        let FrameResourceDescriptor::HostPlanar(cloned_descriptor) = cloned_descriptor else {
            panic!("expected host-planar cloned descriptor");
        };
        assert_eq!(
            cloned_descriptor
                .visible_plane_row_bytes(0, 0)
                .expect("cloned descriptor keeps AVFrame owner alive"),
            &[7, 8]
        );
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn invalid_linesize_and_data_are_rejected() {
        let provider = FfmpegHostResourceProvider::new(2);
        let mut invalid_linesize_frame = test_yuv420_frame(2, 2, 32);
        invalid_linesize_frame.set_test_linesize(0, 1);

        let linesize_error = provider
            .insert_frame(
                1,
                invalid_linesize_frame,
                host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
            )
            .expect_err("visible row wider than linesize must be rejected");
        assert!(
            linesize_error
                .to_string()
                .contains("AVFrame HostPlanar descriptor validation")
        );

        let mut null_data_frame = test_yuv420_frame(2, 2, 32);
        null_data_frame.clear_test_plane_data(1);
        let data_error = provider
            .insert_frame(
                1,
                null_data_frame,
                host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
            )
            .expect_err("null AVFrame plane data must be rejected");
        assert!(
            data_error
                .to_string()
                .contains("AVFrame HostPlanar descriptor validation")
        );
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn unsupported_avframe_format_is_rejected_with_diagnostic_context() {
        let provider = FfmpegHostResourceProvider::new(2);
        let unsupported_frame = OwnedAvFrame::new_test_unsupported_nv12_frame(2, 2, 32)
            .expect("test NV12 AVFrame allocation should succeed");

        let error = provider
            .insert_frame(
                1,
                unsupported_frame,
                host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
            )
            .expect_err("NV12 must not enter the HostPlanar software resource table");

        assert!(
            error
                .to_string()
                .contains("not a supported v1 software planar YUV format")
        );
        assert_eq!(provider.snapshot(0).upload_failures, 1);
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn padded_avframe_linesize_reads_visible_bytes_and_validates_descriptor() {
        let provider = FfmpegHostResourceProvider::new(2);
        let mut frame = test_yuv420_frame(3, 3, 32);
        frame
            .write_test_plane_row(0, 0, &[1, 2, 3])
            .expect("Y row 0 should be writable");
        frame
            .write_test_plane_row(0, 2, &[9, 10, 11])
            .expect("Y row 2 should be writable");
        frame
            .write_test_plane_row(1, 1, &[21, 22])
            .expect("U row should be writable");
        frame
            .write_test_plane_row(2, 1, &[31, 32])
            .expect("V row should be writable");

        let publication = provider
            .insert_frame(
                5,
                frame,
                host_planar_contract(VideoFramePixelLayout::Yuv420Planar8),
            )
            .expect("padded AVFrame should validate");
        let descriptor = lookup_host_planar_descriptor(&provider, publication.handle);

        assert!(
            descriptor.planes[0].stride > 3,
            "FFmpeg test frame should expose row padding through linesize"
        );
        assert_eq!(
            descriptor
                .visible_plane_row_bytes(0, 0)
                .expect("visible Y row should ignore right padding"),
            &[1, 2, 3]
        );
        assert_eq!(
            descriptor
                .visible_plane_row_bytes(1, 1)
                .expect("visible U row should ignore right padding"),
            &[21, 22]
        );
    }

    #[derive(Debug, Clone)]
    struct ScriptedDecodeApi {
        next_packet_id: u64,
        created_packets: Vec<FakePacket>,
        sent_packet_ids: Vec<u64>,
        send_results: VecDeque<FakeSendResult>,
        receive_results: VecDeque<FakeReceiveResult>,
        end_of_stream_send_count: usize,
        flush_buffers_count: usize,
    }

    impl Default for ScriptedDecodeApi {
        fn default() -> Self {
            Self {
                next_packet_id: 1,
                created_packets: Vec::new(),
                sent_packet_ids: Vec::new(),
                send_results: VecDeque::new(),
                receive_results: VecDeque::new(),
                end_of_stream_send_count: 0,
                flush_buffers_count: 0,
            }
        }
    }

    impl ScriptedDecodeApi {
        fn with_send_results(
            mut self,
            send_results: impl IntoIterator<Item = FakeSendResult>,
        ) -> Self {
            self.push_send_results(send_results);
            self
        }

        fn with_receive_results(
            mut self,
            receive_results: impl IntoIterator<Item = FakeReceiveResult>,
        ) -> Self {
            self.push_receive_results(receive_results);
            self
        }

        fn push_send_results(&mut self, send_results: impl IntoIterator<Item = FakeSendResult>) {
            self.send_results.extend(send_results);
        }

        fn push_receive_results(
            &mut self,
            receive_results: impl IntoIterator<Item = FakeReceiveResult>,
        ) {
            self.receive_results.extend(receive_results);
        }
    }

    impl SendReceiveCodecApi for ScriptedDecodeApi {
        type Packet = FakePacket;

        fn create_packet(
            &mut self,
            packet: &DecodePacket,
        ) -> Result<Self::Packet, FfmpegDecoderThreadError> {
            let fake_packet = FakePacket {
                id: self.next_packet_id,
                padded_bytes: PaddedPacketBytes::new(packet.encoded_bytes.as_ref()),
            };
            self.next_packet_id = self.next_packet_id.saturating_add(1);
            self.created_packets.push(fake_packet.clone());
            Ok(fake_packet)
        }

        fn send_packet(&mut self, packet: &Self::Packet) -> Result<(), DecodeApiError> {
            self.sent_packet_ids.push(packet.id);

            match self
                .send_results
                .pop_front()
                .unwrap_or(FakeSendResult::Accepted)
            {
                FakeSendResult::Accepted => Ok(()),
                FakeSendResult::Again => Err(DecodeApiError::Again),
                FakeSendResult::EndOfFile => Err(DecodeApiError::EndOfFile),
            }
        }

        fn send_end_of_stream(&mut self) -> Result<(), DecodeApiError> {
            self.end_of_stream_send_count = self.end_of_stream_send_count.saturating_add(1);

            match self
                .send_results
                .pop_front()
                .unwrap_or(FakeSendResult::Accepted)
            {
                FakeSendResult::Accepted => Ok(()),
                FakeSendResult::Again => Err(DecodeApiError::Again),
                FakeSendResult::EndOfFile => Err(DecodeApiError::EndOfFile),
            }
        }

        fn receive_frame(&mut self) -> Result<ReceivedFrameMetadata, DecodeApiError> {
            match self
                .receive_results
                .pop_front()
                .unwrap_or(FakeReceiveResult::Again)
            {
                FakeReceiveResult::Frame(timestamps) => Ok(ReceivedFrameMetadata {
                    timestamps,
                    #[cfg(feature = "ffmpeg")]
                    frame_ref: None,
                    #[cfg(feature = "ffmpeg")]
                    color: None,
                }),
                FakeReceiveResult::Again => Err(DecodeApiError::Again),
                FakeReceiveResult::EndOfFile => Err(DecodeApiError::EndOfFile),
                FakeReceiveResult::Fatal(reason) => Err(DecodeApiError::Fatal(
                    FfmpegDecoderThreadError::ProtocolViolation {
                        reason: reason.to_owned(),
                    },
                )),
            }
        }

        fn flush_buffers(&mut self) -> Result<(), FfmpegDecoderThreadError> {
            self.flush_buffers_count = self.flush_buffers_count.saturating_add(1);
            Ok(())
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct FakePacket {
        id: u64,
        padded_bytes: PaddedPacketBytes,
    }

    impl FakePacket {
        fn payload(&self) -> &[u8] {
            self.padded_bytes.payload()
        }

        fn padded_bytes(&self) -> &[u8] {
            self.padded_bytes.padded_bytes()
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeSendResult {
        Accepted,
        Again,
        EndOfFile,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeReceiveResult {
        Frame(FrameTimestamps),
        Again,
        EndOfFile,
        Fatal(&'static str),
    }

    fn fake_loop(
        send_results: impl IntoIterator<Item = FakeSendResult>,
        receive_results: impl IntoIterator<Item = FakeReceiveResult>,
    ) -> SendReceiveDecodeLoop<ScriptedDecodeApi> {
        let fake_api = ScriptedDecodeApi::default()
            .with_send_results(send_results)
            .with_receive_results(receive_results);
        let (activity_notifier, _activity_subscription) = VideoDecoderActivityNotifier::new();

        SendReceiveDecodeLoop::new(fake_api, activity_notifier, shared_idle_drain_state())
    }

    #[cfg(feature = "ffmpeg")]
    fn host_planar_contract(
        pixel_layout: VideoFramePixelLayout,
    ) -> video_frame_contract::VideoFrameContract {
        video_frame_contract::VideoFrameContract {
            pixel_layout,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        }
    }

    #[cfg(feature = "ffmpeg")]
    fn test_yuv420_frame(width: i32, height: i32, alignment: i32) -> OwnedAvFrame {
        OwnedAvFrame::new_test_video_frame(
            SoftwarePixelFormat::Yuv420Planar8,
            width,
            height,
            alignment,
        )
        .expect("test AVFrame allocation should succeed")
    }

    #[cfg(feature = "ffmpeg")]
    fn lookup_host_planar_descriptor(
        provider: &FfmpegHostResourceProvider,
        handle: FrameResourceHandle,
    ) -> HostPlanarFrameDescriptor {
        match provider.resource_descriptor_lookup(handle) {
            PresentFrameResourceDescriptorLookup::Ready {
                descriptor: FrameResourceDescriptor::HostPlanar(descriptor),
                ..
            } => descriptor,
            other => panic!("expected ready host-planar descriptor lookup, got {other:?}"),
        }
    }

    fn shared_idle_drain_state() -> Arc<Mutex<VideoDecoderEndOfStreamDrainState>> {
        Arc::new(Mutex::new(VideoDecoderEndOfStreamDrainState::Idle))
    }

    fn frame_timestamps(best_effort_timestamp: i64, pts: i64, duration: i64) -> FrameTimestamps {
        FrameTimestamps {
            best_effort_timestamp,
            pts,
            packet_dts: NO_TIMESTAMP,
            duration,
        }
    }

    fn decode_packet_with_pts(generation: u64, dts_units: i64, pts: Duration) -> DecodePacket {
        let track_id = TrackId::new(1);

        DecodePacket {
            track_id,
            pts,
            dts: None,
            track_dts: Some(TrackTimestamp::new(
                track_id,
                dts_units,
                TimeBase::new(1, 1_000).expect("test time base is valid"),
            )),
            generation,
            encoded_bytes: Bytes::from(vec![1, 2, 3, generation as u8]),
            keyframe: true,
            resolved_color: Some(VideoColorMetadata::sdr_bt709_limited()),
        }
    }
}
