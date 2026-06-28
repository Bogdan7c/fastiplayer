//! Decoder I/O scheduler внутри session tick boundary.
//!
//! Модуль не владеет seek lifecycle и не решает, когда session должна начинать
//! или завершать seek. Его ответственность уже: синхронизировать worker-side
//! очереди с decoder thread, принять decoded frames, валидировать packets перед
//! отправкой и сохранить typed причины no-op/backpressure/fatal/drop.

use std::time::{Duration, Instant};

use bytes::Bytes;
#[cfg(test)]
use codec_core::video_frame_pixel_layout_from_decode_requirement;
use codec_core::{
    VideoCodec, VideoDecodeRequirement, VideoRequirementProbe, VideoRequirementRejection,
    probe_video_packet_requirement_with_codec_private, resolve_video_metadata,
    video_requirement_needs_packet_refinement,
};
use media_core::{PacketKeyframe, TrackId};
use tracing::{debug, trace, warn};
use video_core::HostUploadBackpressureReason;

use super::{
    PlayerTickResult, PlayerVideoDropReason,
    demux_admission::catch_up_deadline_reached,
    presentation_scheduler::{
        drop_seek_preroll_fallback_frame, release_video_texture,
        replace_seek_preroll_fallback_frame,
    },
    record_pipeline_pause, record_video_drop,
};
use crate::{
    DecodeSendError, DecodeThreadError, PipelinePauseReason, PlaybackPipeline, PlayerDecodePacket,
    PlayerError, PlayerErrorKind, pipeline::VideoDecoderSendBackpressure, session::PlayerSession,
};

/// Typed лимит texture/surface pressure для decoder admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VideoDecoderTextureLimits {
    /// Минимальный резерв свободных slots, который decode не должен проедать.
    min_slots_available_for_decode: usize,
}

impl VideoDecoderTextureLimits {
    /// Создаёт texture limits без доступа к user config внутри decoder I/O.
    pub(super) const fn new(min_slots_available_for_decode: usize) -> Self {
        Self {
            min_slots_available_for_decode,
        }
    }
}

/// Typed лимит decode-ahead относительно audio clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VideoDecoderDecodeAheadLimits {
    /// Лимит текущего режима admission: steady-state target или catch-up max.
    admission_limit: Duration,

    /// Жёсткий максимум из scheduler policy.
    max_limit: Duration,
}

impl VideoDecoderDecodeAheadLimits {
    /// Создаёт decode-ahead limits для текущего режима отправки packets.
    pub(super) const fn new(admission_limit: Duration, max_limit: Duration) -> Self {
        Self {
            admission_limit,
            max_limit,
        }
    }

    /// Возвращает эффективный лимит, сохраняя старое fail-closed ограничение max.
    fn effective_limit(self) -> Duration {
        self.admission_limit.min(self.max_limit)
    }
}

/// Typed лимиты одного прохода decoder I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VideoDecoderIoLimits {
    /// Сколько decoded frames можно принять из decoder thread-а.
    max_frames_to_drain: usize,

    /// Сколько pending packets можно отправить decoder thread-у.
    max_packets_to_send: usize,

    /// Безопасный размер presentation queue для admission/drop policy.
    present_queue_limit: usize,

    /// Bounded размер software host-upload ready queue.
    host_upload_ready_queue_capacity: usize,

    /// Texture/surface pressure limits.
    texture: VideoDecoderTextureLimits,

    /// Decode-ahead limits для packet send admission.
    decode_ahead: VideoDecoderDecodeAheadLimits,
}

impl VideoDecoderIoLimits {
    /// Создаёт полный набор лимитов без чтения `PlayerTickConfig` в decoder I/O.
    pub(super) const fn new(
        max_frames_to_drain: usize,
        max_packets_to_send: usize,
        present_queue_limit: usize,
        host_upload_ready_queue_capacity: usize,
        texture: VideoDecoderTextureLimits,
        decode_ahead: VideoDecoderDecodeAheadLimits,
    ) -> Self {
        Self {
            max_frames_to_drain,
            max_packets_to_send,
            present_queue_limit,
            host_upload_ready_queue_capacity,
            texture,
            decode_ahead,
        }
    }
}

/// Runtime context одного прохода decoder I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VideoDecoderIoContext {
    /// Deadline bounded catch-up прохода; `None` означает обычный tick.
    catch_up_deadline: Option<Instant>,

    /// Нужно ли освободить stale present frame для final seek texture pressure.
    release_stale_final_seek_frame: bool,

    /// Нужно ли после drain попытаться отправить pending video packets.
    send_pending_packets: bool,
}

impl VideoDecoderIoContext {
    /// Создаёт context без доступа scheduler-а к storage detail-ам config-а.
    pub(super) const fn new(
        catch_up_deadline: Option<Instant>,
        release_stale_final_seek_frame: bool,
        send_pending_packets: bool,
    ) -> Self {
        Self {
            catch_up_deadline,
            release_stale_final_seek_frame,
            send_pending_packets,
        }
    }
}

