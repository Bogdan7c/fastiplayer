//! Presentation scheduler: present/drop/repeat и bounded adaptive catch-up.
//!
//! Модуль не владеет render resources напрямую: все release/accounting решения
//! проходят через `PlayerSession` и `PlaybackPipeline` boundary methods.

use std::time::Instant;

use tracing::{debug, trace};

use super::{
    PlayerTickConfig, PlayerTickContext, PlayerTickResult, PlayerVideoDropReason,
    demux_admission::{
        catch_up_deadline_reached, read_demux_packets, seek_preroll_pending_video_limit,
    },
    record_pipeline_pause, record_video_drop,
    video_decoder_io::{
        VideoDecoderIoContext, drain_decoded_video_frames, run_video_decoder_io,
        send_pending_video_packets_to_decoder,
    },
};
use crate::{PipelineLatencyStage, PipelinePauseReason, PlaybackState, session::PlayerSession};

mod timing;

pub(super) use timing::{
    AdaptiveCatchUpBudget, adaptive_catch_up_budget, adaptive_catch_up_deadline,
    adaptive_catch_up_needed, available_video_present_slots, front_frame_ready_for_scheduler,
    front_frame_scheduler_delay, front_frame_timing, has_texture_capacity_for_catch_up,
    host_upload_ready_queue_capacity, normal_present_queue_blocks_video_admission,
    seek_admission_active, seek_fast_preroll_active, seek_preroll_decoder_io_budget,
    should_drop_front_frame_as_late, should_wait_for_front_frame, target_media_time_for_present,
    texture_slot_min_watermark, video_decode_ahead_limit, video_decode_ahead_target,
    video_decoder_decode_ahead_limits, video_decoder_io_limits, video_decoder_texture_limits,
    video_late_drop_grace, video_present_queue_limit, video_present_queue_target,
    video_present_window,
};
pub(crate) use timing::{SchedulerTimingDiagnosticsSnapshot, scheduler_timing_diagnostics};
#[cfg(test)]
pub(super) use timing::{
    adaptive_catch_up_frame_need, effective_seek_resume_video_min_ready_frames,
};

