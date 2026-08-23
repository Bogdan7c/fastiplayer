//! FFmpeg send/receive decoder thread.
//!
//! Модуль держит FFmpeg decode state внутри `video-ffmpeg` и отдаёт наружу
//! только нейтральный `VideoDecoderThreadHandle`. AVFrame-backed HostPlanar
//! resource table остаётся внутренней частью этого backend-а.

#[cfg(feature = "ffmpeg")]
use crate::FFMPEG_SOFTWARE_BACKEND_ID;
use crate::ffi::error::FfmpegError;
#[cfg(test)]
use crate::ffi::frame::FrameTimestamps;
#[cfg(all(test, feature = "ffmpeg"))]
use crate::ffi::frame::OwnedAvFrame;
#[cfg(any(test, feature = "ffmpeg"))]
use codec_core::VideoColorMetadata;
#[cfg(all(test, feature = "ffmpeg"))]
use codec_core::{H264Packetization, H265Packetization, VideoDecodeRequirement};
#[cfg(feature = "ffmpeg")]
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, TrySendError, bounded};
#[cfg(feature = "ffmpeg")]
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
#[cfg(any(test, feature = "ffmpeg"))]
use std::sync::{Arc, Mutex};
use thiserror::Error;
use video_backend_api::StartedVideoBackend;
#[cfg(all(test, feature = "ffmpeg"))]
use video_backend_api::{PresentFrameResourceDescriptorLookup, PresentFrameResourceProviderLookup};
#[cfg(feature = "ffmpeg")]
use video_backend_api::{PresentFrameResourceProvider, PresentFrameResourceProviderHandle};
#[cfg(feature = "ffmpeg")]
use video_core::DecodeThreadError;
#[cfg(feature = "ffmpeg")]
use video_core::SoftwareDecodeThreadBudget;
use video_core::VideoDecoderThreadConfig;
#[cfg(feature = "ffmpeg")]
use video_core::VideoStreamConfigRejection;
#[cfg(all(test, feature = "ffmpeg"))]
use video_core::VideoStreamPacketization;
#[cfg(feature = "ffmpeg")]
use video_core::{
    DecodeBackpressureReason, DecodeSendError, DecodedFrame, FrameResourceHandle,
    HostUploadResourceSnapshotStatus, VideoDecoderActivitySnapshot,
    VideoDecoderActivitySubscription, VideoDecoderControlBackpressureReason,
    VideoDecoderControlChannelPressureSnapshot, VideoDecoderThreadHandle, VideoFrameDiagnostics,
    VideoStreamConfigResult, VideoStreamDecodeConfig,
};
#[cfg(any(test, feature = "ffmpeg"))]
use video_core::{
    DecodePacket, VideoDecoderActivityNotifier, VideoDecoderEndOfStreamDrainResult,
    VideoDecoderEndOfStreamDrainState,
};
#[cfg(all(test, feature = "ffmpeg"))]
use video_core::{FrameResourceDescriptor, HostPlanarFrameDescriptor};
#[cfg(all(test, feature = "ffmpeg"))]
use video_frame_contract::{VideoFramePixelLayout, VideoFrameTransferPath};

#[cfg(all(test, feature = "ffmpeg"))]
use crate::ffi::pixel_format::SoftwarePixelFormat;
#[cfg(test)]
use color_metadata::merge_frame_color_with_context_color;
#[cfg(feature = "ffmpeg")]
use host_resources::{FfmpegHostResourceProvider, invalid_avframe_resource};
#[cfg(feature = "ffmpeg")]
use lifecycle::FfmpegWorkerLifecycle;
#[cfg(feature = "ffmpeg")]
use send_receive::DecodedFrameRecord;
#[cfg(feature = "ffmpeg")]
use send_receive::RealFfmpegDecodeApi;
#[cfg(test)]
use send_receive::{
    DecodeApiError, DecodeProgressReport, EofDrainProgressReport, NO_TIMESTAMP, ReceiveStopReason,
    ReceivedFrameMetadata, SendReceiveCodecApi,
};
#[cfg(any(test, feature = "ffmpeg"))]
use send_receive::{SendPacketOutcome, SendReceiveDecodeLoop};
#[cfg(all(test, feature = "ffmpeg"))]
use stream_config::extradata_for_stream_config;