/// Счётчики progress одного decoder I/O прохода.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct VideoDecoderIoProgress {
    /// Сколько decoded frames было снято с decoder boundary.
    pub(super) decoded_frames_drained: usize,

    /// Сколько packets было принято decoder thread-ом.
    pub(super) packets_sent: usize,
}

/// Выполняет bounded decoder I/O pass для обычного tick-а.
pub(super) fn run_video_decoder_io(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    limits: VideoDecoderIoLimits,
    context: VideoDecoderIoContext,
) -> VideoDecoderIoProgress {
    if context.release_stale_final_seek_frame {
        session.release_stale_present_frame_for_final_seek_texture_pressure(
            limits.texture.min_slots_available_for_decode,
        );
    }

    let decoded_frames_drained =
        drain_decoded_video_frames(session, tick_result, limits, context.catch_up_deadline);

    let packets_sent = if context.send_pending_packets
        && session.can_present_video()
        && (session.playback_state().is_playback_active() || session.seek_landing_decode_active())
    {
        send_pending_video_packets_to_decoder(
            session,
            tick_result,
            limits,
            context.catch_up_deadline,
        )
    } else {
        0
    };

    VideoDecoderIoProgress {
        decoded_frames_drained,
        packets_sent,
    }
}

/// Проверяет, можно ли отправить video packet в decoder thread без чрезмерного decode-ahead.
pub(super) fn can_send_video_packet_to_decoder(
    session: &PlayerSession,
    texture_limits: VideoDecoderTextureLimits,
    decode_ahead_limits: VideoDecoderDecodeAheadLimits,
    host_upload_ready_queue_capacity: usize,
    bypass_texture_capacity: bool,
    packet_pts: Duration,
) -> bool {
    if session
        .pipeline
        .video_decoder_send_backpressure(host_upload_ready_queue_capacity)
        .is_some()
    {
        return false;
    }

    if !bypass_texture_capacity && !has_texture_capacity_for_decode(session, texture_limits) {
        return false;
    }

    if session.should_bypass_audio_clock_decode_ahead_for_active_seek() {
        return true;
    }

    if !session.pipeline.has_selected_audio_track() || !session.pipeline.has_audio_clock() {
        return true;
    }

    let media_audio_now = session.pipeline.media_position_from_audio_clock();
    let packet_lead = packet_pts.saturating_sub(media_audio_now);

    packet_lead <= decode_ahead_limits.effective_limit()
}

/// Проверяет запас texture slots для ещё одного decoded frame.
pub(super) fn has_texture_capacity_for_decode(
    session: &PlayerSession,
    texture_limits: VideoDecoderTextureLimits,
) -> bool {
    texture_capacity_backpressure_reason(session, texture_limits).is_none()
}

/// Диагностирует neutral decoder send backpressure без доступа к concrete backend-у.
pub(super) fn decoder_send_backpressure_pause_reason(
    session: &PlayerSession,
    host_upload_ready_queue_capacity: usize,
) -> Option<PipelinePauseReason> {
    match session
        .pipeline
        .video_decoder_send_backpressure(host_upload_ready_queue_capacity)?
    {
        VideoDecoderSendBackpressure::AbsentDecoder => None,
        VideoDecoderSendBackpressure::HostUpload(reason) => {
            Some(host_upload_backpressure_pause_reason(reason))
        }
        VideoDecoderSendBackpressure::DecoderControl(_) => {
            Some(PipelinePauseReason::DecoderControlQueueFull)
        }
    }
}

/// Маппит software host-upload pressure в player diagnostics, не смешивая причины.
fn host_upload_backpressure_pause_reason(
    reason: HostUploadBackpressureReason,
) -> PipelinePauseReason {
    match reason {
        HostUploadBackpressureReason::ReadyQueueFull { .. } => {
            PipelinePauseReason::HostUploadReadyQueueFull
        }
        HostUploadBackpressureReason::UploadSlotsExhausted { .. } => {
            PipelinePauseReason::HostUploadSlotsExhausted
        }
    }
}

/// Диагностирует, почему texture/surface pool не даёт отправить новый packet.
pub(super) fn texture_capacity_backpressure_reason(
    session: &PlayerSession,
    texture_limits: VideoDecoderTextureLimits,
) -> Option<PipelinePauseReason> {
    let stats = session.pipeline.video_decoder_resource_snapshot()?;

    let available_slots = stats.available_slots();
    if available_slots > texture_limits.min_slots_available_for_decode {
        return None;
    }

    trace!(
        texture_slots = stats.slots,
        texture_in_use = stats.in_use,
        texture_capacity = stats.capacity,
        available_slots,
        reserve = texture_limits.min_slots_available_for_decode,
        waiting_gpu_completion = stats.waiting_gpu_completion,
        waiting_decoder_reuse = stats.waiting_decoder_reuse,
        "Video backpressure: waiting for texture slots"
    );

    if stats.waiting_gpu_completion > 0 || stats.waiting_decoder_reuse > 0 {
        Some(PipelinePauseReason::WaitingForGpuRelease)
    } else {
        Some(PipelinePauseReason::WaitingForFreeSurface)
    }
}

