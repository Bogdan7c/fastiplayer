/// Dedicated decoder thread для VA-API hardware decode.
///
/// Изолирует blocking hardware decode и DMA-BUF export от render thread.
///
/// Архитектура:
/// - Render thread отправляет video packets через `send_packet()`.
/// - Decoder thread вызывает `decode()` и публикует только neutral DMA-BUF resource handle.
/// - Готовые `DecodedFrame` возвращаются через `try_recv_frame()`.
/// - Resource pool shared между потоками: decoder thread хранит exported descriptors,
///   render thread получает duplicated fd через provider boundary.
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

#[cfg(test)]
use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError, bounded, unbounded};
use tracing::{info, trace};
use video_core::{
    DecodedFrame, VideoDecoderActivityNotifier, VideoDecoderActivitySnapshot,
    VideoDecoderActivitySubscription, VideoDecoderDiagnosticEvent,
};

#[cfg(test)]
use crate::decoder::VaapiDecodePacketOutcome;
use crate::resource_pool::ResourcePoolStats;

mod config;
mod control;
mod resource_provider;
mod runtime_loop;

use config::{
    DECODER_DIAGNOSTIC_CHANNEL_CAPACITY, DECODER_FRAME_PUBLISH_RETRY_MS, DecodePacketAck,
    DecoderDiagnosticReceiver, DecoderDiagnosticSender, DecoderThreadState,
};
pub use config::{
    DEFAULT_DECODER_FRAME_CHANNEL_FRAMES, DEFAULT_DECODER_PACKET_CHANNEL_FRAMES, DecodePacket,
    DecodeThreadBackpressureReason, DecodeThreadError, DecodeThreadSendError,
    VideoDecodeThreadConfig,
};

pub use control::VideoDecoderControlChannelPressureStats;
use control::{
    DecoderControlChannelPressureCounters, DecoderControlOperation, ThreadControlMsg,
    record_decoder_control_send_failure, wait_for_configure_stream_ack,
    wait_for_end_of_stream_drain_ack, wait_for_flush_ack, wait_for_preroll_output_floor_ack,
};
pub use resource_provider::{
    VideoFrameResourceDescriptorLookup, VideoFrameResourceLockDiagnostics,
    VideoFrameResourceLookup, VideoFrameResourceProvider,
};
#[cfg(test)]
use resource_provider::{
    resource_lookup_from_pool_started_at, try_resource_lookup_from_pool,
    try_resource_lookup_from_pool_started_at,
};
#[cfg(test)]
use runtime_loop::{
    DecodeQueuedPacketContext, DecodeQueuedPacketResult, FramePublishPressureCounters,
    PendingFramePublish, drain_queued_decode_packets, handle_decode_packet_outcome,
    publish_pending_frame, send_decoder_thread_error, send_frame_publish_pressure_event,
    set_decoder_eof_drain_state,
};
use runtime_loop::{
    DecoderThreadChannels, decoder_eof_drain_state_matches_generation, decoder_thread_loop,
    reject_unsupported_vaapi_stream_config,
};
#[cfg(test)]
use video_core::VideoFramePublishPressureDiagnostics;

/// Packet вместе с моментом попадания в bounded decoder channel.
struct QueuedDecodePacket {
    /// Encoded packet payload и metadata.
    packet: DecodePacket,

    /// Монотонный момент successful enqueue.
    enqueued_at: Instant,
}

/// Управляющая структура decoder thread.
///
/// Владеет sender/reciever каналов. Сама decoder thread запущена в фоне.
pub struct VideoDecodeThread {
    packet_tx: Sender<QueuedDecodePacket>,
    control_tx: Sender<ThreadControlMsg>,
    control_pressure: Arc<DecoderControlChannelPressureCounters>,
    frame_rx: Receiver<DecodedFrame>,
    packet_ack_rx: Receiver<DecodePacketAck>,
    error_rx: Receiver<DecodeThreadError>,
    diagnostic_rx: Mutex<DecoderDiagnosticReceiver>,
    activity_subscription: VideoDecoderActivitySubscription,
    resource_pool: Arc<Mutex<crate::resource_pool::FrameResourcePool>>,
    thread_state: DecoderThreadState,
    stream_config: Arc<Mutex<Option<video_core::VideoStreamDecodeConfig>>>,
    end_of_stream_drain_state: Arc<Mutex<video_core::VideoDecoderEndOfStreamDrainState>>,
    config: VideoDecodeThreadConfig,
    backend_name: &'static str,
}

