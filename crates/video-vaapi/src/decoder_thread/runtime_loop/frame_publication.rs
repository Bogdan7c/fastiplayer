use std::time::{Duration, Instant};

use codec_core::VideoColorMetadata;
use crossbeam_channel::{Sender, TrySendError};
use video_core::{
    DecodedFrame, VideoDecoderActivityNotifier, VideoDecoderDiagnosticEvent,
    VideoFramePublishPressureDiagnostics,
};

use crate::decoder::VaapiDecodePacketOutcome;

use super::super::{
    DecodePacketAck, DecodeThreadError, DecoderDiagnosticSender, QueuedDecodePacket,
};
use super::{notify_decoder_activity, send_decoder_thread_error};

/// Decoded frame, который уже готов, но ещё ждёт место в bounded frame channel.
pub(in super::super) struct PendingFramePublish {
    /// Frame metadata и zero-copy texture handle.
    frame: DecodedFrame,

    /// Монотонный момент начала publish stage.
    publish_started_at: Instant,

    /// Был ли этот frame уже остановлен заполненным bounded frame channel.
    has_seen_channel_full: bool,
}

impl PendingFramePublish {
    /// Создаёт pending publish item и начинает измерять decoded-frame publish latency.
    pub(in super::super) fn new(frame: DecodedFrame) -> Self {
        Self {
            frame,
            publish_started_at: Instant::now(),
            has_seen_channel_full: false,
        }
    }

    /// Помечает frame как ожидающий свободного места в bounded frame channel.
    fn mark_channel_full(&mut self) {
        self.has_seen_channel_full = true;
    }
}

/// Локальные counters decoder thread-а для decoded-frame publish boundary.
#[derive(Debug, Default)]
pub(in super::super) struct FramePublishPressureCounters {
    /// Накопительный snapshot, который можно отправлять через diagnostics event.
    pressure: VideoFramePublishPressureDiagnostics,
}

impl FramePublishPressureCounters {
    /// Учитывает заполненный bounded frame channel без изменения publish lifecycle.
    fn record_channel_full(&mut self) {
        self.pressure.frame_publish_channel_full_count = self
            .pressure
            .frame_publish_channel_full_count
            .saturating_add(1);
    }

    /// Учитывает повторную попытку публикации уже pending frame.
    fn record_pending_retry(&mut self) {
        self.pressure.pending_publish_retry_count =
            self.pressure.pending_publish_retry_count.saturating_add(1);
    }

    /// Учитывает latency только один раз: когда frame реально опубликован worker-у.
    fn record_published_latency(&mut self, latency: Duration) {
        self.pressure.total_decoded_frame_publish_latency = self
            .pressure
            .total_decoded_frame_publish_latency
            .saturating_add(latency);
        if latency > self.pressure.max_decoded_frame_publish_latency {
            self.pressure.max_decoded_frame_publish_latency = latency;
        }
    }

    /// Возвращает копию counters для неблокирующей отправки в diagnostics channel.
    fn snapshot(&self) -> VideoFramePublishPressureDiagnostics {
        self.pressure
    }
}

/// Собирает decoder-thread state, который нужен одному packet decode step.
pub(in super::super) struct DecodeQueuedPacketContext<'a> {
    pub(in super::super) frame_tx: &'a Sender<DecodedFrame>,
    pub(in super::super) packet_ack_tx: &'a Sender<DecodePacketAck>,
    pub(in super::super) error_tx: &'a Sender<DecodeThreadError>,
    pub(in super::super) pending_publish: &'a mut Option<PendingFramePublish>,
    pub(in super::super) publish_pressure: &'a mut FramePublishPressureCounters,
    pub(in super::super) diagnostic_tx: &'a DecoderDiagnosticSender,
    pub(in super::super) activity_notifier: &'a VideoDecoderActivityNotifier,
    pub(in super::super) latest_color_metadata: &'a mut Option<VideoColorMetadata>,
}

/// Result одного decode attempt-а внутри decoder thread loop-а.
pub(in super::super) enum DecodeQueuedPacketResult {
    /// Loop может продолжать normal scheduling.
    Continue,

    /// Decode thread должен завершиться.
    Stop,

    /// Packet не принят из-за output-buffer pressure и должен быть повторён позже.
    OutputBackpressured(QueuedDecodePacket),
}