/// Забирает готовые кадры из decoder thread и кладёт их в presentation queue.
pub(super) fn drain_decoded_video_frames(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    limits: VideoDecoderIoLimits,
    catch_up_deadline: Option<Instant>,
) -> usize {
    drain_completed_video_decode_packets(session);
    drain_video_decoder_thread_diagnostics(session, tick_result);

    if drain_video_decoder_thread_error(session) {
        return 0;
    }

    if !session.pipeline.can_receive_decoded_video_frames() {
        if !session.pipeline.pending_video_packet_is_empty() {
            tracing::warn!(
                count = session.pipeline.pending_video_packet_len(),
                "No video decoder thread — dropping video packets"
            );
        }
        while let Some(packet) = session.pipeline.pop_pending_video_packet_front() {
            record_video_drop(
                session,
                tick_result,
                packet.pts,
                PlayerVideoDropReason::DecoderStarvation,
            );
        }
        session.pipeline.reset_video_decode_in_flight();
        return 0;
    }

    let playback_can_present = session.can_present_video();
    let receive_budget = decoded_frame_receive_budget(session, limits);

    if receive_budget == 0 {
        return 0;
    }

    let mut decoded_frames = Vec::new();
    for _ in 0..receive_budget {
        if catch_up_deadline_reached(catch_up_deadline) {
            break;
        }
        let Some(frame) = session.pipeline.try_recv_decoded_video_frame() else {
            break;
        };
        decoded_frames.push(frame);
    }

    if drain_video_decoder_thread_error(session) {
        for frame in decoded_frames {
            release_video_texture(session, frame.resource_handle);
        }
        return 0;
    }
    drain_video_decoder_thread_diagnostics(session, tick_result);

    let drained_frame_count = decoded_frames.len();
    let mut seek_preroll_frames_handled = 0usize;
    for frame in decoded_frames {
        if !session
            .pipeline
            .packet_generation_is_current(frame.generation)
        {
            release_video_texture(session, frame.resource_handle);
            record_video_drop(
                session,
                tick_result,
                frame.pts,
                PlayerVideoDropReason::StaleGeneration,
            );
            tracing::debug!(
                frame_generation = frame.generation,
                pipeline_generation = session.pipeline.seek_generation(),
                pts_ms = frame.pts.as_millis(),
                "Dropping decoded video frame from stale seek generation"
            );
            continue;
        }

        if let Some(expected_contract) = session.pipeline.active_video_frame_contract()
            && let Err(error) = frame.validate_against_expected_contract(expected_contract)
        {
            release_video_texture(session, frame.resource_handle);
            session.mark_fatal_error(crate::PlayerError::new(
                crate::PlayerErrorKind::RuntimeError,
                format!("Decoded video frame contract mismatch: {error}"),
            ));
            tracing::error!(
                error = %error,
                expected_contract = %expected_contract,
                actual_contract = %frame.frame_contract,
                pts_ms = frame.pts.as_millis(),
                "Decoded video frame does not match active stream contract"
            );
            return drained_frame_count;
        }

        session.note_decoded_video_frame_for_seek_trace(frame.pts, frame.generation);
        tracing::debug!(
            generation = frame.generation,
            pts_ms = frame.pts.as_millis(),
            format = %frame.format(),
            bit_depth = ?frame.bit_depth(),
            memory_path = %frame.memory_path(),
            width = frame.width,
            height = frame.height,
            "Video frame decoded"
        );
        tick_result.record_decoded_video_frame();
        session.record_decoded_frame_diagnostics(&frame);

        let frame_pts = frame.pts;
        if session.should_drop_decoded_frame_for_seek(frame_pts) {
            session.note_decoded_pre_target_frame_dropped_for_seek_diagnostics();
            if session.can_keep_seek_preroll_fallback(frame_pts) {
                replace_seek_preroll_fallback_frame(session, tick_result, frame);
            } else {
                release_video_texture(session, frame.resource_handle);
                record_video_drop(
                    session,
                    tick_result,
                    frame_pts,
                    PlayerVideoDropReason::SeekPreroll,
                );
            }
            seek_preroll_frames_handled = seek_preroll_frames_handled.saturating_add(1);
            tracing::debug!(
                pts_ms = frame_pts.as_millis(),
                "Retaining or dropping pre-roll video frame before seek target"
            );
            continue;
        }

        drop_seek_preroll_fallback_frame(session, tick_result);
        session.observe_video_frame_pts(frame.pts);

        if playback_can_present {
            enqueue_decoded_video_frame(session, tick_result, limits, frame);
        } else {
            release_video_texture(session, frame.resource_handle);
            record_video_drop(
                session,
                tick_result,
                frame.pts,
                PlayerVideoDropReason::Paused,
            );
            tracing::debug!("Dropping decoded frame received while playback is paused");
        }
    }

    if seek_preroll_frames_handled > 0 {
        debug!(
            seek_preroll_frames_handled,
            decoded_frames_drained = drained_frame_count,
            pending_video_packets = session.pipeline.pending_video_packet_len(),
            decoder_in_flight_packets = session.pipeline.video_decode_in_flight_packets(),
            present_queue_depth = session.pipeline.video_present_queue_len(),
            "Accurate seek preroll decoded frames handled before target"
        );
    }

    drained_frame_count
}

