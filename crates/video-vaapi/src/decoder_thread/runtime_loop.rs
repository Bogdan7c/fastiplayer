use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use codec_core::VideoColorMetadata;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, TrySendError, select};
use media_core::{Packet, TrackKind};
use tracing::trace;
use video_core::{
    DecodedFrame, VideoDecoder, VideoDecoderActivityNotifier, VideoDecoderDiagnosticEvent,
    VideoFramePublishPressureDiagnostics,
};

use crate::decoder::VaapiDecodePacketOutcome;

use super::control::ThreadControlMsg;
use super::{
    DECODER_FRAME_PUBLISH_RETRY_MS, DecodePacketAck, DecodeThreadError, DecoderDiagnosticSender,
    QueuedDecodePacket,
};

/// Проверяет stream config на реализованный в этой фазе VA-API adapter intersection.
pub(super) fn reject_unsupported_vaapi_stream_config(
    config: &video_core::VideoStreamDecodeConfig,
) -> Option<video_core::VideoStreamConfigRejection> {
    crate::codec_adapter::VaapiCodecAdapterFactory::stream_config_rejection(config)
}

/// Decoded frame, который уже готов, но ещё ждёт место в bounded frame channel.
pub(super) struct PendingFramePublish {
    /// Frame metadata и zero-copy texture handle.
    frame: DecodedFrame,

    /// Монотонный момент начала publish stage.
    publish_started_at: Instant,

    /// Был ли этот frame уже остановлен заполненным bounded frame channel.
    has_seen_channel_full: bool,
}