/// Продвигает active Accurate seek несколькими короткими demux/decode проходами.
///
/// Обычный tick сначала отдаёт всё demux-окно чтению контейнера, а decoder I/O
/// запускает только после этого. На dense audio interleave это создаёт pacing:
/// pending-video лимит освобождается только в следующем worker pass-е. Этот
/// helper сохраняет существующие admission/resource boundaries, но чередует
/// demux, audio-preroll processing и decoder I/O внутри одного bounded seek
/// deadline-а, пока target-or-after frame ещё не готов.
///
/// Возвращает `true`, если helper уже владел seek-specific budget этого tick-а.
/// Это не обещает progress: real decoder/resource backpressure всё ещё может
/// остановить проход, но обычный demux pass не должен получать второй budget.
pub(super) fn run_seek_fast_preroll_catch_up(
    session: &mut PlayerSession,
    tick_context: PlayerTickContext,
    tick_result: &mut PlayerTickResult,
) -> bool {
    let tick_config = &tick_context.config;
    if tick_config.seek_fast_preroll_time_budget.is_zero()
        || !seek_fast_preroll_active(session, tick_config)
    {
        return false;
    }

    let deadline = tick_context
        .now
        .checked_add(tick_config.seek_fast_preroll_time_budget)
        .unwrap_or(tick_context.now);
    let decoder_io_budget = seek_preroll_decoder_io_budget(tick_config);
    let demux_packets_per_pass = tick_config
        .max_demux_packets_per_tick
        .min(seek_preroll_pending_video_limit(tick_config));
    let mut passes = 0usize;
    let mut demux_budget_packets_read = 0usize;
    let mut decoded_frames_drained = 0usize;
    let mut packets_sent = 0usize;
    let started_at = Instant::now();

    while seek_fast_preroll_active(session, tick_config)
        && !catch_up_deadline_reached(Some(deadline))
    {
        passes = passes.saturating_add(1);
        let mut made_progress = false;

        let pre_demux_progress = run_seek_fast_preroll_decoder_io_pass(
            session,
            tick_result,
            tick_config,
            decoder_io_budget,
            deadline,
        );
        decoded_frames_drained =
            decoded_frames_drained.saturating_add(pre_demux_progress.decoded_frames_drained);
        packets_sent = packets_sent.saturating_add(pre_demux_progress.packets_sent);
        made_progress |= pre_demux_progress.decoded_frames_drained > 0;
        made_progress |= pre_demux_progress.packets_sent > 0;

        if !seek_fast_preroll_active(session, tick_config)
            || catch_up_deadline_reached(Some(deadline))
        {
            break;
        }

        if demux_packets_per_pass > 0
            && session.is_demuxing_active()
            && session.pipeline.has_demuxer()
        {
            let packets_read = read_demux_packets(
                session,
                tick_config,
                tick_result,
                demux_packets_per_pass,
                Some(deadline),
            );
            demux_budget_packets_read = demux_budget_packets_read.saturating_add(packets_read);
            made_progress |= packets_read > 0;

            if packets_read > 0 {
                session.process_pending_audio_packets_with_buffer_limit(
                    tick_config.audio_buffer_high_water_mark_ms,
                );
                session.start_eof_audio_tail_if_needed();
            }
        }

        let post_demux_progress = run_seek_fast_preroll_decoder_io_pass(
            session,
            tick_result,
            tick_config,
            decoder_io_budget,
            deadline,
        );
        decoded_frames_drained =
            decoded_frames_drained.saturating_add(post_demux_progress.decoded_frames_drained);
        packets_sent = packets_sent.saturating_add(post_demux_progress.packets_sent);
        made_progress |= post_demux_progress.decoded_frames_drained > 0;
        made_progress |= post_demux_progress.packets_sent > 0;

        if !made_progress {
            break;
        }
    }

    let deadline_reached = catch_up_deadline_reached(Some(deadline));
    let target_ready = !seek_fast_preroll_active(session, tick_config);
    if demux_budget_packets_read > 0
        || decoded_frames_drained > 0
        || packets_sent > 0
        || deadline_reached
        || target_ready
    {
        debug!(
            passes,
            demux_budget_packets_read,
            decoded_frames_drained,
            packets_sent,
            elapsed_ms = started_at.elapsed().as_millis(),
            deadline_reached,
            target_ready,
            "Active accurate seek fast-preroll catch-up pass completed"
        );
    } else {
        trace!(
            passes,
            "Active accurate seek fast-preroll catch-up pass yielded no progress"
        );
    }

    true
}

fn run_seek_fast_preroll_decoder_io_pass(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    tick_config: &PlayerTickConfig,
    decoder_io_budget: usize,
    deadline: Instant,
) -> super::video_decoder_io::VideoDecoderIoProgress {
    let decoder_io_limits = video_decoder_io_limits(
        tick_config,
        decoder_io_budget,
        decoder_io_budget,
        video_decode_ahead_limit(tick_config),
    );

    run_video_decoder_io(
        session,
        tick_result,
        decoder_io_limits,
        VideoDecoderIoContext::new(Some(deadline), true, true),
    )
}

/// Удаляет лишние кадры, если presentation queue стала больше безопасного лимита.
pub(super) fn trim_video_present_queue(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
) {
    let queue_limit = video_present_queue_limit(tick_config);

    while session.pipeline.video_present_queue_len() > queue_limit {
        let Some(frame) = session.pipeline.pop_queued_video_frame_front() else {
            break;
        };

        release_video_texture(session, frame.resource_handle);
        record_video_drop(
            session,
            tick_result,
            frame.pts,
            PlayerVideoDropReason::QueueOverflow,
        );
        tracing::debug!(
            pts_ms = frame.pts.as_millis(),
            "Dropping frame: queue overflow protection"
        );
    }
}

/// Освобождает texture handle через decoder thread, если он ещё существует.
pub(super) fn release_video_texture(
    session: &mut PlayerSession,
    resource_handle: video_core::FrameResourceHandle,
) {
    session.release_video_texture(resource_handle);
}