/// Отправляет ограниченное число pending video packets в decoder thread.
pub(super) fn send_pending_video_packets_to_decoder(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    limits: VideoDecoderIoLimits,
    catch_up_deadline: Option<Instant>,
) -> usize {
    if let Some(reason) =
        decoder_send_backpressure_pause_reason(session, limits.host_upload_ready_queue_capacity)
    {
        session.note_decoder_backpressure_for_seek_preroll_diagnostics();
        record_pipeline_pause(session, tick_result, reason);
        return 0;
    }

    let mut sent_packets = 0usize;
    let mut seek_preroll_packets_sent = 0usize;
    let present_admission_budget = video_packet_send_present_admission_budget(session, limits);

    while sent_packets < limits.max_packets_to_send {
        if catch_up_deadline_reached(catch_up_deadline) {
            break;
        }

        if present_admission_budget == 0 {
            session.note_decoder_backpressure_for_seek_preroll_diagnostics();
            record_pipeline_pause(
                session,
                tick_result,
                PipelinePauseReason::WaitingForPresentQueue,
            );
            break;
        }
        if sent_packets >= present_admission_budget {
            break;
        }

        let Some(packet) = session.pipeline.front_pending_video_packet() else {
            break;
        };
        let packet_track_id = packet.track_id;
        let packet_pts = packet.pts;
        let packet_dts = packet.dts;
        let packet_track_dts = packet.track_dts;
        let packet_generation = packet.generation;
        let packet_keyframe = packet.keyframe;
        let encoded_bytes = packet.encoded_bytes.clone();

        if let Some(drop_reason) =
            pending_video_packet_generation_drop_reason(&session.pipeline, packet_generation)
        {
            session.pipeline.pop_pending_video_packet_front();
            record_video_drop(session, tick_result, packet_pts, drop_reason);
            continue;
        }

        if !session
            .pipeline
            .video_packet_belongs_to_selected_track(packet_track_id)
        {
            session.pipeline.pop_pending_video_packet_front();
            continue;
        }

        let decoder_was_waiting_for_keyframe = session.pipeline.video_decoder_needs_keyframe();
        if !accept_video_packet_for_decoder_bootstrap(session, packet_keyframe, packet_pts) {
            session.pipeline.pop_pending_video_packet_front();
            record_video_drop(
                session,
                tick_result,
                packet_pts,
                PlayerVideoDropReason::SeekPreroll,
            );
            continue;
        }

        let packet_probe = PendingVideoPacketProbe {
            track_id: packet_track_id,
            encoded_bytes,
        };
        if !validate_pending_video_packet_before_decode(session, &packet_probe) {
            break;
        }

        if let Some(reason) =
            decoder_send_backpressure_pause_reason(session, limits.host_upload_ready_queue_capacity)
        {
            session.note_decoder_backpressure_for_seek_preroll_diagnostics();
            record_pipeline_pause(session, tick_result, reason);
            break;
        }

        let packet_bypasses_texture_capacity = session
            .decoder_output_floor_applies_to_seek_preroll_packet(packet_pts, packet_generation);
        if !packet_bypasses_texture_capacity
            && let Some(reason) = texture_capacity_backpressure_reason(session, limits.texture)
        {
            session.note_decoder_backpressure_for_seek_preroll_diagnostics();
            record_pipeline_pause(session, tick_result, reason);
            break;
        }

        let packet_is_seek_preroll =
            session.should_fast_decode_video_packet_for_seek_preroll(packet_pts);

        if !can_send_video_packet_to_decoder(
            session,
            limits.texture,
            limits.decode_ahead,
            limits.host_upload_ready_queue_capacity,
            packet_bypasses_texture_capacity,
            packet_pts,
        ) {
            session.note_decoder_backpressure_for_seek_preroll_diagnostics();
            trace!(
                pts_ms = packet_pts.as_millis(),
                audio_ms = session.audio_clock_now().as_millis(),
                decode_ahead_limit_ms = limits.decode_ahead.admission_limit.as_millis(),
                max_ahead_ms = limits.decode_ahead.max_limit.as_millis(),
                "A/V sync: holding video packet to limit decode-ahead"
            );
            break;
        }

        let decode_packet_keyframe_hint =
            video_decode_packet_keyframe_hint(packet_keyframe, decoder_was_waiting_for_keyframe);
        trace!(
            pts_ms = packet_pts.as_millis(),
            encoded_len = packet_probe.encoded_bytes.len(),
            keyframe = ?packet_keyframe,
            decode_keyframe_hint = decode_packet_keyframe_hint,
            "Sending video packet to decoder thread"
        );

        let resolved_color = session
            .pipeline
            .active_video_requirement()
            .and_then(|requirement| requirement.color.clone());
        let decode_packet = PlayerDecodePacket {
            track_id: packet_track_id,
            pts: packet_pts,
            dts: packet_dts,
            track_dts: packet_track_dts,
            generation: packet_generation,
            encoded_bytes: packet_probe.encoded_bytes,
            keyframe: decode_packet_keyframe_hint,
            resolved_color,
        };

        match session.pipeline.send_video_decode_packet(decode_packet) {
            None => break,
            Some(Ok(())) => {
                session.pipeline.pop_pending_video_packet_front();
                session.pipeline.note_video_packet_sent_to_decoder();
                if packet_is_seek_preroll {
                    session.note_video_preroll_packet_sent_for_seek_diagnostics();
                    seek_preroll_packets_sent = seek_preroll_packets_sent.saturating_add(1);
                } else {
                    session.note_target_or_after_video_packet_sent_for_seek_diagnostics();
                }
            }
            Some(Err(DecodeSendError::Backpressure(reason))) => {
                session.note_decoder_backpressure_for_seek_preroll_diagnostics();
                tracing::debug!(reason = %reason, "Decoder packet channel backpressure");
                record_pipeline_pause(
                    session,
                    tick_result,
                    PipelinePauseReason::DecoderPacketQueueFull,
                );
                break;
            }
            Some(Err(DecodeSendError::Fatal(error))) => {
                tracing::warn!(error = %error, "Failed to send packet to decoder thread");
                session.mark_fatal_error(PlayerError::new(
                    PlayerErrorKind::RuntimeError,
                    format!("Video decoder thread stopped before accepting packet: {error}"),
                ));
                break;
            }
        }

        sent_packets += 1;
    }

    if seek_preroll_packets_sent > 0 {
        debug!(
            seek_preroll_packets_sent,
            total_packets_sent = sent_packets,
            pending_video_packets = session.pipeline.pending_video_packet_len(),
            decoder_in_flight_packets = session.pipeline.video_decode_in_flight_packets(),
            "Accurate seek preroll video packets sent to decoder"
        );
    }

    sent_packets
}

