//! Demux admission и audio-priority policy внутри session tick boundary.
//!
//! Модуль решает только, можно ли читать следующий packet и как положить
//! прочитанный packet в session-owned очереди. EOF-drain остаётся владельцем
//! lifecycle и не активирует новое demux-чтение через этот boundary.

use std::time::Instant;

use media_core::{DemuxReadEvent, TrackKind, TrackTimestamp};
use tracing::{debug, trace, warn};

use super::{
    PlayerTickConfig, PlayerTickResult,
    presentation_scheduler::{
        available_video_present_slots, seek_admission_active, video_decoder_texture_limits,
    },
    record_pipeline_pause,
    video_decoder_io::{has_texture_capacity_for_decode, texture_capacity_backpressure_reason},
};
use crate::{
    PendingAudioPacket, PendingVideoPacket, PipelineLatencyStage, PipelinePauseReason,
    PlaybackState, PlayerError, PlayerErrorKind, session::PlayerSession,
    session::audio_runtime::sanitize_audio_high_water_mark,
};

/// Нормализует low-water mark для audio catch-up demux.
pub(super) fn sanitize_audio_demux_low_water_mark(low_water_mark_ms: f64) -> f64 {
    if low_water_mark_ms.is_finite() && low_water_mark_ms > 0.0 {
        low_water_mark_ms
    } else {
        PlayerTickConfig::default().audio_demux_low_water_mark_ms
    }
}

/// Возвращает bounded лимит video packets для audio catch-up режима.
pub(super) fn audio_catchup_pending_video_limit(tick_config: &PlayerTickConfig) -> usize {
    tick_config
        .max_pending_video_packets_during_audio_catchup
        .max(tick_config.max_pending_video_packets)
}

/// Проверяет, нужно ли временно приоритизировать demux ради заполнения audio buffer.
pub(super) fn audio_demux_catchup_needed(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    audio_demux_catchup_needed_for_level(
        session.pipeline.has_selected_audio_track(),
        session.audio_buffer_level_ms(),
        tick_config.audio_demux_low_water_mark_ms,
    )
}

/// Чистая часть audio catch-up policy для unit-тестов без CPAL device.
pub(super) fn audio_demux_catchup_needed_for_level(
    audio_track_selected: bool,
    audio_buffer_level_ms: Option<f64>,
    low_water_mark_ms: f64,
) -> bool {
    if !audio_track_selected {
        return false;
    }

    let Some(audio_buffer_level_ms) = audio_buffer_level_ms else {
        return false;
    };

    audio_buffer_level_ms.is_finite()
        && audio_buffer_level_ms < sanitize_audio_demux_low_water_mark(low_water_mark_ms)
}

/// Проверяет bootstrap-фазу, когда audio track уже выбран, но output ещё не создан.
pub(super) fn selected_audio_bootstrap_needs_demux(session: &PlayerSession) -> bool {
    session.pipeline.has_selected_audio_track()
        && session.audio_buffer_level_ms().is_none()
        && session.pipeline.pending_audio_packet_is_empty()
}

/// Проверяет audio read-ahead policy перед чтением следующего demux packet-а.
pub(super) fn audio_read_ahead_blocks_demux(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    if !session.pipeline.has_selected_audio_track() {
        return false;
    }

    match session.audio_buffer_level_ms() {
        Some(audio_buffer_level_ms) => {
            !audio_buffer_level_ms.is_finite()
                || audio_buffer_level_ms
                    > sanitize_audio_high_water_mark(tick_config.audio_buffer_high_water_mark_ms)
        }
        None => !session.pipeline.pending_audio_packet_is_empty(),
    }
}

/// Проверяет bounded video backlog, который разрешён audio bootstrap/catch-up режимам.
pub(super) fn audio_priority_pending_video_limit_allows_demux(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    session.pipeline.pending_video_packet_len() < audio_catchup_pending_video_limit(tick_config)
}