/// Удаляет первый queued frame и записывает причину drop.
pub(super) fn drop_front_queued_video_frame(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    reason: PlayerVideoDropReason,
) -> bool {
    let Some(frame) = session.pipeline.pop_queued_video_frame_front() else {
        return false;
    };

    let frame_pts = frame.pts;
    release_video_texture(session, frame.resource_handle);
    record_video_drop(session, tick_result, frame_pts, reason);
    tracing::debug!(
        pts_ms = frame_pts.as_millis(),
        ?reason,
        "Dropping queued video frame"
    );
    true
}

/// Сохраняет самый поздний pre-target frame для final seek near EOF.
pub(super) fn replace_seek_preroll_fallback_frame(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    frame: video_core::DecodedFrame,
) {
    if let Some(replaced_frame) = session.replace_seek_preroll_fallback_frame(frame) {
        release_video_texture(session, replaced_frame.resource_handle);
        record_video_drop(
            session,
            tick_result,
            replaced_frame.pts,
            PlayerVideoDropReason::SeekPreroll,
        );
    }
}

/// Удаляет EOF fallback, когда точный target frame уже найден.
pub(super) fn drop_seek_preroll_fallback_frame(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
) {
    if let Some(frame) = session.take_seek_preroll_fallback_frame() {
        release_video_texture(session, frame.resource_handle);
        record_video_drop(
            session,
            tick_result,
            frame.pts,
            PlayerVideoDropReason::SeekPreroll,
        );
    }
}

/// Делает первый queued frame текущим present frame.
pub(super) fn present_front_queued_video_frame(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
) -> bool {
    let Some(frame) = session.pipeline.pop_queued_video_frame_front() else {
        return false;
    };

    tracing::debug!(
        pts_ms = frame.pts.as_millis(),
        "Presenting scheduled video frame"
    );
    let frame_pts = frame.pts;
    if let Some(old_frame) = session.pipeline.replace_present_video_frame(frame) {
        release_video_texture(session, old_frame.resource_handle);
    }
    session.note_presented_frame_for_seek(frame_pts);
    tick_result.record_presented_video_frame();
    true
}

/// Показывает свежий pre-target frame, если final seek дошёл до EOF без target frame-а.
pub(super) fn present_seek_preroll_fallback_after_eof(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
) -> bool {
    if !seek_preroll_fallback_ready_after_eof(session) {
        return false;
    }

    let Some(frame) = session.take_seek_preroll_fallback_frame() else {
        return false;
    };

    let frame_pts = frame.pts;
    tracing::debug!(
        pts_ms = frame_pts.as_millis(),
        "Presenting final seek EOF fallback frame"
    );
    if let Some(old_frame) = session.pipeline.replace_present_video_frame(frame) {
        release_video_texture(session, old_frame.resource_handle);
    }
    session.note_presented_seek_eof_fallback_frame(frame_pts);
    tick_result.record_presented_video_frame();
    true
}

/// Показывает «прокат» live scrub: держит только новейший pre-target кадр и
/// презентует его немедленно, пока exact landing frame ещё не готов.
///
/// Возвращает `true`, если scheduler-pass обработан этой веткой и обычная
/// A/V-логика выполняться не должна. Как только в очереди появился
/// target-or-after кадр текущего generation-а, ветка дропает pre-target кадры
/// перед ним и уступает существующей landing-логике (force present + gates).
pub(super) fn present_live_scrub_preroll_roll(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
) -> bool {
    if !session.active_seek_presents_preroll_progressively() {
        return false;
    }

    let landing_frame_in_queue = session
        .pipeline
        .queued_video_frames()
        .any(|frame| session.active_seek_frame_ready_for_scheduler(frame.pts, frame.generation));
    if landing_frame_in_queue {
        // Landing уже декодирован: прокат больше не нужен, расчищаем pre-target
        // кадры перед ним и передаём present обычному seek-путю этого же tick-а.
        while session
            .pipeline
            .front_queued_video_frame()
            .is_some_and(|frame| {
                !session.active_seek_frame_ready_for_scheduler(frame.pts, frame.generation)
            })
        {
            if !drop_front_queued_video_frame(
                session,
                tick_result,
                PlayerVideoDropReason::SeekPreroll,
            ) {
                break;
            }
        }
        return false;
    }

    // Latest-wins: показываем самый свежий декодированный кадр прохода, более
    // старые сразу release-им, чтобы texture pool продолжал крутить decode.
    while session.pipeline.video_present_queue_len() > 1 {
        if !drop_front_queued_video_frame(session, tick_result, PlayerVideoDropReason::SeekPreroll)
        {
            break;
        }
    }

    if session.pipeline.video_present_queue_is_empty() {
        // Новых кадров прохода ещё нет — держим текущую картинку до следующего tick-а.
        repeat_present_video_frame(session, tick_result, None);
        return true;
    }

    present_front_queued_video_frame(session, tick_result);
    true
}