/// Пропускает inter-frames, пока decoder после flush ждёт новый keyframe.
pub(super) fn accept_video_packet_for_decoder_bootstrap(
    session: &mut PlayerSession,
    packet_keyframe: PacketKeyframe,
    packet_pts: Duration,
) -> bool {
    if !session.pipeline.video_decoder_needs_keyframe() {
        return true;
    }

    match packet_keyframe {
        PacketKeyframe::Keyframe => {
            let bootstrap = session.record_video_decoder_bootstrap_accepted(packet_keyframe);
            debug!(
                pts_ms = packet_pts.as_millis(),
                dropped_until_keyframe = bootstrap.dropped_until_keyframe,
                first_accepted_keyframe = ?bootstrap.first_accepted_keyframe,
                "Accepted post-flush video decoder bootstrap packet"
            );
            session.pipeline.mark_video_decoder_bootstrapped();
            true
        }
        PacketKeyframe::NotKeyframe => {
            let bootstrap = session.record_video_packet_dropped_until_keyframe();
            debug!(
                pts_ms = packet_pts.as_millis(),
                dropped_until_keyframe = bootstrap.dropped_until_keyframe,
                "Dropping video packet until decoder receives post-flush keyframe"
            );
            false
        }
        PacketKeyframe::Unknown => {
            if active_video_codec_requires_proven_decode_start(session) {
                let bootstrap = session.record_video_packet_dropped_until_keyframe();
                warn!(
                    pts_ms = packet_pts.as_millis(),
                    dropped_until_keyframe = bootstrap.dropped_until_keyframe,
                    "Dropping video packet with unknown keyframe state until codec-aware decode-start proof"
                );
                return false;
            }

            let bootstrap = session.record_video_decoder_bootstrap_accepted(packet_keyframe);
            warn!(
                pts_ms = packet_pts.as_millis(),
                dropped_until_keyframe = bootstrap.dropped_until_keyframe,
                first_accepted_keyframe = ?bootstrap.first_accepted_keyframe,
                "Accepting video packet with unknown keyframe state as post-flush decode start"
            );
            session.pipeline.mark_video_decoder_bootstrapped();
            true
        }
    }
}

/// Проверяет, требует ли активный codec доказанный decode-start после seek/flush.
fn active_video_codec_requires_proven_decode_start(session: &PlayerSession) -> bool {
    session
        .pipeline
        .active_video_requirement()
        .is_some_and(|requirement| matches!(requirement.codec, VideoCodec::H264 | VideoCodec::H265))
}