impl VideoDecodeThread {
    /// Создаёт decoder thread с VA-API hardware decoder.
    pub fn new() -> anyhow::Result<Self> {
        Self::new_with_config(VideoDecodeThreadConfig::from_env())
    }

    /// Создаёт decoder thread с явно заданными bounded queue/runtime limits.
    pub fn new_with_config(config: VideoDecodeThreadConfig) -> anyhow::Result<Self> {
        let config = config.normalized();
        let resource_pool = Arc::new(Mutex::new(
            crate::resource_pool::FrameResourcePool::new_with_capacity(
                config.zero_copy_surface_pool_slots,
            ),
        ));
        let resource_pool_for_thread = resource_pool.clone();

        let (packet_tx, packet_rx) = bounded::<QueuedDecodePacket>(config.packet_channel_frames);
        let (control_tx, control_rx) = bounded::<ThreadControlMsg>(config.control_channel_frames);
        let control_pressure = Arc::new(DecoderControlChannelPressureCounters::default());
        let (frame_tx, frame_rx) = bounded::<DecodedFrame>(config.frame_channel_frames);
        let (packet_ack_tx, packet_ack_rx) = unbounded::<DecodePacketAck>();
        let (error_tx, error_rx) = bounded::<DecodeThreadError>(1);
        let (diagnostic_tx, diagnostic_rx) =
            std::sync::mpsc::sync_channel(DECODER_DIAGNOSTIC_CHANNEL_CAPACITY);
        let thread_diagnostic_tx = diagnostic_tx.clone();
        let (activity_notifier, activity_subscription) = VideoDecoderActivityNotifier::new();
        let decoder_activity_notifier = activity_notifier.clone();
        let (init_tx, init_rx) = bounded::<anyhow::Result<()>>(1);
        let thread_state = DecoderThreadState::new();
        let decoder_runtime_config = config.vaapi_decoder_config();
        let end_of_stream_drain_state = Arc::new(Mutex::new(
            video_core::VideoDecoderEndOfStreamDrainState::Idle,
        ));
        let end_of_stream_drain_state_for_thread = end_of_stream_drain_state.clone();

        std::thread::Builder::new()
            .name("video-decode".into())
            .spawn(move || {
                info!("Decoder thread started");

                let decoder = match crate::VaapiVideoDecoder::new_with_pool_and_activity_notifier(
                    resource_pool_for_thread,
                    Some(diagnostic_tx),
                    decoder_runtime_config,
                    Some(decoder_activity_notifier),
                ) {
                    Ok(decoder) => {
                        if init_tx.send(Ok(())).is_err() {
                            trace!("Decoder thread init receiver dropped — exiting");
                            return;
                        }
                        decoder
                    }
                    Err(error) => {
                        tracing::error!(
                            error = %error,
                            "Decoder thread failed to create VA-API decoder"
                        );
                        let _ = init_tx.send(Err(
                            error.context("Decoder thread failed to create VA-API decoder")
                        ));
                        return;
                    }
                };

                decoder_thread_loop(
                    decoder,
                    DecoderThreadChannels {
                        packet_rx,
                        control_rx,
                        frame_tx,
                        packet_ack_tx,
                        error_tx,
                        diagnostic_tx: thread_diagnostic_tx,
                        activity_notifier,
                        end_of_stream_drain_state: end_of_stream_drain_state_for_thread,
                    },
                );
                info!("Decoder thread exiting");
            })
            .map_err(|e| anyhow::anyhow!("Failed to spawn decoder thread: {}", e))?;

        match init_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error),
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "Decoder thread exited before initialization completed: {}",
                    error
                ));
            }
        }

        Ok(Self {
            packet_tx,
            control_tx,
            control_pressure,
            frame_rx,
            packet_ack_rx,
            error_rx,
            diagnostic_rx: Mutex::new(diagnostic_rx),
            activity_subscription,
            resource_pool,
            thread_state,
            stream_config: Arc::new(Mutex::new(None)),
            end_of_stream_drain_state,
            config,
            backend_name: "VA-API VP9",
        })
    }

    /// Отправляет video packet в decoder thread.
    pub fn send_packet(&self, packet: DecodePacket) -> Result<(), DecodeThreadSendError> {
        self.ensure_thread_usable()
            .map_err(DecodeThreadSendError::Fatal)?;
        let queued_packet = QueuedDecodePacket {
            packet,
            enqueued_at: Instant::now(),
        };

        self.packet_tx
            .try_send(queued_packet)
            .map_err(|error| match error {
                TrySendError::Full(_) => DecodeThreadSendError::Backpressure(
                    DecodeThreadBackpressureReason::PacketQueueFull {
                        queued_packets: self.packet_tx.len(),
                        capacity: self.packet_tx.capacity().unwrap_or(0),
                    },
                ),
                TrySendError::Disconnected(_) => {
                    let fatal_error = self
                        .thread_state
                        .mark_fatal(DecodeThreadError::new("Decoder thread disconnected"));
                    DecodeThreadSendError::Fatal(fatal_error)
                }
            })
    }

    /// Принимает stream config для текущей VA-API adapter matrix.
    pub fn configure_stream(
        &self,
        config: video_core::VideoStreamDecodeConfig,
    ) -> video_core::VideoStreamConfigResult {
        if let Err(error) = self.ensure_thread_usable() {
            return video_core::VideoStreamConfigResult::Fatal(error.into());
        }
        if let Some(rejection) = reject_unsupported_vaapi_stream_config(&config) {
            return video_core::VideoStreamConfigResult::Unsupported(rejection);
        }

        {
            let stream_config = match self.stream_config.lock() {
                Ok(stream_config) => stream_config,
                Err(error) => {
                    let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(
                        format!("VA-API stream config mutex poisoned: {error}"),
                    ));
                    return video_core::VideoStreamConfigResult::Fatal(fatal_error.into());
                }
            };

            if stream_config.as_ref() == Some(&config) {
                return video_core::VideoStreamConfigResult::Unchanged;
            }
        }

        let (done_tx, done_rx) = bounded(1);
        if let Err(error) = self
            .control_tx
            .try_send(ThreadControlMsg::ConfigureStream(config.clone(), done_tx))
        {
            return match error {
                TrySendError::Full(_) => video_core::VideoStreamConfigResult::Backpressure(
                    video_core::VideoDecoderControlBackpressureReason::ControlChannelFull {
                        queued_messages: self.control_tx.len(),
                        capacity: self.control_tx.capacity().unwrap_or(0),
                    },
                ),
                TrySendError::Disconnected(_) => {
                    let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(
                        "Decoder thread disconnected before stream configure",
                    ));
                    video_core::VideoStreamConfigResult::Fatal(fatal_error.into())
                }
            };
        }

        let configure_result =
            wait_for_configure_stream_ack(done_rx, self.config.flush_timeout, &self.thread_state);

        if configure_result == video_core::VideoStreamConfigResult::Configured {
            let mut stream_config = match self.stream_config.lock() {
                Ok(stream_config) => stream_config,
                Err(error) => {
                    let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(
                        format!("VA-API stream config mutex poisoned after configure: {error}"),
                    ));
                    return video_core::VideoStreamConfigResult::Fatal(fatal_error.into());
                }
            };

            *stream_config = Some(config);
            self.reset_end_of_stream_drain_state();
        }

        configure_result
    }

    /// Очищает stream config как explicit media-switch lifecycle step.
    pub fn clear_stream(&self) -> video_core::VideoStreamConfigResult {
        if let Err(error) = self.ensure_thread_usable() {
            return video_core::VideoStreamConfigResult::Fatal(error.into());
        }

        let mut stream_config = match self.stream_config.lock() {
            Ok(stream_config) => stream_config,
            Err(error) => {
                let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(format!(
                    "VA-API stream config mutex poisoned during clear: {error}"
                )));
                return video_core::VideoStreamConfigResult::Fatal(fatal_error.into());
            }
        };

        self.reset_end_of_stream_drain_state();
        if stream_config.take().is_some() {
            video_core::VideoStreamConfigResult::Cleared
        } else {
            video_core::VideoStreamConfigResult::Unchanged
        }
    }

    /// Устанавливает decoder-side output floor для accurate seek preroll.
    pub fn set_preroll_output_floor(
        &self,
        floor: video_core::VideoPrerollOutputFloor,
    ) -> video_core::VideoPrerollOutputFloorResult {
        if let Err(error) = self.ensure_thread_usable() {
            return video_core::VideoPrerollOutputFloorResult::Fatal(error.into());
        }

        let (done_tx, done_rx) = bounded(1);
        if let Err(error) = self
            .control_tx
            .try_send(ThreadControlMsg::SetPrerollOutputFloor(floor, done_tx))
        {
            return match error {
                TrySendError::Full(_) => {
                    let _message = record_decoder_control_send_failure(
                        DecoderControlOperation::SetPrerollOutputFloor,
                        &self.control_tx,
                        &self.control_pressure,
                        &error,
                    );
                    video_core::VideoPrerollOutputFloorResult::Backpressure(
                        video_core::VideoDecoderControlBackpressureReason::ControlChannelFull {
                            queued_messages: self.control_tx.len(),
                            capacity: self.control_tx.capacity().unwrap_or(0),
                        },
                    )
                }
                TrySendError::Disconnected(_) => {
                    let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(
                        "Decoder thread disconnected before preroll output-floor set",
                    ));
                    video_core::VideoPrerollOutputFloorResult::Fatal(fatal_error.into())
                }
            };
        }

        wait_for_preroll_output_floor_ack(
            done_rx,
            self.config.flush_timeout,
            &self.thread_state,
            "preroll output-floor set",
        )
    }

    /// Очищает decoder-side output floor без изменения seek generation.
    pub fn clear_preroll_output_floor(
        &self,
        clear: video_core::VideoPrerollOutputFloorClear,
    ) -> video_core::VideoPrerollOutputFloorResult {
        if let Err(error) = self.ensure_thread_usable() {
            return video_core::VideoPrerollOutputFloorResult::Fatal(error.into());
        }

        let (done_tx, done_rx) = bounded(1);
        if let Err(error) = self
            .control_tx
            .try_send(ThreadControlMsg::ClearPrerollOutputFloor(clear, done_tx))
        {
            return match error {
                TrySendError::Full(_) => {
                    let _message = record_decoder_control_send_failure(
                        DecoderControlOperation::ClearPrerollOutputFloor,
                        &self.control_tx,
                        &self.control_pressure,
                        &error,
                    );
                    video_core::VideoPrerollOutputFloorResult::Backpressure(
                        video_core::VideoDecoderControlBackpressureReason::ControlChannelFull {
                            queued_messages: self.control_tx.len(),
                            capacity: self.control_tx.capacity().unwrap_or(0),
                        },
                    )
                }
                TrySendError::Disconnected(_) => {
                    let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(
                        "Decoder thread disconnected before preroll output-floor clear",
                    ));
                    video_core::VideoPrerollOutputFloorResult::Fatal(fatal_error.into())
                }
            };
        }

        wait_for_preroll_output_floor_ack(
            done_rx,
            self.config.flush_timeout,
            &self.thread_state,
            "preroll output-floor clear",
        )
    }

    /// Запускает explicit EOF drain через decoder thread без seek flush semantics.
    pub fn begin_end_of_stream_drain(
        &self,
        generation: u64,
    ) -> video_core::VideoDecoderEndOfStreamDrainResult {
        if let Err(error) = self.ensure_thread_usable() {
            return video_core::VideoDecoderEndOfStreamDrainResult::Fatal(error.into());
        }

        let drain_state = match self.end_of_stream_drain_state.lock() {
            Ok(drain_state) => drain_state,
            Err(error) => {
                let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(format!(
                    "VA-API EOF drain state mutex poisoned: {error}"
                )));
                return video_core::VideoDecoderEndOfStreamDrainResult::Fatal(fatal_error.into());
            }
        };

        if decoder_eof_drain_state_matches_generation(&drain_state, generation) {
            return video_core::VideoDecoderEndOfStreamDrainResult::Unchanged(drain_state.clone());
        }
        drop(drain_state);

        let (done_tx, done_rx) = bounded(1);
        if let Err(error) = self
            .control_tx
            .try_send(ThreadControlMsg::BeginEndOfStreamDrain(generation, done_tx))
        {
            return match error {
                TrySendError::Full(_) => {
                    let _message = record_decoder_control_send_failure(
                        DecoderControlOperation::EofDrain,
                        &self.control_tx,
                        &self.control_pressure,
                        &error,
                    );
                    video_core::VideoDecoderEndOfStreamDrainResult::Backpressure(
                        video_core::VideoDecoderControlBackpressureReason::ControlChannelFull {
                            queued_messages: self.control_tx.len(),
                            capacity: self.control_tx.capacity().unwrap_or(0),
                        },
                    )
                }
                TrySendError::Disconnected(_) => {
                    let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(
                        "Decoder thread disconnected before EOF drain",
                    ));
                    video_core::VideoDecoderEndOfStreamDrainResult::Fatal(fatal_error.into())
                }
            };
        }

        wait_for_end_of_stream_drain_ack(done_rx, self.config.flush_timeout, &self.thread_state)
    }

    /// Возвращает текущее explicit EOF drain state без блокировки decoder thread loop-а.
    pub fn end_of_stream_drain_state(&self) -> video_core::VideoDecoderEndOfStreamDrainState {
        match self.end_of_stream_drain_state.lock() {
            Ok(drain_state) => {
                player_visible_eof_drain_state(drain_state.clone(), self.frame_rx.len())
            }
            Err(error) => {
                let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(format!(
                    "VA-API EOF drain state mutex poisoned during read: {error}"
                )));
                video_core::VideoDecoderEndOfStreamDrainState::Fatal {
                    generation: None,
                    error: fatal_error.into(),
                }
            }
        }
    }

    /// Сбрасывает EOF-drain marker после смены stream-а или media.
    fn reset_end_of_stream_drain_state(&self) {
        if let Ok(mut drain_state) = self.end_of_stream_drain_state.lock() {
            *drain_state = video_core::VideoDecoderEndOfStreamDrainState::Idle;
        }
    }

    /// Освобождает frame, который не находится в renderer GPU work.
    ///
    /// Используется для queued/present frames без active render lease. Такой frame
    /// можно вернуть decoder-у сразу: GPU completion уже не требуется.
    pub fn release_frame(&self, handle: video_core::FrameResourceHandle) {
        let release_stats = match self.resource_pool.lock() {
            Ok(mut resource_pool) => {
                if let Err(error) = resource_pool.release_without_gpu_submission(handle) {
                    let resource_stats = resource_pool.stats();
                    let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(
                        format!("Zero-copy immediate release lifecycle violation: {error}"),
                    ));
                    tracing::warn!(
                        error = %error,
                        fatal = %fatal_error,
                        handle_id = handle.0,
                        zero_copy_capacity = resource_stats.capacity,
                        zero_copy_slots = resource_stats.slots,
                        zero_copy_in_use = resource_stats.in_use,
                        zero_copy_free_surfaces = resource_stats.free_surfaces,
                        zero_copy_waiting_gpu_completion =
                            resource_stats.waiting_gpu_completion,
                        zero_copy_waiting_decoder_reuse =
                            resource_stats.waiting_decoder_reuse,
                        "Failed to move zero-copy surface into decoder reuse state"
                    );
                    return;
                }
                let resource_stats = resource_pool.stats();
                trace!(
                    handle_id = handle.0,
                    zero_copy_capacity = resource_stats.capacity,
                    zero_copy_slots = resource_stats.slots,
                    zero_copy_in_use = resource_stats.in_use,
                    zero_copy_free_surfaces = resource_stats.free_surfaces,
                    zero_copy_waiting_gpu_completion = resource_stats.waiting_gpu_completion,
                    zero_copy_waiting_decoder_reuse = resource_stats.waiting_decoder_reuse,
                    "Queued decoder-owned zero-copy frame for decoder reuse"
                );
                Some(resource_stats)
            }
            Err(error) => {
                let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(format!(
                    "Zero-copy resource pool mutex poisoned during immediate release: {error}"
                )));
                tracing::warn!(
                    error = %error,
                    fatal = %fatal_error,
                    handle_id = handle.0,
                    "Resource pool mutex poisoned during immediate release"
                );
                return;
            }
        };

        if let Err(error) = self
            .control_tx
            .try_send(ThreadControlMsg::ReleaseZeroCopy(handle))
        {
            let error_message = record_decoder_control_send_failure(
                DecoderControlOperation::Release,
                &self.control_tx,
                &self.control_pressure,
                &error,
            );
            let fatal_error = self
                .thread_state
                .mark_fatal(DecodeThreadError::new(error_message));
            tracing::warn!(
                error = %error,
                fatal = %fatal_error,
                handle_id = handle.0,
                zero_copy_capacity = ?release_stats.map(|stats| stats.capacity),
                zero_copy_slots = ?release_stats.map(|stats| stats.slots),
                zero_copy_in_use = ?release_stats.map(|stats| stats.in_use),
                zero_copy_free_surfaces = ?release_stats.map(|stats| stats.free_surfaces),
                zero_copy_waiting_gpu_completion =
                    ?release_stats.map(|stats| stats.waiting_gpu_completion),
                zero_copy_waiting_decoder_reuse =
                    ?release_stats.map(|stats| stats.waiting_decoder_reuse),
                "Failed to send immediate zero-copy release to decoder thread"
            );
        }
    }

    /// Забирает готовый decoded frame из очереди (неблокирующий).
    pub fn try_recv_frame(&self) -> Option<DecodedFrame> {
        self.frame_rx.try_recv().ok()
    }

    /// Забирает backend diagnostics event без блокировки.
    pub fn try_recv_diagnostic_event(&self) -> Option<VideoDecoderDiagnosticEvent> {
        match self.diagnostic_rx.lock() {
            Ok(receiver) => receiver.try_recv().ok(),
            Err(poisoned) => {
                tracing::error!("VA-API diagnostic receiver mutex poisoned");
                poisoned.into_inner().try_recv().ok()
            }
        }
    }

    /// Возвращает нейтральный snapshot decoder activity для player-side wait boundary.
    pub fn decoder_activity_snapshot(&self) -> VideoDecoderActivitySnapshot {
        self.activity_subscription.snapshot()
    }

    /// Забирает fatal error из decoder thread, если backend остановился fail-closed.
    pub fn try_recv_error(&self) -> Option<DecodeThreadError> {
        self.absorb_decoder_thread_errors();
        self.thread_state.take_pending_error()
    }

    /// Синхронно сбрасывает decoder thread и освобождает уже полученные кадры.
    pub fn flush(&self) -> anyhow::Result<()> {
        if let Err(error) = self.ensure_thread_usable() {
            self.release_received_frames();
            return Err(anyhow::anyhow!("{}", error));
        }

        let (done_tx, done_rx) = bounded(1);
        if let Err(error) = self.control_tx.try_send(ThreadControlMsg::Flush(done_tx)) {
            self.release_received_frames();
            let error_message = record_decoder_control_send_failure(
                DecoderControlOperation::Flush,
                &self.control_tx,
                &self.control_pressure,
                &error,
            );
            let fatal_error = self
                .thread_state
                .mark_fatal(DecodeThreadError::new(error_message));
            return Err(anyhow::anyhow!("{}", fatal_error));
        }

        let flush_result =
            wait_for_flush_ack(done_rx, self.config.flush_timeout, &self.thread_state);
        self.release_received_frames();
        self.drain_completed_packet_acks();
        if flush_result.is_ok() {
            self.reset_end_of_stream_drain_state();
        }
        flush_result
    }

    /// Возвращает cloneable provider для resource lookup/descriptor/release.
    #[must_use]
    pub fn frame_resource_provider(&self) -> VideoFrameResourceProvider {
        VideoFrameResourceProvider {
            control_tx: self.control_tx.clone(),
            control_pressure: self.control_pressure.clone(),
            resource_pool: self.resource_pool.clone(),
            thread_state: self.thread_state.clone(),
        }
    }

    /// Возвращает состояние resource pool для backpressure и UI.
    pub fn resource_pool_stats(&self) -> Option<ResourcePoolStats> {
        match self.resource_pool.lock() {
            Ok(resource_pool) => Some(resource_pool.stats()),
            Err(error) => {
                let fatal_error = self.thread_state.mark_fatal(DecodeThreadError::new(format!(
                    "Zero-copy resource pool mutex poisoned during stats read: {error}"
                )));
                tracing::warn!(
                    error = %error,
                    fatal = %fatal_error,
                    "Resource pool mutex poisoned during stats read"
                );
                None
            }
        }
    }

    /// Возвращает sender-side pressure snapshot bounded control channel-а.
    #[must_use]
    pub fn control_channel_pressure_stats(&self) -> VideoDecoderControlChannelPressureStats {
        self.control_pressure.snapshot(&self.control_tx)
    }

    /// Возвращает текущую глубину bounded packet channel.
    #[must_use]
    pub fn packet_queue_depth(&self) -> usize {
        self.packet_tx.len()
    }

    /// Забирает количество packets, которые decoder thread уже обработал.
    #[must_use]
    pub fn drain_completed_packet_count(&self) -> usize {
        self.drain_completed_packet_acks()
    }

    /// Имя бэкенда для UI.
    pub fn backend_name(&self) -> &'static str {
        self.backend_name
    }

    /// Переносит fatal ошибки из decoder thread channel в sticky state.
    fn absorb_decoder_thread_errors(&self) {
        loop {
            match self.error_rx.try_recv() {
                Ok(error) => {
                    self.thread_state.mark_fatal(error);
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    /// Проверяет, что decoder thread ещё можно использовать для новых команд.
    fn ensure_thread_usable(&self) -> Result<(), DecodeThreadError> {
        self.absorb_decoder_thread_errors();
        if let Some(error) = self.thread_state.current_error() {
            return Err(error);
        }
        Ok(())
    }

    /// Освобождает кадры, которые уже пришли через frame channel до/во время flush.
    fn release_received_frames(&self) {
        while let Ok(frame) = self.frame_rx.try_recv() {
            self.release_frame(frame.resource_handle);
        }
    }

    /// Очищает packet-ack channel и возвращает число подтверждений.
    fn drain_completed_packet_acks(&self) -> usize {
        let mut completed_packet_count = 0usize;
        while self.packet_ack_rx.try_recv().is_ok() {
            completed_packet_count = completed_packet_count.saturating_add(1);
        }
        completed_packet_count
    }
}

/// Не показывает player-у `Drained`, пока decoded frame channel ещё несёт tail frames.
fn player_visible_eof_drain_state(
    state: video_core::VideoDecoderEndOfStreamDrainState,
    pending_decoded_frames: usize,
) -> video_core::VideoDecoderEndOfStreamDrainState {
    if pending_decoded_frames == 0 {
        return state;
    }

    match state {
        video_core::VideoDecoderEndOfStreamDrainState::Drained { generation } => {
            video_core::VideoDecoderEndOfStreamDrainState::Draining { generation }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests;