/// Проверяет, что после EOF больше нет decoder work, способного дать точный target frame.
pub(super) fn seek_preroll_fallback_ready_after_eof(session: &PlayerSession) -> bool {
    if session.active_final_seek_target().is_none() || !session.is_eof_draining() {
        return false;
    }

    if !session.pipeline.has_seek_preroll_fallback_video_frame() {
        return false;
    }

    if !session.pipeline.video_present_queue_is_empty()
        || !session.pipeline.pending_video_packet_is_empty()
        || session.pipeline.video_decode_in_flight_packets() > 0
    {
        return false;
    }

    session
        .pipeline
        .video_decoder_packet_queue_depth()
        .is_none_or(|packet_queue_depth| packet_queue_depth == 0)
}

/// Повторно показывает текущий кадр и учитывает это в telemetry result.
pub(super) fn repeat_present_video_frame(
    session: &mut PlayerSession,
    tick_result: &mut PlayerTickResult,
    pause_reason: Option<PipelinePauseReason>,
) {
    if session.pipeline.has_present_video_frame() {
        tick_result.record_repeated_video_frame();
        session.record_repeated_video_frame();
    }
    if session.pipeline.has_selected_video_track()
        && session.playback_state() == PlaybackState::Playing
        && let Some(pause_reason) = pause_reason
    {
        record_pipeline_pause(session, tick_result, pause_reason);
    }
}

/// Делает один проход adaptive catch-up без смешивания scheduling и rendering.
pub(super) fn run_adaptive_catch_up_pass(
    session: &mut PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_result: &mut PlayerTickResult,
    budget: &mut AdaptiveCatchUpBudget,
    deadline: Instant,
) -> bool {
    let mut made_progress = false;

    if budget.decoded_frames > 0
        && session.pipeline.can_receive_decoded_video_frames()
        && available_video_present_slots(session, tick_config) > 0
    {
        let drain_budget = budget
            .decoded_frames
            .min(tick_config.max_decoded_video_frames_drained_per_tick);
        let drain_limits = video_decoder_io_limits(
            tick_config,
            drain_budget,
            0,
            video_decode_ahead_limit(tick_config),
        );
        let drained_frames =
            drain_decoded_video_frames(session, tick_result, drain_limits, Some(deadline));
        budget.decoded_frames = budget.decoded_frames.saturating_sub(drained_frames);
        made_progress |= drained_frames > 0;
    }

    if budget.video_packets > 0
        && session.is_demuxing_active()
        && has_texture_capacity_for_catch_up(session, tick_config)
    {
        let send_budget = budget
            .video_packets
            .min(tick_config.max_video_packets_sent_per_tick);
        let send_limits = video_decoder_io_limits(
            tick_config,
            0,
            send_budget,
            video_decode_ahead_limit(tick_config),
        );
        let sent_packets = send_pending_video_packets_to_decoder(
            session,
            tick_result,
            send_limits,
            Some(deadline),
        );
        budget.video_packets = budget.video_packets.saturating_sub(sent_packets);
        made_progress |= sent_packets > 0;
    }

    if budget.demux_packets > 0 && session.is_demuxing_active() {
        let demux_budget = budget
            .demux_packets
            .min(tick_config.max_demux_packets_per_tick);
        let demuxed_packets = read_demux_packets(
            session,
            tick_config,
            tick_result,
            demux_budget,
            Some(deadline),
        );
        budget.demux_packets = budget.demux_packets.saturating_sub(demuxed_packets);
        made_progress |= demuxed_packets > 0;

        if demuxed_packets > 0 {
            session.process_pending_audio_packets_with_buffer_limit(
                tick_config.audio_buffer_high_water_mark_ms,
            );
        }
    }

    made_progress
}