/// Отделяет stale seek generation от late-drop policy.
pub(super) fn pending_video_packet_generation_drop_reason(
    pipeline: &PlaybackPipeline,
    packet_generation: u64,
) -> Option<PlayerVideoDropReason> {
    (!pipeline.packet_generation_is_current(packet_generation))
        .then_some(PlayerVideoDropReason::StaleGeneration)
}

/// Возвращает количество свободных мест в presentation queue.
fn available_video_present_slots(session: &PlayerSession, limits: VideoDecoderIoLimits) -> usize {
    limits
        .present_queue_limit
        .saturating_sub(session.pipeline.video_present_queue_len())
}

/// Возвращает bounded budget приёма decoded frames для текущего admission mode.
fn decoded_frame_receive_budget(session: &PlayerSession, limits: VideoDecoderIoLimits) -> usize {
    if session.has_active_seek_commit() {
        return limits.max_frames_to_drain;
    }

    if session.can_present_video() {
        return available_video_present_slots(session, limits).min(limits.max_frames_to_drain);
    }

    limits.max_frames_to_drain
}

/// Возвращает bounded budget отправки packets в decoder для текущего admission mode.
fn video_packet_send_present_admission_budget(
    session: &PlayerSession,
    limits: VideoDecoderIoLimits,
) -> usize {
    if session.has_active_seek_commit() {
        return limits.max_packets_to_send;
    }

    available_video_present_slots(session, limits).min(limits.max_packets_to_send)
}

/// Кладёт decoded frame в presentation queue, сохраняя фиксированный размер очереди.
fn enqueue_decoded_video_frame(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    limits: VideoDecoderIoLimits,
    frame: video_core::DecodedFrame,
) {
    while session.pipeline.video_present_queue_len() >= limits.present_queue_limit {
        let Some(stale_frame) = session.pipeline.pop_queued_video_frame_front() else {
            break;
        };

        release_video_texture(session, stale_frame.resource_handle);
        record_video_drop(
            session,
            tick_result,
            stale_frame.pts,
            PlayerVideoDropReason::QueueOverflow,
        );
        tracing::debug!(
            pts_ms = stale_frame.pts.as_millis(),
            queue_limit = limits.present_queue_limit,
            "Dropping oldest queued video frame before enqueue"
        );
    }

    let frame_pts = frame.pts;
    let frame_generation = frame.generation;
    session.pipeline.enqueue_queued_video_frame(frame);
    session.note_queued_video_frame_for_seek_trace(frame_pts, frame_generation);
}

/// Забирает fatal decoder-thread error и переводит его в состояние player session.
fn drain_video_decoder_thread_error(session: &mut PlayerSession) -> bool {
    let mut fatal_decoder_error = None;

    while let Some(error) = session.pipeline.try_recv_video_decoder_error() {
        fatal_decoder_error = Some(error);
    }

    let Some(error) = fatal_decoder_error else {
        return false;
    };

    session.pipeline.clear_pending_video_packets();
    session.pipeline.reset_video_decode_in_flight();
    session.mark_fatal_error(player_error_from_decode_thread_error(&error));
    true
}

/// Забирает typed diagnostics events от decoder/backend boundary.
fn drain_video_decoder_thread_diagnostics(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
) {
    let mut matching_suppressed_preroll_frames = 0u64;
    loop {
        let diagnostic_event = session.pipeline.try_recv_video_decoder_diagnostic_event();
        let Some(event) = diagnostic_event else {
            break;
        };
        match event {
            video_core::VideoDecoderDiagnosticEvent::FrameDropped { pts, reason } => {
                let drop_reason = match reason {
                    video_core::VideoDecoderDropReason::ReadyQueueOverflow => {
                        PlayerVideoDropReason::QueueOverflow
                    }
                };
                record_video_drop(session, tick_result, pts, drop_reason);
            }
            video_core::VideoDecoderDiagnosticEvent::SeekPrerollFrameSuppressed {
                pts,
                generation,
                floor_pts,
            } => {
                if session
                    .note_suppressed_preroll_frame_for_seek_diagnostics(pts, generation, floor_pts)
                {
                    matching_suppressed_preroll_frames =
                        matching_suppressed_preroll_frames.saturating_add(1);
                }
            }
            video_core::VideoDecoderDiagnosticEvent::DecodedFramePublishPressure { pressure } => {
                session.record_decoded_frame_publish_pressure(pressure);
            }
        }
    }

    if matching_suppressed_preroll_frames > 0 {
        debug!(
            suppressed_preroll_frames = matching_suppressed_preroll_frames,
            "Accurate seek decoder output floor suppressed preroll frames"
        );
    }
}

/// Мапит fail-closed ошибку decoder thread в player error model.
pub(super) fn player_error_from_decode_thread_error(error: &DecodeThreadError) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::RuntimeError,
        format!("Video decoder thread failed: {}", error.message()),
    )
}