/// Обрабатывает результат backend decode без повторного знания о VAAPI submit path.
pub(in super::super) fn handle_decode_packet_outcome(
    decode_result: anyhow::Result<VaapiDecodePacketOutcome>,
    queued_packet: QueuedDecodePacket,
    packet_receive_latency: Duration,
    decode_context: DecodeQueuedPacketContext<'_>,
) -> DecodeQueuedPacketResult {
    let decode_packet = &queued_packet.packet;

    match decode_result {
        Ok(VaapiDecodePacketOutcome::OutputBackpressured) => {
            DecodeQueuedPacketResult::OutputBackpressured(queued_packet)
        }
        Ok(VaapiDecodePacketOutcome::Accepted(Some(frame))) => {
            let mut frame = *frame;
            let _ = decode_context.packet_ack_tx.try_send(());
            notify_decoder_activity(decode_context.activity_notifier);
            *decode_context.latest_color_metadata = decode_packet.resolved_color.clone();
            if let Some(color_metadata) = &decode_packet.resolved_color {
                frame.color = color_metadata.clone();
            }
            frame.diagnostics.timings.decoder_packet_receive_latency = Some(packet_receive_latency);
            *decode_context.pending_publish = Some(PendingFramePublish::new(frame));
            notify_decoder_activity(decode_context.activity_notifier);
            if publish_pending_frame(
                decode_context.frame_tx,
                decode_context.pending_publish,
                decode_context.publish_pressure,
                decode_context.diagnostic_tx,
                decode_context.activity_notifier,
            ) {
                DecodeQueuedPacketResult::Continue
            } else {
                DecodeQueuedPacketResult::Stop
            }
        }
        Ok(VaapiDecodePacketOutcome::Accepted(None)) => {
            let _ = decode_context.packet_ack_tx.try_send(());
            notify_decoder_activity(decode_context.activity_notifier);
            *decode_context.latest_color_metadata = decode_packet.resolved_color.clone();
            DecodeQueuedPacketResult::Continue
        }
        Err(error) => {
            if crate::decoder::is_fatal_decoder_error(&error) {
                let message = format!("Video decoder stopped after fatal error: {error:#}");
                tracing::warn!(
                    error = %message,
                    "Decoder thread: fatal decode error, exiting"
                );
                send_decoder_thread_error(
                    decode_context.error_tx,
                    message,
                    decode_context.activity_notifier,
                );
                return DecodeQueuedPacketResult::Stop;
            }
            let _ = decode_context.packet_ack_tx.try_send(());
            notify_decoder_activity(decode_context.activity_notifier);
            tracing::warn!(error = %error, "Decoder thread: decode error");
            DecodeQueuedPacketResult::Continue
        }
    }
}

/// Пытается передать pending frame worker-у, не блокируя release/flush control path.
pub(in super::super) fn publish_pending_frame(
    frame_tx: &Sender<DecodedFrame>,
    pending_publish: &mut Option<PendingFramePublish>,
    publish_pressure: &mut FramePublishPressureCounters,
    diagnostic_tx: &DecoderDiagnosticSender,
    activity_notifier: &VideoDecoderActivityNotifier,
) -> bool {
    let Some(mut pending_frame) = pending_publish.take() else {
        return true;
    };

    let is_retry = pending_frame.has_seen_channel_full;
    let publish_latency = pending_frame.publish_started_at.elapsed();
    pending_frame
        .frame
        .diagnostics
        .timings
        .decoded_frame_publish_latency = Some(publish_latency);

    match frame_tx.try_send(pending_frame.frame) {
        Ok(()) => {
            if is_retry {
                publish_pressure.record_pending_retry();
            }
            publish_pressure.record_published_latency(publish_latency);
            if is_retry {
                send_frame_publish_pressure_event(
                    diagnostic_tx,
                    publish_pressure.snapshot(),
                    activity_notifier,
                );
            }
            notify_decoder_activity(activity_notifier);
            true
        }
        Err(TrySendError::Full(frame)) => {
            if is_retry {
                publish_pressure.record_pending_retry();
            }
            publish_pressure.record_channel_full();
            let mut blocked_frame = PendingFramePublish {
                frame,
                publish_started_at: pending_frame.publish_started_at,
                has_seen_channel_full: pending_frame.has_seen_channel_full,
            };
            blocked_frame.mark_channel_full();
            *pending_publish = Some(blocked_frame);
            send_frame_publish_pressure_event(
                diagnostic_tx,
                publish_pressure.snapshot(),
                activity_notifier,
            );
            true
        }
        Err(TrySendError::Disconnected(frame)) => {
            if is_retry {
                publish_pressure.record_pending_retry();
                send_frame_publish_pressure_event(
                    diagnostic_tx,
                    publish_pressure.snapshot(),
                    activity_notifier,
                );
            }
            tracing::warn!(
                handle_id = frame.resource_handle.0,
                "Player thread dropped decoded frame receiver"
            );
            *pending_publish = Some(PendingFramePublish {
                frame,
                publish_started_at: pending_frame.publish_started_at,
                has_seen_channel_full: pending_frame.has_seen_channel_full,
            });
            false
        }
    }
}

/// Отправляет cumulative publish-pressure snapshot без блокировки decoder thread-а.
pub(in super::super) fn send_frame_publish_pressure_event(
    diagnostic_tx: &DecoderDiagnosticSender,
    pressure: VideoFramePublishPressureDiagnostics,
    activity_notifier: &VideoDecoderActivityNotifier,
) {
    let _ = diagnostic_tx
        .try_send(VideoDecoderDiagnosticEvent::DecodedFramePublishPressure { pressure });
    notify_decoder_activity(activity_notifier);
}

/// Освобождает frame, который decoder уже импортировал, но не успел отдать worker-у.
pub(super) fn release_pending_publish_frame(
    decoder: &mut crate::VaapiVideoDecoder,
    pending_publish: Option<PendingFramePublish>,
) {
    let Some(pending_frame) = pending_publish else {
        return;
    };

    if let Err(error) = decoder.release_frame(pending_frame.frame.resource_handle) {
        tracing::warn!(
            error = %error,
            handle_id = pending_frame.frame.resource_handle.0,
            "Failed to release pending decoded frame during decoder thread shutdown/flush"
        );
    }
}