#[cfg(any(test, feature = "ffmpeg"))]
mod color_metadata;
#[cfg(feature = "ffmpeg")]
mod host_resources;
#[cfg(feature = "ffmpeg")]
mod lifecycle;
#[cfg(any(test, feature = "ffmpeg"))]
mod send_receive;
#[cfg(feature = "ffmpeg")]
mod stream_config;

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
        FfmpegVideoDecoderThread::spawn(config).map(|decoder_thread| {
            StartedVideoBackend::from_decoder_thread(FFMPEG_SOFTWARE_BACKEND_ID, decoder_thread)
        })
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

    /// Durable packet completions for player in-flight accounting.
    packet_completion_counter: Arc<FfmpegPacketCompletionCounter>,

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

    /// Владелец независимого shutdown signal и exactly-once worker join.
    worker_lifecycle: FfmpegWorkerLifecycle,
}

#[cfg(feature = "ffmpeg")]
impl Drop for FfmpegVideoDecoderThread {
    fn drop(&mut self) {
        // Явный вызов гарантирует shutdown/join до автоматического drop channels/resources.
        self.worker_lifecycle.shutdown_and_join();
    }
}

#[cfg(feature = "ffmpeg")]
impl FfmpegVideoDecoderThread {
    /// Spawns the concrete FFmpeg worker. The codec is opened later by `configure_stream`.
    #[cfg(feature = "ffmpeg")]
    fn spawn(config: FfmpegDecoderThreadConfig) -> Result<Self, FfmpegDecoderThreadError> {
        let thread_config = config.thread_config().normalized();
        let (packet_tx, packet_rx) = bounded(thread_config.packet_channel_frames);
        let (control_tx, control_rx) = bounded(thread_config.control_channel_frames);
        // Shutdown не делит capacity с packet/control: teardown обязан пройти
        // даже когда оба обычных protocol channel-а находятся под backpressure.
        let (shutdown_tx, shutdown_rx) = bounded(1);
        // Decoded-frame channel сделан с запасом над ready-queue backpressure
        // порогом (decoder_ready_queue_frames): один send_packet при frame
        // threading может слить burst кадров (или весь EOF/DPB tail ~thread_count
        // сразу), а player тормозит отправку только по frame_rx.len() >=
        // ready-queue. Размер по software_frame_pool_frames даёт headroom, чтобы
        // try_send не упёрся в full channel внутри одной drain-итерации.
        let (frame_tx, frame_rx) = bounded(thread_config.software_frame_pool_frames);
        let (error_tx, error_rx) = bounded(1);
        // Completion accounting не использует bounded channel: его заполнение
        // не должно ни терять ACK, ни блокировать единственный FFmpeg owner thread.
        let packet_completion_counter = Arc::new(FfmpegPacketCompletionCounter::default());
        let (activity_notifier, activity_subscription) = VideoDecoderActivityNotifier::new();
        let eof_drain_state = Arc::new(Mutex::new(VideoDecoderEndOfStreamDrainState::Idle));
        let control_pressure = Arc::new(FfmpegControlPressureCounters::default());
        // Resource table должна вмещать все одновременно живущие decoded frames,
        // а не только те, что лежат в frame channel: кадры остаются в таблице
        // после выборки из channel-а, пока consumer держит их в present queue и
        // render lease-ах, и освобождаются только через release_frame. Размер по
        // frame_channel_frames (ready-queue) переполнялся, как только быстрый
        // (теперь многопоточный) decode заполнял весь pipeline. Берём
        // software_frame_pool_frames — это software-специфичный output frame pool
        // (host-frame аналог hardware surface pool), который покрывает ready
        // channel + present queue + leases. Он отделён от hardware
        // decoder_surface_pool_frames намеренно: каждый software-кадр — полный
        // host-буфер (~12 МБ для 4K), и держать их много вредно для memory
        // bandwidth iGPU. Ready-queue backpressure продолжает считаться отдельно
        // по frame channel длине.
        // bounded(1) coalescing wake-up: release_frame пишет токен, worker будит
        // reception, как только освободился pool slot, без busy-poll.
        let (release_notify_tx, release_notify_rx) = bounded(1);
        let host_resource_provider = FfmpegHostResourceProvider::new(
            thread_config.software_frame_pool_frames,
            release_notify_tx,
        );
        let worker_activity_notifier = activity_notifier.clone();
        let worker_eof_drain_state = eof_drain_state.clone();
        let worker_resource_provider = host_resource_provider.clone();
        let worker_packet_completion_counter = packet_completion_counter.clone();
        let worker_software_decode_thread_budget = thread_config.software_decode_thread_budget;

        let worker_thread = std::thread::Builder::new()
            .name("ffmpeg-video-decoder".to_owned())
            .spawn(move || {
                // Worker и все raw FFmpeg owners создаются уже на owner thread.
                // Через thread boundary проходят только нейтральные channels,
                // handles и immutable configuration, а не codec/frame/packet state.
                let worker = FfmpegDecoderWorker {
                    active_decoder: None,
                    activity_notifier: worker_activity_notifier,
                    eof_drain_state: worker_eof_drain_state,
                    frame_tx,
                    resource_provider: worker_resource_provider,
                    release_notify_rx,
                    pending_packet: None,
                    pending_eof_drain_generation: None,
                    packet_completion_counter: worker_packet_completion_counter,
                    error_tx,
                    software_decode_thread_budget: worker_software_decode_thread_budget,
                };

                worker.run(packet_rx, control_rx, shutdown_rx);
            })
            .map_err(|error| FfmpegDecoderThreadError::ThreadSpawn {
                reason: error.to_string(),
            })?;

        Ok(Self {
            packet_tx,
            control_tx,
            frame_rx,
            error_rx,
            packet_completion_counter,
            activity_subscription,
            resource_provider: PresentFrameResourceProviderHandle::new(
                host_resource_provider.clone(),
            ),
            host_resource_provider,
            eof_drain_state,
            thread_config,
            control_pressure,
            worker_lifecycle: FfmpegWorkerLifecycle::new(shutdown_tx, worker_thread),
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
        self.packet_completion_counter.drain()
    }
}

/// Нетеряемый accumulator завершений packet work между FFmpeg worker и player.
///
/// Activity notifier остаётся только coalesced сигналом пробуждения. Истина для
/// in-flight accounting живёт здесь и поэтому не зависит от размера channel-а.
#[derive(Debug, Default)]
#[cfg(feature = "ffmpeg")]
struct FfmpegPacketCompletionCounter {
    /// Число завершений, которые player ещё не забрал через boundary drain.
    pending_count: AtomicUsize,
}

#[cfg(feature = "ffmpeg")]
impl FfmpegPacketCompletionCounter {
    /// Фиксирует exactly-once completion без блокировки decoder owner thread-а.
    fn record_completion(&self) {
        // Closure всегда возвращает Some, поэтому CAS повторяется до успешного
        // saturating increment и не может завершиться веткой Err.
        self.pending_count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current_count| {
                Some(current_count.saturating_add(1))
            })
            .expect("packet completion increment closure always returns Some");
    }

    /// Атомарно передаёт player-у все накопленные completions ровно один раз.
    fn drain(&self) -> usize {
        // Concurrent completion попадёт либо в текущий swap, либо в следующий
        // drain; потерять increment при такой гонке невозможно.
        self.pending_count.swap(0, Ordering::Relaxed)
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

    /// Wake-up consumed when the renderer releases a pool slot, used to resume
    /// reception that was paused by host-upload pool backpressure.
    release_notify_rx: Receiver<()>,

    /// Packet whose reception was deferred because the pool was full; retried
    /// once a slot frees instead of overflowing the resource table.
    pending_packet: Option<DecodePacket>,

    /// EOF generation, чей FFmpeg tail ждёт освобождения host-upload slots.
    pending_eof_drain_generation: Option<u64>,

    /// Durable packet completions for player in-flight accounting.
    packet_completion_counter: Arc<FfmpegPacketCompletionCounter>,

    /// Fatal errors surfaced through `try_recv_error`.
    error_tx: Sender<DecodeThreadError>,

    /// Software decode thread budget, применяемый при открытии FFmpeg context-а.
    software_decode_thread_budget: SoftwareDecodeThreadBudget,
}

/// Проверяет, обязан ли decoder owner немедленно сделать следующий EOF turn.
///
/// `Draining` означает незавершённый receive-side lifecycle. После container EOF
/// packet/control сообщений больше может не быть, поэтому переход к обычному
/// blocking select потерял бы уже coalesced release edge и оставил бы decoder в
/// `Draining` навсегда.
#[cfg(any(feature = "ffmpeg", test))]
fn eof_drain_result_requires_owner_reentry(
    drain_result: &VideoDecoderEndOfStreamDrainResult,
) -> bool {
    matches!(
        drain_result,
        VideoDecoderEndOfStreamDrainResult::Started(
            VideoDecoderEndOfStreamDrainState::Draining { .. }
        ) | VideoDecoderEndOfStreamDrainResult::Unchanged(
            VideoDecoderEndOfStreamDrainState::Draining { .. }
        )
    )
}

#[cfg(feature = "ffmpeg")]
impl FfmpegDecoderWorker {
    fn run(
        mut self,
        packet_rx: Receiver<DecodePacket>,
        control_rx: Receiver<FfmpegDecoderControl>,
        shutdown_rx: Receiver<()>,
    ) {
        loop {
            // Проверяем shutdown до остальных ready operations: disconnected
            // control receiver не должен даже теоретически вытеснять teardown.
            match shutdown_rx.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }

            // Host-upload pool — единственный hard-источник backpressure: пока в
            // нём нет свободных slots, мы не забираем новые packet-ы и не
            // дренируем кадры (они ждут внутри FFmpeg), иначе таблица
            // переполнится fatal-ом. Control обслуживается всегда; release pool
            // slot-а будит reception через release_notify_rx.
            if self.resource_provider.free_slots() == 0 {
                crossbeam_channel::select! {
                    recv(shutdown_rx) -> _ => break,
                    recv(control_rx) -> control_message => {
                        match control_message {
                            Ok(control) => self.handle_control(control, &packet_rx),
                            // Единственный control sender принадлежит frontend;
                            // disconnect поэтому является terminal lifecycle signal.
                            Err(_) => break,
                        }
                    }
                    recv(self.release_notify_rx) -> _ => {}
                }
                continue;
            }

            // Pool освободился: сначала дотолкаем отложенный packet, чтобы не
            // нарушить порядок и слить буферизованные внутри FFmpeg кадры.
            if let Some(packet) = self.pending_packet.take() {
                self.handle_packet(packet);
                continue;
            }

            if let Some(generation) = self.pending_eof_drain_generation {
                let drain_result = self.drive_end_of_stream_drain(generation);
                if eof_drain_result_requires_owner_reentry(&drain_result) {
                    // Не засыпаем в select только на packet/control: после EOF
                    // их больше не будет. Верхняя pool-проверка сама дождётся
                    // release, если slots закончились; уже свободные slots
                    // используются сразу, чтобы не потерять coalesced release
                    // edge перед terminal AVERROR_EOF.
                    continue;
                }
            }

            crossbeam_channel::select! {
                recv(shutdown_rx) -> _ => break,
                recv(control_rx) -> control_message => {
                    match control_message {
                        Ok(control) => self.handle_control(control, &packet_rx),
                        // Queued packets старого frontend-а не продлевают worker lifecycle.
                        Err(_) => break,
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
                // Любой отложенный packet принадлежит прошлой конфигурации.
                self.pending_packet = None;
                self.pending_eof_drain_generation = None;
                let result = self.configure_stream(config);
                let _ = reply_tx.try_send(result);
            }
            FfmpegDecoderControl::Clear { reply_tx } => {
                self.pending_packet = None;
                self.pending_eof_drain_generation = None;
                let result = self.clear_stream();
                let _ = reply_tx.try_send(result);
            }
            FfmpegDecoderControl::Flush { reply_tx } => {
                // Seek flush сбрасывает очередь packet-ов; pending packet тоже
                // относится к до-seek потоку и должен быть отброшен.
                self.pending_packet = None;
                self.pending_eof_drain_generation = None;
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

        let codec_api = match RealFfmpegDecodeApi::open(&config, self.software_decode_thread_budget)
        {
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
        let current_state = self
            .active_decoder
            .as_ref()
            .map(|decoder| decoder.decode_loop.end_of_stream_drain_state());
        if matches!(
            current_state,
            Some(VideoDecoderEndOfStreamDrainState::Draining {
                generation: active_generation,
            } | VideoDecoderEndOfStreamDrainState::Drained {
                generation: active_generation,
            }) if active_generation == generation
        ) {
            return VideoDecoderEndOfStreamDrainResult::Unchanged(
                current_state.expect("matched state is present"),
            );
        }

        self.drive_end_of_stream_drain(generation)
    }

    /// Делает один bounded receive-pass; run-loop повторит его после release notification.
    fn drive_end_of_stream_drain(&mut self, generation: u64) -> VideoDecoderEndOfStreamDrainResult {
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
                self.pending_eof_drain_generation = None;
                return VideoDecoderEndOfStreamDrainResult::Started(drained);
            };

            let receive_budget = self.resource_provider.free_slots();
            let config = active_decoder.config.clone();
            active_decoder
                .decode_loop
                .begin_end_of_stream_drain(generation, receive_budget)
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
                    self.pending_eof_drain_generation = None;
                    VideoDecoderEndOfStreamDrainResult::Fatal(thread_error)
                } else {
                    self.pending_eof_drain_generation = matches!(
                        progress_report.state,
                        VideoDecoderEndOfStreamDrainState::Draining { .. }
                    )
                    .then_some(generation);
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
                self.pending_eof_drain_generation = None;
                VideoDecoderEndOfStreamDrainResult::Fatal(thread_error)
            }
        }
    }

    fn handle_packet(&mut self, packet: DecodePacket) {
        // Receive budget = свободные pool slots: decode loop не произведёт больше
        // кадров, чем влезет в таблицу. Если pool полон до приёма packet-а,
        // packet откладывается в pending_packet и повторяется после release.
        let receive_budget = self.resource_provider.free_slots();

        let decode_result = {
            let Some(active_decoder) = self.active_decoder.as_mut() else {
                self.report_fatal_error(FfmpegDecoderThreadError::DecoderNotConfigured);
                return;
            };

            let config = active_decoder.config.clone();
            active_decoder
                .decode_loop
                .send_packet(packet, receive_budget)
                .map(|outcome| (config, outcome))
        };

        match decode_result {
            Ok((config, SendPacketOutcome::Consumed(progress_report))) => {
                if let Err(error) = self.publish_decoded_frames(&config, progress_report.frames) {
                    self.report_fatal_error(error);
                    return;
                }

                if progress_report.packet_completed {
                    self.packet_completion_counter.record_completion();
                }
                let _ = self.activity_notifier.notify_activity();
            }
            Ok((config, SendPacketOutcome::Deferred { progress, packet })) => {
                if let Err(error) = self.publish_decoded_frames(&config, progress.frames) {
                    self.report_fatal_error(error);
                    return;
                }

                self.pending_packet = Some(packet);
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
#[derive(Debug)]
enum FfmpegOpenDecoderError {
    /// Stream config is not supported by FFmpeg software policy.
    Unsupported(VideoStreamConfigRejection),

    /// FFmpeg failed while allocating/opening the decoder.
    Fatal(FfmpegDecoderThreadError),
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
mod test_support;
#[cfg(test)]
mod tests;