impl PendingFramePublish {
    /// Создаёт pending publish item и начинает измерять decoded-frame publish latency.
    pub(super) fn new(frame: DecodedFrame) -> Self {
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
pub(super) struct FramePublishPressureCounters {
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

/// Каналы и shared state, которыми владеет lifetime decoder thread loop-а.
pub(super) struct DecoderThreadChannels {
    /// Encoded packets от player worker-а.
    pub(super) packet_rx: Receiver<QueuedDecodePacket>,

    /// Control messages: release, flush, stream config, EOF drain.
    pub(super) control_rx: Receiver<ThreadControlMsg>,

    /// Decoded frames обратно в player worker.
    pub(super) frame_tx: Sender<DecodedFrame>,

    /// Packet completion ACK-и для player-side in-flight accounting.
    pub(super) packet_ack_tx: Sender<DecodePacketAck>,

    /// Fatal decoder errors для player boundary.
    pub(super) error_tx: Sender<DecodeThreadError>,

    /// Decoder diagnostics events без player-core dependency.
    pub(super) diagnostic_tx: DecoderDiagnosticSender,

    /// Нейтральный non-blocking activity notifier для event-driven player wakeup.
    pub(super) activity_notifier: VideoDecoderActivityNotifier,

    /// Shared EOF/DPB drain state, читаемый handle-ом с player thread-а.
    pub(super) end_of_stream_drain_state: Arc<Mutex<video_core::VideoDecoderEndOfStreamDrainState>>,
}

/// Borrowed context для control-message handlers внутри одного decoder-loop шага.
struct DecoderControlContext<'a> {
    /// Packet channel нужен flush path-у для сброса уже queued packets.
    packet_rx: &'a Receiver<QueuedDecodePacket>,

    /// Fatal errors публикуются отдельно от diagnostics и frame channel-а.
    pub(super) error_tx: &'a Sender<DecodeThreadError>,

    /// Pending frame release остаётся lifecycle-решением decoder thread-а.
    pub(super) pending_publish: &'a mut Option<PendingFramePublish>,

    /// Packet, который ждёт output capacity, должен сбрасываться при flush/reconfigure.
    output_backpressured_packet: &'a mut Option<QueuedDecodePacket>,

    /// Shared EOF/DPB drain state, читаемый playback-facing handle-ом.
    end_of_stream_drain_state: &'a Arc<Mutex<video_core::VideoDecoderEndOfStreamDrainState>>,

    /// Neutral activity notifier для wakeup side, не раскрывающий VAAPI channels.
    pub(super) activity_notifier: &'a VideoDecoderActivityNotifier,
}

/// Проверяет, относится ли уже начатый/завершённый EOF drain к текущей generation.
pub(super) fn decoder_eof_drain_state_matches_generation(
    state: &video_core::VideoDecoderEndOfStreamDrainState,
    generation: u64,
) -> bool {
    match state {
        video_core::VideoDecoderEndOfStreamDrainState::Draining {
            generation: active_generation,
        }
        | video_core::VideoDecoderEndOfStreamDrainState::Drained {
            generation: active_generation,
        } => *active_generation == generation,
        video_core::VideoDecoderEndOfStreamDrainState::Idle
        | video_core::VideoDecoderEndOfStreamDrainState::Fatal { .. } => false,
    }
}

/// Главный цикл decoder thread.
pub(super) fn decoder_thread_loop(
    mut decoder: crate::VaapiVideoDecoder,
    channels: DecoderThreadChannels,
) {
    let DecoderThreadChannels {
        packet_rx,
        control_rx,
        frame_tx,
        packet_ack_tx,
        error_tx,
        diagnostic_tx,
        activity_notifier,
        end_of_stream_drain_state,
    } = channels;
    let mut pending_publish: Option<PendingFramePublish> = None;
    let mut output_backpressured_packet: Option<QueuedDecodePacket> = None;
    let mut publish_pressure = FramePublishPressureCounters::default();
    let mut latest_color_metadata: Option<VideoColorMetadata> = None;

    loop {
        if let Err(error) = decoder.reclaim_suppressed_surfaces_for_thread() {
            send_decoder_thread_error(
                &error_tx,
                format!("Video decoder stopped during suppressed reclaim: {error:#}"),
                &activity_notifier,
            );
            break;
        }

        let controls_drained = {
            let mut control_context = DecoderControlContext {
                packet_rx: &packet_rx,
                error_tx: &error_tx,
                pending_publish: &mut pending_publish,
                output_backpressured_packet: &mut output_backpressured_packet,
                end_of_stream_drain_state: &end_of_stream_drain_state,
                activity_notifier: &activity_notifier,
            };
            drain_decoder_control_messages(&mut decoder, &control_rx, &mut control_context)
        };
        if !controls_drained {
            break;
        }

        if !publish_pending_frame(
            &frame_tx,
            &mut pending_publish,
            &mut publish_pressure,
            &diagnostic_tx,
            &activity_notifier,
        ) {
            break;
        }
        if let Err(error) = complete_decoder_eof_drain_if_ready(
            &decoder,
            pending_publish.as_ref(),
            &end_of_stream_drain_state,
            &activity_notifier,
        ) {
            send_decoder_thread_error(&error_tx, error.message().to_string(), &activity_notifier);
            break;
        }

        if pending_publish.is_some() {
            match control_rx.recv_timeout(Duration::from_millis(DECODER_FRAME_PUBLISH_RETRY_MS)) {
                Ok(control_message) => {
                    let mut control_context = DecoderControlContext {
                        packet_rx: &packet_rx,
                        error_tx: &error_tx,
                        pending_publish: &mut pending_publish,
                        output_backpressured_packet: &mut output_backpressured_packet,
                        end_of_stream_drain_state: &end_of_stream_drain_state,
                        activity_notifier: &activity_notifier,
                    };
                    if !handle_decoder_control_message(
                        &mut decoder,
                        control_message,
                        &mut control_context,
                    ) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            continue;
        }

        if let Some(mut frame) = decoder.take_ready_frame() {
            if let Some(color_metadata) = &latest_color_metadata {
                frame.color = color_metadata.clone();
            }
            pending_publish = Some(PendingFramePublish::new(frame));
            notify_decoder_activity(&activity_notifier);
            continue;
        }

        if let Some(queued_packet) = output_backpressured_packet.take() {
            match decode_queued_packet(
                &mut decoder,
                queued_packet,
                DecodeQueuedPacketContext {
                    frame_tx: &frame_tx,
                    packet_ack_tx: &packet_ack_tx,
                    error_tx: &error_tx,
                    pending_publish: &mut pending_publish,
                    publish_pressure: &mut publish_pressure,
                    diagnostic_tx: &diagnostic_tx,
                    activity_notifier: &activity_notifier,
                    latest_color_metadata: &mut latest_color_metadata,
                },
            ) {
                DecodeQueuedPacketResult::Continue => continue,
                DecodeQueuedPacketResult::Stop => break,
                DecodeQueuedPacketResult::OutputBackpressured(queued_packet) => {
                    output_backpressured_packet = Some(queued_packet);
                    match control_rx
                        .recv_timeout(Duration::from_millis(DECODER_FRAME_PUBLISH_RETRY_MS))
                    {
                        Ok(control_message) => {
                            let mut control_context = DecoderControlContext {
                                packet_rx: &packet_rx,
                                error_tx: &error_tx,
                                pending_publish: &mut pending_publish,
                                output_backpressured_packet: &mut output_backpressured_packet,
                                end_of_stream_drain_state: &end_of_stream_drain_state,
                                activity_notifier: &activity_notifier,
                            };
                            if !handle_decoder_control_message(
                                &mut decoder,
                                control_message,
                                &mut control_context,
                            ) {
                                break;
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    continue;
                }
            }
        }

        select! {
            recv(control_rx) -> control_result => {
                match control_result {
                    Ok(control_message) => {
                        let mut control_context = DecoderControlContext {
                            packet_rx: &packet_rx,
                            error_tx: &error_tx,
                            pending_publish: &mut pending_publish,
                            output_backpressured_packet: &mut output_backpressured_packet,
                            end_of_stream_drain_state: &end_of_stream_drain_state,
                            activity_notifier: &activity_notifier,
                        };
                        if !handle_decoder_control_message(
                            &mut decoder,
                            control_message,
                            &mut control_context,
                        ) {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            recv(packet_rx) -> packet_result => {
                match packet_result {
                    Ok(queued_packet) => {
                        match decode_queued_packet(
                            &mut decoder,
                            queued_packet,
                            DecodeQueuedPacketContext {
                                frame_tx: &frame_tx,
                                packet_ack_tx: &packet_ack_tx,
                                error_tx: &error_tx,
                                pending_publish: &mut pending_publish,
                                publish_pressure: &mut publish_pressure,
                                diagnostic_tx: &diagnostic_tx,
                                activity_notifier: &activity_notifier,
                                latest_color_metadata: &mut latest_color_metadata,
                            },
                        ) {
                            DecodeQueuedPacketResult::Continue => {}
                            DecodeQueuedPacketResult::Stop => break,
                            DecodeQueuedPacketResult::OutputBackpressured(queued_packet) => {
                                output_backpressured_packet = Some(queued_packet);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    release_pending_publish_frame(&mut decoder, pending_publish);
}

/// Обрабатывает все pending control messages перед packet receive.
fn drain_decoder_control_messages(
    decoder: &mut crate::VaapiVideoDecoder,
    control_rx: &Receiver<ThreadControlMsg>,
    control_context: &mut DecoderControlContext<'_>,
) -> bool {
    loop {
        match control_rx.try_recv() {
            Ok(control_message) => {
                if !handle_decoder_control_message(decoder, control_message, control_context) {
                    return false;
                }
            }
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => return false,
        }
    }
}

/// Читает shared EOF drain state с typed fatal error для decoder thread-а.
fn decoder_eof_drain_state(
    end_of_stream_drain_state: &Arc<Mutex<video_core::VideoDecoderEndOfStreamDrainState>>,
) -> Result<video_core::VideoDecoderEndOfStreamDrainState, DecodeThreadError> {
    end_of_stream_drain_state
        .lock()
        .map(|state| state.clone())
        .map_err(|error| {
            DecodeThreadError::new(format!("VA-API EOF drain state poisoned: {error}"))
        })
}

/// Записывает shared EOF drain state без раскрытия mutex-а в control handlers.
pub(super) fn set_decoder_eof_drain_state(
    end_of_stream_drain_state: &Arc<Mutex<video_core::VideoDecoderEndOfStreamDrainState>>,
    next_state: video_core::VideoDecoderEndOfStreamDrainState,
    activity_notifier: &VideoDecoderActivityNotifier,
) -> Result<(), DecodeThreadError> {
    let mut state = end_of_stream_drain_state.lock().map_err(|error| {
        DecodeThreadError::new(format!(
            "VA-API EOF drain state poisoned during update: {error}"
        ))
    })?;
    let state_changed = *state != next_state;
    *state = next_state;
    if state_changed {
        notify_decoder_activity(activity_notifier);
    }
    Ok(())
}

/// Завершает decoder-side EOF drain только после публикации всех backend-ready frames.
fn complete_decoder_eof_drain_if_ready(
    decoder: &crate::VaapiVideoDecoder,
    pending_publish: Option<&PendingFramePublish>,
    end_of_stream_drain_state: &Arc<Mutex<video_core::VideoDecoderEndOfStreamDrainState>>,
    activity_notifier: &VideoDecoderActivityNotifier,
) -> Result<(), DecodeThreadError> {
    if pending_publish.is_some() || decoder.has_ready_frames() {
        return Ok(());
    }

    let current_state = decoder_eof_drain_state(end_of_stream_drain_state)?;
    if let video_core::VideoDecoderEndOfStreamDrainState::Draining { generation } = current_state {
        set_decoder_eof_drain_state(
            end_of_stream_drain_state,
            video_core::VideoDecoderEndOfStreamDrainState::Drained { generation },
            activity_notifier,
        )?;
    }

    Ok(())
}

/// Обрабатывает release/flush control message без ожидания packet channel.
fn handle_decoder_control_message(
    decoder: &mut crate::VaapiVideoDecoder,
    control_message: ThreadControlMsg,
    control_context: &mut DecoderControlContext<'_>,
) -> bool {
    let packet_rx = control_context.packet_rx;
    let error_tx = control_context.error_tx;
    let pending_publish = &mut *control_context.pending_publish;
    let output_backpressured_packet = &mut *control_context.output_backpressured_packet;
    let end_of_stream_drain_state = control_context.end_of_stream_drain_state;
    let activity_notifier = control_context.activity_notifier;

    match control_message {
        ThreadControlMsg::ConfigureStream(config, done_tx) => {
            release_pending_publish_frame(decoder, pending_publish.take());
            output_backpressured_packet.take();
            let config_result =
                if let Some(rejection) = reject_unsupported_vaapi_stream_config(&config) {
                    video_core::VideoStreamConfigResult::Unsupported(rejection)
                } else {
                    match decoder.configure_stream(&config) {
                        Ok(()) => video_core::VideoStreamConfigResult::Configured,
                        Err(error) => {
                            let fatal_error = DecodeThreadError::new(format!(
                                "Decoder thread failed to configure VA-API stream: {error:#}"
                            ));
                            send_decoder_thread_error(
                                error_tx,
                                fatal_error.message().to_string(),
                                activity_notifier,
                            );
                            video_core::VideoStreamConfigResult::Fatal(fatal_error.into())
                        }
                    }
                };
            let keep_running =
                !matches!(config_result, video_core::VideoStreamConfigResult::Fatal(_));

            if done_tx.send(config_result).is_err() {
                tracing::warn!(
                    "Decoder thread: stream configure completed, but caller dropped receiver"
                );
            }

            keep_running
        }
        ThreadControlMsg::ReleaseZeroCopy(handle) => {
            if let Err(error) = decoder.release_zero_copy_frame(handle) {
                let message = format!("Video decoder zero-copy release failed: {error:#}");
                tracing::warn!(
                    error = %message,
                    handle_id = handle.0,
                    "Decoder thread: fatal zero-copy release error"
                );
                send_decoder_thread_error(error_tx, message, activity_notifier);
                return false;
            }
            true
        }
        ThreadControlMsg::SetPrerollOutputFloor(floor, done_tx) => {
            let result = decoder.set_preroll_output_floor(floor);
            let keep_running =
                !matches!(result, video_core::VideoPrerollOutputFloorResult::Fatal(_));
            if let video_core::VideoPrerollOutputFloorResult::Fatal(error) = &result {
                send_decoder_thread_error(error_tx, error.message().to_string(), activity_notifier);
            }
            if done_tx.send(result).is_err() {
                tracing::warn!(
                    "Decoder thread: preroll output-floor set completed, but caller dropped receiver"
                );
            }
            keep_running
        }
        ThreadControlMsg::ClearPrerollOutputFloor(clear, done_tx) => {
            let result = decoder.clear_preroll_output_floor(clear);
            let keep_running =
                !matches!(result, video_core::VideoPrerollOutputFloorResult::Fatal(_));
            if let video_core::VideoPrerollOutputFloorResult::Fatal(error) = &result {
                send_decoder_thread_error(error_tx, error.message().to_string(), activity_notifier);
            }
            if done_tx.send(result).is_err() {
                tracing::warn!(
                    "Decoder thread: preroll output-floor clear completed, but caller dropped receiver"
                );
            }
            keep_running
        }
        ThreadControlMsg::BeginEndOfStreamDrain(generation, done_tx) => {
            if let Ok(state) = decoder_eof_drain_state(end_of_stream_drain_state)
                && decoder_eof_drain_state_matches_generation(&state, generation)
            {
                if done_tx
                    .send(video_core::VideoDecoderEndOfStreamDrainResult::Unchanged(
                        state,
                    ))
                    .is_err()
                {
                    tracing::warn!(
                        "Decoder thread: EOF drain unchanged, but caller dropped receiver"
                    );
                }
                return true;
            }

            if let Err(error) = set_decoder_eof_drain_state(
                end_of_stream_drain_state,
                video_core::VideoDecoderEndOfStreamDrainState::Draining { generation },
                activity_notifier,
            ) {
                send_decoder_thread_error(error_tx, error.message().to_string(), activity_notifier);
                let _ = done_tx.send(video_core::VideoDecoderEndOfStreamDrainResult::Fatal(
                    error.into(),
                ));
                return false;
            }

            let drain_result = decoder.begin_end_of_stream_drain_for_thread(generation);
            if let Err(error) = drain_result {
                let fatal_error = DecodeThreadError::new(format!(
                    "Decoder thread failed during VA-API EOF drain: {error:#}"
                ));
                let _ = set_decoder_eof_drain_state(
                    end_of_stream_drain_state,
                    video_core::VideoDecoderEndOfStreamDrainState::Fatal {
                        generation: Some(generation),
                        error: fatal_error.clone().into(),
                    },
                    activity_notifier,
                );
                send_decoder_thread_error(
                    error_tx,
                    fatal_error.message().to_string(),
                    activity_notifier,
                );
                let _ = done_tx.send(video_core::VideoDecoderEndOfStreamDrainResult::Fatal(
                    fatal_error.into(),
                ));
                return false;
            }

            if let Err(error) = complete_decoder_eof_drain_if_ready(
                decoder,
                pending_publish.as_ref(),
                end_of_stream_drain_state,
                activity_notifier,
            ) {
                send_decoder_thread_error(error_tx, error.message().to_string(), activity_notifier);
                let _ = done_tx.send(video_core::VideoDecoderEndOfStreamDrainResult::Fatal(
                    error.into(),
                ));
                return false;
            }

            let state =
                decoder_eof_drain_state(end_of_stream_drain_state).unwrap_or_else(|error| {
                    video_core::VideoDecoderEndOfStreamDrainState::Fatal {
                        generation: Some(generation),
                        error: error.into(),
                    }
                });
            if done_tx
                .send(video_core::VideoDecoderEndOfStreamDrainResult::Started(
                    state,
                ))
                .is_err()
            {
                tracing::warn!("Decoder thread: EOF drain completed, but caller dropped receiver");
            }
            true
        }
        ThreadControlMsg::Flush(done_tx) => {
            release_pending_publish_frame(decoder, pending_publish.take());
            output_backpressured_packet.take();
            let dropped_packet_count = drain_queued_decode_packets(packet_rx);
            if dropped_packet_count > 0 {
                tracing::debug!(
                    dropped_packet_count,
                    "Dropped queued decoder packets during flush"
                );
            }
            let flush_result = decoder.flush().map_err(|error| format!("{error:#}"));
            let flush_failed = flush_result.is_err();

            if let Err(error) = &flush_result {
                let message = format!("Video decoder stopped after flush error: {error}");
                tracing::warn!(
                    error = %message,
                    "Decoder thread: fatal flush error, exiting"
                );
                send_decoder_thread_error(error_tx, message, activity_notifier);
            }

            if done_tx.send(flush_result).is_err() {
                tracing::warn!("Decoder thread: flush completed, but caller dropped receiver");
            }

            !flush_failed
        }
    }
}

/// Очищает packet backlog, который был поставлен в decoder до flush/seek.
///
/// Важно чистить именно receiver-side queue: worker после `flush()` уже очистит
/// свои pending packets, но packets, которые успели попасть в decoder channel,
/// иначе будут декодированы после backend flush без старых reference frames.
pub(super) fn drain_queued_decode_packets(packet_rx: &Receiver<QueuedDecodePacket>) -> usize {
    let mut dropped_packet_count = 0usize;
    loop {
        match packet_rx.try_recv() {
            Ok(_queued_packet) => {
                dropped_packet_count = dropped_packet_count.saturating_add(1);
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                return dropped_packet_count;
            }
        }
    }
}

/// Собирает decoder-thread state, который нужен одному packet decode step.
pub(super) struct DecodeQueuedPacketContext<'a> {
    pub(super) frame_tx: &'a Sender<DecodedFrame>,
    pub(super) packet_ack_tx: &'a Sender<DecodePacketAck>,
    pub(super) error_tx: &'a Sender<DecodeThreadError>,
    pub(super) pending_publish: &'a mut Option<PendingFramePublish>,
    pub(super) publish_pressure: &'a mut FramePublishPressureCounters,
    pub(super) diagnostic_tx: &'a DecoderDiagnosticSender,
    pub(super) activity_notifier: &'a VideoDecoderActivityNotifier,
    pub(super) latest_color_metadata: &'a mut Option<VideoColorMetadata>,
}

/// Result одного decode attempt-а внутри decoder thread loop-а.
pub(super) enum DecodeQueuedPacketResult {
    /// Loop может продолжать normal scheduling.
    Continue,

    /// Decode thread должен завершиться.
    Stop,

    /// Packet не принят из-за output-buffer pressure и должен быть повторён позже.
    OutputBackpressured(QueuedDecodePacket),
}

/// Декодирует один queued packet и ставит первый готовый frame в publish stage.
fn decode_queued_packet(
    decoder: &mut crate::VaapiVideoDecoder,
    queued_packet: QueuedDecodePacket,
    decode_context: DecodeQueuedPacketContext<'_>,
) -> DecodeQueuedPacketResult {
    let packet_receive_latency = queued_packet.enqueued_at.elapsed();
    let decode_packet = &queued_packet.packet;
    let packet = Packet::new_with_keyframe_unbounded(
        decode_packet.track_id,
        TrackKind::Video,
        decode_packet.pts,
        decode_packet.dts,
        decode_packet.keyframe.into(),
        decode_packet.encoded_bytes.clone(),
    )
    .with_track_timestamps(None, decode_packet.track_dts);

    let decode_result = decoder.decode_packet_for_thread(&packet, decode_packet.generation);

    handle_decode_packet_outcome(
        decode_result,
        queued_packet,
        packet_receive_latency,
        decode_context,
    )
}

/// Обрабатывает результат backend decode без повторного знания о VAAPI submit path.
pub(super) fn handle_decode_packet_outcome(
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
            tracing::warn!(error = %error, "Decoder thread: decode error");
            DecodeQueuedPacketResult::Continue
        }
    }
}

/// Пытается передать pending frame worker-у, не блокируя release/flush control path.
pub(super) fn publish_pending_frame(
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
pub(super) fn send_frame_publish_pressure_event(
    diagnostic_tx: &DecoderDiagnosticSender,
    pressure: VideoFramePublishPressureDiagnostics,
    activity_notifier: &VideoDecoderActivityNotifier,
) {
    let _ = diagnostic_tx
        .try_send(VideoDecoderDiagnosticEvent::DecodedFramePublishPressure { pressure });
    notify_decoder_activity(activity_notifier);
}

/// Освобождает frame, который decoder уже импортировал, но не успел отдать worker-у.
fn release_pending_publish_frame(
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

/// Отправляет fatal decoder-thread error без блокировки.
pub(super) fn send_decoder_thread_error(
    error_tx: &Sender<DecodeThreadError>,
    message: String,
    activity_notifier: &VideoDecoderActivityNotifier,
) {
    if error_tx.try_send(DecodeThreadError::new(message)).is_err() {
        trace!("Player thread dropped decoder error receiver");
    }
    notify_decoder_activity(activity_notifier);
}

/// Продвигает neutral activity epoch, не делая disconnect receiver fatal для decoder thread-а.
fn notify_decoder_activity(activity_notifier: &VideoDecoderActivityNotifier) {
    let _ = activity_notifier.notify_activity();
}