/// Синхронизирует player-side in-flight счётчик с packet ack-ами decoder thread-а.
fn drain_completed_video_decode_packets(session: &mut PlayerSession) {
    let completed_packet_count = session.pipeline.drain_completed_video_decode_packet_count();

    session
        .pipeline
        .note_video_packets_completed_by_decoder(completed_packet_count);
}

/// Переводит typed demux-классификацию в текущий bool contract decoder-а.
///
/// `Unknown` становится decode-start hint только если codec policy уже разрешила
/// такой bootstrap. Для H.264/H.265 этот путь закрыт выше codec-aware проверкой.
fn video_decode_packet_keyframe_hint(
    packet_keyframe: PacketKeyframe,
    accepted_as_decode_start: bool,
) -> bool {
    matches!(packet_keyframe, PacketKeyframe::Keyframe)
        || (accepted_as_decode_start && packet_keyframe == PacketKeyframe::Unknown)
}

/// Минимальный view pending packet-а для bitstream capability validation.
struct PendingVideoPacketProbe {
    /// Track ID нужен, чтобы найти container codec.
    track_id: TrackId,

    /// Codec payload нужен adapter-у для чтения header-level requirement.
    encoded_bytes: Bytes,
}

/// Проверяет profile/format до отправки packet-а в hardware decoder.
fn validate_pending_video_packet_before_decode(
    session: &mut PlayerSession,
    packet: &PendingVideoPacketProbe,
) -> bool {
    if session
        .pipeline
        .active_video_requirement()
        .is_some_and(|requirement| !video_requirement_needs_packet_refinement(requirement))
    {
        return true;
    }

    let requirement = match video_requirement_from_packet(session, packet) {
        Ok(Some(requirement)) => requirement,
        Ok(None) => return true,
        Err(error) => {
            warn!(error = %error, "Video stream rejected by packet requirement probe");
            session.mark_fatal_error(error);
            session.pipeline.clear_pending_video_packets();
            return false;
        }
    };

    match session.refine_active_video_requirement(requirement) {
        Ok(()) => true,
        Err(error) => {
            warn!(error = %error, "Video stream rejected before hardware decode");
            session.mark_fatal_error(error);
            session.pipeline.clear_pending_video_packets();
            false
        }
    }
}

/// Читает codec header через adapter registry и строит уточнённое requirement.
fn video_requirement_from_packet_data(
    codec: VideoCodec,
    packet_data: &[u8],
    codec_private: Option<&[u8]>,
    container_source: Option<codec_core::VideoMetadataSource>,
) -> Result<Option<VideoDecodeRequirement>, PlayerError> {
    match probe_video_packet_requirement_with_codec_private(codec, packet_data, codec_private) {
        VideoRequirementProbe::Candidate(candidate) => Ok(Some(
            resolve_video_metadata(codec, container_source, Some(candidate)).requirement,
        )),
        VideoRequirementProbe::Rejected(rejection) => {
            Err(player_error_from_requirement_rejection(rejection))
        }
        VideoRequirementProbe::Recoverable(uncertainty) => {
            trace!(
                ?uncertainty,
                "Video requirement probe skipped before decode"
            );
            Ok(None)
        }
    }
}

/// Переводит codec adapter reject в player error без generic hardware wording.
fn player_error_from_requirement_rejection(rejection: VideoRequirementRejection) -> PlayerError {
    let kind = match rejection {
        VideoRequirementRejection::UnsupportedBitDepth { .. } => {
            PlayerErrorKind::UnsupportedVideoBitDepth
        }
        VideoRequirementRejection::UnsupportedChroma { .. } => {
            PlayerErrorKind::UnsupportedVideoChroma
        }
        VideoRequirementRejection::UnsupportedChromaFormat { .. } => {
            PlayerErrorKind::UnsupportedVideoChroma
        }
        VideoRequirementRejection::UnsupportedProfile { .. } => {
            PlayerErrorKind::UnsupportedVideoProfile
        }
        VideoRequirementRejection::UnsupportedPacketization { .. } => {
            PlayerErrorKind::UnsupportedVideoCodec
        }
        VideoRequirementRejection::UnsupportedCodecAdapter { .. } => {
            PlayerErrorKind::UnsupportedVideoCodec
        }
    };

    PlayerError::new(kind, rejection.user_message())
}