/// Проверяет, исчерпано ли bounded окно adaptive catch-up.
pub(super) fn catch_up_deadline_reached(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

/// Читает новые packets из demuxer в пределах бюджета текущего tick.
pub(super) fn read_demux_packets(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
    packet_budget: usize,
    catch_up_deadline: Option<Instant>,
) -> usize {
    let mut packets_read = 0usize;

    for _ in 0..packet_budget {
        if catch_up_deadline_reached(catch_up_deadline) {
            break;
        }

        let prioritize_audio_catchup = audio_demux_catchup_needed(session, tick_config);
        if prioritize_audio_catchup
            && session.pipeline.pending_video_packet_len() >= tick_config.max_pending_video_packets
        {
            trace!(
                pending_video_packets = session.pipeline.pending_video_packet_len(),
                catchup_video_packet_limit = audio_catchup_pending_video_limit(tick_config),
                audio_buffer_ms = session.audio_buffer_level_ms().unwrap_or(0.0),
                low_water_mark_ms =
                    sanitize_audio_demux_low_water_mark(tick_config.audio_demux_low_water_mark_ms),
                "Demux audio catch-up: reading through video pressure"
            );
        }

        if !can_read_next_demux_packet_with_audio_priority(
            session,
            tick_config,
            prioritize_audio_catchup,
        ) {
            tick_result.demux_backpressured = true;
            let pause_reason =
                demux_backpressure_reason(session, tick_config, prioritize_audio_catchup)
                    .unwrap_or(PipelinePauseReason::DemuxBackpressure);
            record_pipeline_pause(session, tick_result, pause_reason);
            trace!(
                pause_reason = ?pause_reason,
                pending_audio_packets = session.pipeline.pending_audio_packet_len(),
                pending_video_packets = session.pipeline.pending_video_packet_len(),
                queued_video_frames = session.pipeline.video_present_queue_len(),
                audio_buffer_ms = ?session.audio_buffer_level_ms(),
                "Demux backpressure: waiting for downstream capacity"
            );
            break;
        }

        let demux_read_started_at = Instant::now();
        let Some(event_result) = session.pipeline.demux_next_event() else {
            break;
        };
        session.record_pipeline_latency(
            PipelineLatencyStage::DemuxRead,
            demux_read_started_at.elapsed(),
            None,
            None,
        );

        match event_result {
            Ok(DemuxReadEvent::Packet(packet)) => {
                tick_result.record_demuxed_packet(&packet);
                route_demuxed_packet(session, packet);
                packets_read += 1;
            }
            Ok(DemuxReadEvent::EndOfStream) => {
                debug!(
                    current_position_ms =
                        session.snapshot().current_position.as_secs_f64() * 1000.0,
                    duration_ms = ?session
                        .snapshot()
                        .duration
                        .map(|duration| duration.as_secs_f64() * 1000.0),
                    pending_audio_packets = session.pipeline.pending_audio_packet_len(),
                    pending_video_packets = session.pipeline.pending_video_packet_len(),
                    queued_video_frames = session.pipeline.video_present_queue_len(),
                    video_decode_in_flight = session.pipeline.video_decode_in_flight_packets(),
                    audio_buffer_ms = ?session.audio_buffer_level_ms(),
                    audio_clock_now_ms = session.audio_clock_now().as_secs_f64() * 1000.0,
                    "Demux reported EOF; entering drain"
                );
                session.enter_eof_drain();
                break;
            }
            Ok(DemuxReadEvent::TracksChanged(track_update)) => {
                session.handle_demux_track_list_update(track_update);
                if session.playback_state() == PlaybackState::Failed {
                    break;
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, "Ошибка чтения packet");
                session.mark_fatal_error(PlayerError::new(
                    PlayerErrorKind::DemuxError,
                    format!("Ошибка чтения packet: {error}"),
                ));
                break;
            }
        }
    }

    packets_read
}

/// Перекладывает packet из demuxer в соответствующую pending queue.
pub(super) fn route_demuxed_packet(session: &mut PlayerSession, packet: media_core::Packet) {
    let generation = session.pipeline.seek_generation();
    session.note_demux_packet_for_seek_trace(&packet, generation);

    match packet.kind {
        TrackKind::Audio => {
            let packet_timing = audio_packet_timing_from_media_packet(&packet);
            let pending_packet = PendingAudioPacket::with_timing(
                packet.track_id,
                packet.pts,
                packet.dts,
                packet.duration,
                packet_timing,
                generation,
                packet.data,
            );
            session
                .pipeline
                .enqueue_pending_audio_packet(pending_packet);
        }
        TrackKind::Video => {
            let pending_packet = PendingVideoPacket::new(
                packet.track_id,
                packet.pts,
                generation,
                packet.data,
                packet.keyframe,
            );
            session
                .pipeline
                .enqueue_pending_video_packet(pending_packet);
        }
    }
}

/// Собирает audio decoder timing из raw metadata, которую demuxer сохранил рядом с media time.
pub(super) fn audio_packet_timing_from_media_packet(
    packet: &media_core::Packet,
) -> audio_core::AudioPacketTiming {
    let Some(track_pts) = packet.track_pts else {
        return audio_core::AudioPacketTiming::unknown();
    };

    if track_pts.track_id != packet.track_id {
        warn!(
            packet_track_id = %packet.track_id,
            timing_track_id = %track_pts.track_id,
            "Audio packet raw PTS принадлежит другому track; decoder timing помечен unknown"
        );
        return audio_core::AudioPacketTiming::unknown();
    }

    let Some(time_base) = audio_time_base_from_media_time_base(track_pts.time_base) else {
        warn!(
            packet_track_id = %packet.track_id,
            time_base = ?track_pts.time_base,
            "Audio packet raw PTS имеет некорректную time base; decoder timing помечен unknown"
        );
        return audio_core::AudioPacketTiming::unknown();
    };

    let dts_units = audio_packet_dts_units(packet, track_pts);
    let duration_units = audio_packet_duration_units(packet, track_pts);

    audio_core::AudioPacketTiming::from_track_units(
        time_base,
        track_pts.units.get(),
        dts_units,
        duration_units,
    )
}

/// Конвертирует media-core time base в audio boundary time base без Symphonia types.
pub(super) fn audio_time_base_from_media_time_base(
    time_base: media_core::TimeBase,
) -> Option<audio_core::AudioPacketTimeBase> {
    audio_core::AudioPacketTimeBase::new(time_base.numer, time_base.denom)
}

/// Возвращает raw DTS units только если DTS согласован с PTS track owner/timebase.
pub(super) fn audio_packet_dts_units(
    packet: &media_core::Packet,
    track_pts: TrackTimestamp,
) -> Option<i64> {
    let track_dts = packet.track_dts?;

    if track_dts.track_id != packet.track_id || track_dts.time_base != track_pts.time_base {
        warn!(
            packet_track_id = %packet.track_id,
            dts_track_id = %track_dts.track_id,
            pts_time_base = ?track_pts.time_base,
            dts_time_base = ?track_dts.time_base,
            "Audio packet raw DTS не согласован с PTS; DTS не передан decoder boundary"
        );
        return None;
    }

    Some(track_dts.units.get())
}

/// Возвращает raw duration units только если duration согласована с PTS track owner/timebase.
pub(super) fn audio_packet_duration_units(
    packet: &media_core::Packet,
    track_pts: TrackTimestamp,
) -> Option<u64> {
    let track_duration = packet.track_duration?;

    if track_duration.track_id != packet.track_id || track_duration.time_base != track_pts.time_base
    {
        warn!(
            packet_track_id = %packet.track_id,
            duration_track_id = %track_duration.track_id,
            pts_time_base = ?track_pts.time_base,
            duration_time_base = ?track_duration.time_base,
            "Audio packet raw duration не согласована с PTS; duration не передана decoder boundary"
        );
        return None;
    }

    Some(track_duration.units.get())
}

/// Проверяет demux admission с явно переданным audio-priority флагом.
pub(super) fn can_read_next_demux_packet_with_audio_priority(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    prioritize_audio_catchup: bool,
) -> bool {
    if audio_read_ahead_blocks_demux(session, tick_config) {
        return false;
    }

    if prioritize_audio_catchup || selected_audio_bootstrap_needs_demux(session) {
        return audio_priority_pending_video_limit_allows_demux(session, tick_config);
    }

    if !has_texture_capacity_for_decode(session, video_decoder_texture_limits(tick_config)) {
        return false;
    }

    if session.pipeline.pending_video_packet_len() >= tick_config.max_pending_video_packets {
        return false;
    }

    seek_admission_active(session) || available_video_present_slots(session, tick_config) > 0
}

/// Возвращает typed причину demux backpressure вместо generic "нет места".
pub(super) fn demux_backpressure_reason(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    prioritize_audio_catchup: bool,
) -> Option<PipelinePauseReason> {
    if audio_read_ahead_blocks_demux(session, tick_config) {
        return Some(PipelinePauseReason::DemuxBackpressure);
    }

    if prioritize_audio_catchup || selected_audio_bootstrap_needs_demux(session) {
        return (!audio_priority_pending_video_limit_allows_demux(session, tick_config))
            .then_some(PipelinePauseReason::WaitingForDemuxAudioPriority);
    }

    if let Some(reason) =
        texture_capacity_backpressure_reason(session, video_decoder_texture_limits(tick_config))
    {
        return Some(reason);
    }

    if session.pipeline.pending_video_packet_len() >= tick_config.max_pending_video_packets {
        return Some(PipelinePauseReason::DemuxBackpressure);
    }

    (!seek_admission_active(session) && available_video_present_slots(session, tick_config) == 0)
        .then_some(PipelinePauseReason::WaitingForPresentQueue)
}