/// Догоняет pipeline после короткого latency spike, но только в bounded окне.
pub(super) fn run_adaptive_catch_up(
    session: &mut PlayerSession,
    tick_context: PlayerTickContext,
    tick_result: &mut PlayerTickResult,
) {
    if !adaptive_catch_up_needed(session, &tick_context.config, tick_context.tick_late_by) {
        return;
    }

    let Some(deadline) = adaptive_catch_up_deadline(tick_context.now, &tick_context.config) else {
        return;
    };

    let mut budget =
        adaptive_catch_up_budget(session, &tick_context.config, tick_context.tick_late_by);

    while budget.has_work()
        && !catch_up_deadline_reached(Some(deadline))
        && adaptive_catch_up_needed(session, &tick_context.config, tick_context.tick_late_by)
    {
        let made_progress = run_adaptive_catch_up_pass(
            session,
            &tick_context.config,
            tick_result,
            &mut budget,
            deadline,
        );

        if !made_progress {
            break;
        }
    }
}

/// Обрабатывает pending video packets: приём кадров, backpressure и A/V sync.
pub(super) fn process_pending_video_packets(
    session: &mut PlayerSession,
    tick_context: PlayerTickContext,
    tick_result: &mut PlayerTickResult,
) {
    let tick_config = &tick_context.config;

    session.release_stale_present_frame_for_final_seek_texture_pressure(
        texture_slot_min_watermark(tick_config),
    );

    let seek_fast_preroll = seek_fast_preroll_active(session, tick_config);
    let base_drain_budget = if seek_fast_preroll {
        seek_preroll_decoder_io_budget(tick_config)
    } else if session.can_present_video() {
        tick_config.max_decoded_video_frames_drained_per_tick
    } else {
        usize::MAX
    };
    let packet_send_budget = if seek_fast_preroll {
        seek_preroll_decoder_io_budget(tick_config)
    } else {
        tick_config.max_video_packets_sent_per_tick
    };
    let decode_ahead_budget = if seek_fast_preroll {
        video_decode_ahead_limit(tick_config)
    } else {
        video_decode_ahead_target(tick_config)
    };
    let decoder_io_limits = video_decoder_io_limits(
        tick_config,
        base_drain_budget,
        packet_send_budget,
        decode_ahead_budget,
    );
    let decoder_io_progress = run_video_decoder_io(
        session,
        tick_result,
        decoder_io_limits,
        VideoDecoderIoContext::new(None, true, true),
    );
    trace!(
        decoded_frames_drained = decoder_io_progress.decoded_frames_drained,
        packets_sent = decoder_io_progress.packets_sent,
        "Decoder I/O pass completed"
    );

    let playback_can_present = session.can_present_video();
    if !playback_can_present {
        return;
    }

    run_adaptive_catch_up(session, tick_context, tick_result);
    trim_video_present_queue(session, tick_config, tick_result);
    let scheduler_started_at = Instant::now();

    if present_seek_preroll_fallback_after_eof(session, tick_result) {
        session.record_pipeline_latency(
            PipelineLatencyStage::WorkerScheduler,
            scheduler_started_at.elapsed(),
            None,
            None,
        );
        return;
    }

    // Прокат live scrub идёт до audio-stall/target-window логики: во время drag
    // audio запаузено, и обычный путь показал бы старейший кадр вместо новейшего.
    if present_live_scrub_preroll_roll(session, tick_result) {
        session.record_pipeline_latency(
            PipelineLatencyStage::WorkerScheduler,
            scheduler_started_at.elapsed(),
            None,
            None,
        );
        return;
    }

    if !session.pipeline.video_present_queue_is_empty() {
        tracing::debug!(
            queue_len = session.pipeline.video_present_queue_len(),
            "A/V sync: processing frame queue"
        );
    }

    let audio_now = session.audio_clock_now();
    session
        .pipeline
        .note_audio_clock_sample(audio_now, tick_context.now);

    let audio_stall_elapsed = session.pipeline.audio_clock_stalled_for(tick_context.now);
    let audio_stalled = audio_now >= tick_config.audio_stall_min_position
        && audio_stall_elapsed >= tick_config.audio_stall_timeout;

    if audio_stalled {
        tracing::debug!(
            audio_ms = audio_now.as_secs_f64() * 1000.0,
            stalled_ms = audio_stall_elapsed.as_millis(),
            queue_len = session.pipeline.video_present_queue_len(),
            "A/V sync: audio stalled"
        );

        if !present_front_queued_video_frame(session, tick_result) {
            repeat_present_video_frame(
                session,
                tick_result,
                Some(PipelinePauseReason::DecoderStarvation),
            );
        }
        session.record_pipeline_latency(
            PipelineLatencyStage::WorkerScheduler,
            scheduler_started_at.elapsed(),
            None,
            None,
        );
        return;
    }

    let presentation_clock_position = session.presentation_clock_position_at(tick_context.now);
    let target_media_time = target_media_time_for_present(session, tick_config, tick_context.now);
    let present_window = video_present_window(session, tick_config);
    let late_drop_grace = video_late_drop_grace(session, tick_config);

    while should_drop_front_frame_as_late(
        session.pipeline.front_and_next_queued_video_frames(),
        target_media_time,
        late_drop_grace,
    ) {
        if session
            .pipeline
            .front_queued_video_frame()
            .is_some_and(|frame| {
                session.active_seek_frame_ready_for_scheduler(frame.pts, frame.generation)
            })
        {
            break;
        }

        if !drop_front_queued_video_frame(session, tick_result, PlayerVideoDropReason::Late) {
            break;
        }
    }

    let Some(frame) = session.pipeline.front_queued_video_frame() else {
        repeat_present_video_frame(
            session,
            tick_result,
            Some(PipelinePauseReason::DecoderStarvation),
        );
        session.record_pipeline_latency(
            PipelineLatencyStage::WorkerScheduler,
            scheduler_started_at.elapsed(),
            None,
            None,
        );
        return;
    };

    let diff_ms = frame.pts.as_secs_f64() * 1000.0 - target_media_time.as_secs_f64() * 1000.0;
    let force_present_for_seek =
        session.active_seek_frame_ready_for_scheduler(frame.pts, frame.generation);
    if !force_present_for_seek
        && should_wait_for_front_frame(frame.pts, target_media_time, present_window)
    {
        trace!(
            pts_ms = frame.pts.as_millis(),
            target_ms = target_media_time.as_millis(),
            diff_ms,
            window_ms = present_window.as_millis(),
            "A/V scheduler: waiting for target media time"
        );
        record_pipeline_pause(session, tick_result, PipelinePauseReason::SyncWaiting);
        repeat_present_video_frame(session, tick_result, None);
        session.record_pipeline_latency(
            PipelineLatencyStage::WorkerScheduler,
            scheduler_started_at.elapsed(),
            None,
            None,
        );
        return;
    }

    tracing::debug!(
        pts_ms = frame.pts.as_millis(),
        audio_ms = audio_now.as_millis(),
        clock_ms = presentation_clock_position.as_millis(),
        target_ms = target_media_time.as_millis(),
        diff_ms,
        window_ms = present_window.as_millis(),
        force_present_for_seek,
        "A/V scheduler: frame selected"
    );
    present_front_queued_video_frame(session, tick_result);
    session.record_pipeline_latency(
        PipelineLatencyStage::WorkerScheduler,
        scheduler_started_at.elapsed(),
        None,
        None,
    );

    run_adaptive_catch_up(session, tick_context, tick_result);
}