/// Возвращает codec-specific requirement probe через adapter registry.
fn video_requirement_from_packet(
    session: &PlayerSession,
    packet: &PendingVideoPacketProbe,
) -> Result<Option<VideoDecodeRequirement>, PlayerError> {
    let Some(codec) = session.video_codec_for_track(packet.track_id) else {
        return Ok(None);
    };

    video_requirement_from_packet_data(
        codec,
        &packet.encoded_bytes,
        session.video_codec_private_for_track(packet.track_id),
        session.video_metadata_source_for_track(packet.track_id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec_core::{BitDepth, ChromaSubsampling, H264Profile, VideoProfile};
    use media_core::{
        DemuxSeekResult, Demuxer, Packet, TimeBase, TrackInfo, TrackKind, VideoTrackMetadata,
    };
    use video_frame_contract::VideoFramePixelLayout;

    #[test]
    fn h264_packet_refinement_uses_codec_private_for_avcc_packets() {
        let codec_private = supported_h264_high_avcc_codec_private();
        let ambiguous_avcc_packet = avcc_packet_with_annex_b_like_length_prefix();

        let false_rejection = video_requirement_from_packet_data(
            VideoCodec::H264,
            &ambiguous_avcc_packet,
            None,
            None,
        )
        .expect_err("без avcC AVCC packet ошибочно выглядит как unsupported SPS");
        assert_eq!(
            false_rejection.kind,
            PlayerErrorKind::UnsupportedVideoProfile
        );

        let refined_requirement = video_requirement_from_packet_data(
            VideoCodec::H264,
            &ambiguous_avcc_packet,
            Some(&codec_private),
            None,
        )
        .expect("valid avcC должен уточнить H.264 requirement")
        .expect("valid avcC должен вернуть candidate requirement");

        assert_eq!(
            refined_requirement.profile,
            Some(VideoProfile::H264(H264Profile::High))
        );
        assert_eq!(refined_requirement.bit_depth, Some(BitDepth::Eight));
        assert_eq!(refined_requirement.chroma, Some(ChromaSubsampling::Yuv420));
        assert_eq!(refined_requirement.width, Some(1280));
        assert_eq!(refined_requirement.height, Some(720));
        assert_eq!(
            video_frame_pixel_layout_from_decode_requirement(&refined_requirement),
            Some(VideoFramePixelLayout::Nv12)
        );
    }

    #[test]
    fn h264_packet_refinement_reads_codec_private_from_session_track() {
        let track_id = TrackId::new(42);
        let codec_private = supported_h264_high_avcc_codec_private();
        let video_track = h264_video_track(track_id, codec_private.clone());
        let mut session = PlayerSession::new();
        session.pipeline.install_opened_media(
            Box::new(NoopDemuxer {
                tracks: vec![video_track.clone()],
            }),
            None,
            None,
            vec![video_track],
        );

        let packet = PendingVideoPacketProbe {
            track_id,
            encoded_bytes: Bytes::from(avcc_packet_with_annex_b_like_length_prefix()),
        };
        let refined_requirement = video_requirement_from_packet(&session, &packet)
            .expect("session track codec_private должен быть передан в H.264 probe")
            .expect("valid avcC должен вернуть refined requirement");

        assert_eq!(
            refined_requirement.profile,
            Some(VideoProfile::H264(H264Profile::High))
        );
        assert_eq!(
            video_frame_pixel_layout_from_decode_requirement(&refined_requirement),
            Some(VideoFramePixelLayout::Nv12)
        );
    }

    struct NoopDemuxer {
        tracks: Vec<TrackInfo>,
    }

    impl Demuxer for NoopDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &self.tracks
        }

        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(1))
        }

        fn next_packet(&mut self) -> anyhow::Result<Option<Packet>> {
            Ok(None)
        }

        fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            Err(anyhow::anyhow!(
                "NoopDemuxer не поддерживает seek в этом тесте"
            ))
        }
    }

    fn h264_video_track(track_id: TrackId, codec_private: Vec<u8>) -> TrackInfo {
        TrackInfo {
            id: track_id,
            kind: TrackKind::Video,
            codec_id: "V_MPEG4/ISO/AVC".to_owned(),
            codec_private: Some(Bytes::from(codec_private)),
            time_base: TimeBase::new(1, 1_000),
            duration: Some(Duration::from_secs(1)),
            sample_rate: None,
            channels: None,
            video: Some(VideoTrackMetadata {
                coded_width: Some(1280),
                coded_height: Some(720),
                profile: Some(VideoProfile::H264(H264Profile::High)),
                bit_depth: None,
                chroma: None,
                color: None,
                orientation: codec_core::VideoDisplayOrientation::Identity,
            }),
        }
    }

    fn supported_h264_high_avcc_codec_private() -> Vec<u8> {
        vec![
            0x01, 0x64, 0x00, 0x20, 0xff, 0xe1, 0x00, 0x1b, 0x67, 0x64, 0x00, 0x20, 0xac, 0xd1,
            0x00, 0x50, 0x05, 0xbb, 0x01, 0x6a, 0x02, 0x02, 0x02, 0x80, 0x00, 0x01, 0xf4, 0x80,
            0x00, 0xea, 0x60, 0x07, 0x8c, 0x18, 0x89, 0x01, 0x00, 0x04, 0x68, 0xeb, 0x8f, 0x2c,
        ]
    }

    fn avcc_packet_with_annex_b_like_length_prefix() -> Vec<u8> {
        vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x00, 0x00, 0x00, 0x02]
    }
}
