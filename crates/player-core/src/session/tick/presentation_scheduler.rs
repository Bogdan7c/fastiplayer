//! Presentation scheduler: present/drop/repeat и bounded adaptive catch-up.
//!
//! Модуль не владеет render resources напрямую: все release/accounting решения
//! проходят через `PlayerSession` и `PlaybackPipeline` boundary methods.

use std::time::{Duration, Instant};

use tracing::{debug, trace};

use super::{
    PlayerTickConfig, PlayerTickContext, PlayerTickResult, PlayerVideoDropReason,
    demux_admission::{
        catch_up_deadline_reached, read_demux_packets, seek_preroll_pending_video_limit,
    },
    record_pipeline_pause, record_video_drop,
    video_decoder_io::{
        VideoDecoderDecodeAheadLimits, VideoDecoderIoContext, VideoDecoderIoLimits,
        VideoDecoderTextureLimits, drain_decoded_video_frames, run_video_decoder_io,
        send_pending_video_packets_to_decoder,
    },
};
use crate::{
    PipelineLatencyStage, PipelinePauseReason, PlaybackState, WorkerFrameTimingSnapshot,
    session::PlayerSession,
};

/// Read-only snapshot scheduler clocks для active seek stall diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SchedulerTimingDiagnosticsSnapshot {
    /// Текущее значение audio clock без media base.
    pub(crate) audio_clock: Duration,

    /// Абсолютная media position, которую scheduler считает "сейчас".
    pub(crate) presentation_clock_position: Duration,

    /// Media target с учётом scheduler lead перед ближайшим present.
    pub(crate) target_media_time_for_present: Duration,
}

/// Собирает scheduler timing без изменения очередей и clock state.
pub(crate) fn scheduler_timing_diagnostics(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    now: Instant,
) -> SchedulerTimingDiagnosticsSnapshot {
    let presentation_clock_position = session.presentation_clock_position_at(now);
    let target_media_time_for_present =
        target_media_time_for_present(session, tick_config, presentation_clock_position);

    SchedulerTimingDiagnosticsSnapshot {
        audio_clock: session.audio_clock_now(),
        presentation_clock_position,
        target_media_time_for_present,
    }
}

/// Возвращает количество свободных мест в presentation queue.
pub(super) fn available_video_present_slots(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> usize {
    video_present_queue_limit(tick_config)
        .saturating_sub(session.pipeline.video_present_queue_len())
}

/// Возвращает безопасный лимит presentation queue.
pub(super) fn video_present_queue_limit(tick_config: &PlayerTickConfig) -> usize {
    tick_config.max_video_present_queue.max(1)
}

/// Возвращает безопасный минимум presentation queue.
pub(super) fn video_present_queue_min(tick_config: &PlayerTickConfig) -> usize {
    tick_config
        .min_video_present_queue
        .max(1)
        .min(video_present_queue_limit(tick_config))
}

/// Возвращает codec-neutral target presentation queue для steady-state playback.
pub(super) fn video_present_queue_target(tick_config: &PlayerTickConfig) -> usize {
    tick_config
        .target_video_present_queue
        .max(video_present_queue_min(tick_config))
        .min(video_present_queue_limit(tick_config))
}

/// Возвращает bounded capacity software decoded-frame ready queue.
pub(super) fn host_upload_ready_queue_capacity(tick_config: &PlayerTickConfig) -> usize {
    tick_config.decoder_ready_queue_frames.max(1)
}

/// Проверяет, что seek transaction сейчас должен продвигаться независимо от обычных present slots.
pub(super) fn seek_admission_active(session: &PlayerSession) -> bool {
    session.has_active_seek_commit()
}

/// Проверяет, блокирует ли normal playback admission работу video pipeline-а.
pub(super) fn normal_present_queue_blocks_video_admission(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    !seek_admission_active(session)
        && session.can_present_video()
        && available_video_present_slots(session, tick_config) == 0
}

/// Возвращает безопасный максимум decode-ahead относительно audio clock.
pub(super) fn video_decode_ahead_limit(tick_config: &PlayerTickConfig) -> Duration {
    tick_config
        .max_video_decode_ahead
        .max(Duration::from_millis(1))
}

/// Возвращает steady-state target decode-ahead относительно audio clock.
pub(super) fn video_decode_ahead_target(tick_config: &PlayerTickConfig) -> Duration {
    tick_config
        .target_video_decode_ahead
        .max(Duration::from_millis(1))
        .min(video_decode_ahead_limit(tick_config))
}

/// Возвращает безопасный минимальный reserve surface/import slots.
pub(super) fn texture_slot_min_watermark(tick_config: &PlayerTickConfig) -> usize {
    tick_config.min_texture_slots_available_for_decode
}

/// Возвращает безопасный target reserve surface/import slots.
pub(super) fn texture_slot_target_watermark(tick_config: &PlayerTickConfig) -> usize {
    tick_config
        .target_texture_slots_available_for_decode
        .max(texture_slot_min_watermark(tick_config))
}

/// Формирует typed texture limits для decoder I/O без передачи всего config-а.
pub(super) fn video_decoder_texture_limits(
    tick_config: &PlayerTickConfig,
) -> VideoDecoderTextureLimits {
    VideoDecoderTextureLimits::new(texture_slot_min_watermark(tick_config))
}

/// Формирует typed decode-ahead limits для конкретного режима admission.
pub(super) fn video_decoder_decode_ahead_limits(
    tick_config: &PlayerTickConfig,
    admission_limit: Duration,
) -> VideoDecoderDecodeAheadLimits {
    VideoDecoderDecodeAheadLimits::new(admission_limit, video_decode_ahead_limit(tick_config))
}

/// Формирует полный набор лимитов для одного decoder I/O прохода.
pub(super) fn video_decoder_io_limits(
    tick_config: &PlayerTickConfig,
    max_frames_to_drain: usize,
    max_packets_to_send: usize,
    decode_ahead_limit: Duration,
) -> VideoDecoderIoLimits {
    VideoDecoderIoLimits::new(
        max_frames_to_drain,
        max_packets_to_send,
        video_present_queue_limit(tick_config),
        host_upload_ready_queue_capacity(tick_config),
        video_decoder_texture_limits(tick_config),
        video_decoder_decode_ahead_limits(tick_config, decode_ahead_limit),
    )
}

/// Проверяет, активен ли fast-preroll режим accurate seek-а для video pipeline.
fn seek_fast_preroll_active(session: &PlayerSession, tick_config: &PlayerTickConfig) -> bool {
    session.active_accurate_seek_needs_fast_video_preroll(
        tick_config.effective_seek_resume_video_min_ready_frames(),
    )
}

/// Возвращает bounded decoder I/O budget для seek preroll burst-а.
fn seek_preroll_decoder_io_budget(tick_config: &PlayerTickConfig) -> usize {
    seek_preroll_pending_video_limit(tick_config)
        .max(tick_config.max_video_packets_sent_per_tick)
        .max(tick_config.max_decoded_video_frames_drained_per_tick)
}

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

    debug!(
        passes,
        demux_budget_packets_read,
        decoded_frames_drained,
        packets_sent,
        elapsed_ms = started_at.elapsed().as_millis(),
        deadline_reached = catch_up_deadline_reached(Some(deadline)),
        target_ready = !seek_fast_preroll_active(session, tick_config),
        "Active accurate seek fast-preroll catch-up pass completed"
    );

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

/// Возвращает достижимый video preroll для seek resume с учётом размера presentation queue.
#[cfg(test)]
pub(super) fn effective_seek_resume_video_min_ready_frames(
    tick_config: &PlayerTickConfig,
) -> usize {
    tick_config.effective_seek_resume_video_min_ready_frames()
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

/// Добавляет duration без panic при переполнении.
pub(super) fn saturating_duration_add(timestamp: Duration, offset: Duration) -> Duration {
    timestamp.checked_add(offset).unwrap_or(Duration::MAX)
}

/// Возвращает безопасный неотрицательный множитель для `Duration::mul_f64`.
pub(super) fn finite_non_negative_factor(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value >= 0.0 {
        value
    } else {
        fallback
    }
}

/// Возвращает signed delta между двумя media timestamps в микросекундах.
pub(super) fn duration_delta_micros(left: Duration, right: Duration) -> i128 {
    let delta = if left >= right {
        left.saturating_sub(right)
            .as_micros()
            .min(i128::MAX as u128) as i128
    } else {
        right
            .saturating_sub(left)
            .as_micros()
            .min(i128::MAX as u128) as i128
    };

    if left >= right { delta } else { -delta }
}

/// Рассчитывает media time, под который выбираем frame для ближайшего present.
pub(super) fn target_media_time_for_present(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    presentation_now: Duration,
) -> Duration {
    let present_lead = video_present_lead(session, tick_config);

    saturating_duration_add(presentation_now, present_lead)
}

/// Возвращает lead scheduler-а перед PTS кадра.
///
/// Это не video cadence и не fixed display tick: lead считается как доля
/// наблюдаемой длительности кадра, чтобы worker успел передать frame render
/// thread-у до ближайшего vsync.
pub(super) fn video_present_lead(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> Duration {
    let lead_frames = finite_non_negative_factor(
        tick_config.video_present_lead_frames,
        PlayerTickConfig::default().video_present_lead_frames,
    );
    session
        .pipeline
        .video_frame_duration_estimate()
        .mul_f64(lead_frames)
}

/// Собирает diagnostics по первому queued frame без мутации очереди.
pub(super) fn front_frame_timing(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    now: Instant,
) -> Option<WorkerFrameTimingSnapshot> {
    let front_frame = session.pipeline.front_queued_video_frame()?;
    let presentation_now = session.presentation_clock_position_at(now);
    let target_media_time = target_media_time_for_present(session, tick_config, presentation_now);

    Some(WorkerFrameTimingSnapshot {
        front_frame_pts: front_frame.pts,
        target_media_time,
        front_frame_delta_from_target_us: duration_delta_micros(front_frame.pts, target_media_time),
    })
}

/// Проверяет, должен ли scheduler немедленно обработать первый queued frame.
pub(super) fn front_frame_ready_for_scheduler(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    now: Instant,
) -> bool {
    let Some(front_frame) = session.pipeline.front_queued_video_frame() else {
        return false;
    };

    if session.active_seek_frame_ready_for_scheduler(front_frame.pts, front_frame.generation) {
        return true;
    }

    let presentation_now = session.presentation_clock_position_at(now);
    let target_media_time = target_media_time_for_present(session, tick_config, presentation_now);
    let present_window = video_present_window(session, tick_config);
    let late_drop_grace = video_late_drop_grace(session, tick_config);

    should_drop_front_frame_as_late(
        session.pipeline.front_and_next_queued_video_frames(),
        target_media_time,
        late_drop_grace,
    ) || !should_wait_for_front_frame(front_frame.pts, target_media_time, present_window)
}

/// Возвращает задержку до момента, когда scheduler должен подготовить первый queued frame.
///
/// Worker просыпается не ровно на PTS, а на `PTS - present_lead`: иначе
/// `Condvar`/`select!` timeout, планировщик ОС и handoff в render thread
/// систематически публикуют frame уже после нужного redraw-а.
pub(super) fn front_frame_scheduler_delay(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    now: Instant,
) -> Option<Duration> {
    let front_frame = session.pipeline.front_queued_video_frame()?;
    let presentation_now = session.presentation_clock_position_at(now);
    let scheduler_media_deadline = front_frame
        .pts
        .saturating_sub(video_present_lead(session, tick_config));

    Some(scheduler_media_deadline.saturating_sub(presentation_now))
}

/// Возвращает допустимое окно выбора кадра вокруг target media time.
pub(super) fn video_present_window(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> Duration {
    let window_frames = finite_non_negative_factor(
        tick_config.video_present_window_frames,
        PlayerTickConfig::default().video_present_window_frames,
    );

    session
        .pipeline
        .video_frame_duration_estimate()
        .mul_f64(window_frames)
}

/// Возвращает допустимое опоздание кадра перед forced catch-up drop.
pub(super) fn video_late_drop_grace(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> Duration {
    let grace_frames = finite_non_negative_factor(
        tick_config.video_late_drop_grace_frames,
        PlayerTickConfig::default().video_late_drop_grace_frames,
    );

    session
        .pipeline
        .video_frame_duration_estimate()
        .mul_f64(grace_frames)
}

/// Проверяет, нужно ли дропнуть первый queued frame как реально устаревший.
pub(super) fn should_drop_front_frame_as_late(
    front_and_next_frames: Option<(&video_core::DecodedFrame, &video_core::DecodedFrame)>,
    target_media_time: Duration,
    late_drop_grace: Duration,
) -> bool {
    let Some((front_frame, next_frame)) = front_and_next_frames else {
        // Без кадра-замены причина опоздания: starvation, а не настоящий late drop.
        return false;
    };

    let latest_front_pts = saturating_duration_add(front_frame.pts, late_drop_grace);
    if target_media_time <= latest_front_pts {
        return false;
    }

    next_frame.pts <= target_media_time
}

/// Проверяет, слишком ли рано показывать первый queued frame.
pub(super) fn should_wait_for_front_frame(
    frame_pts: Duration,
    target_media_time: Duration,
    _present_window: Duration,
) -> bool {
    frame_pts > target_media_time
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

/// Остаток bounded adaptive work внутри одного tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct AdaptiveCatchUpBudget {
    /// Сколько дополнительных demux packets ещё можно прочитать.
    pub(super) demux_packets: usize,

    /// Сколько дополнительных video packets ещё можно отправить decoder thread-у.
    pub(super) video_packets: usize,

    /// Сколько дополнительных decoded frames ещё можно принять из decoder thread-а.
    pub(super) decoded_frames: usize,
}

impl AdaptiveCatchUpBudget {
    /// Проверяет, остался ли хоть один вид catch-up work.
    #[must_use]
    const fn has_work(self) -> bool {
        self.demux_packets > 0 || self.video_packets > 0 || self.decoded_frames > 0
    }
}

/// Возвращает deadline дополнительного catch-up окна.
pub(super) fn adaptive_catch_up_deadline(
    now: Instant,
    tick_config: &PlayerTickConfig,
) -> Option<Instant> {
    if tick_config.adaptive_catch_up_time_budget.is_zero() {
        return None;
    }

    now.checked_add(tick_config.adaptive_catch_up_time_budget)
}

/// Считает, сколько frame intervals worker потерял из-за задержки tick-а.
pub(super) fn delayed_frame_count(session: &PlayerSession, tick_late_by: Duration) -> usize {
    if tick_late_by.is_zero() {
        return 0;
    }

    let frame_nanos = session
        .pipeline
        .video_frame_duration_estimate()
        .as_nanos()
        .max(1);
    let late_nanos = tick_late_by.as_nanos();
    let delayed_frames = late_nanos.saturating_add(frame_nanos.saturating_sub(1)) / frame_nanos;

    delayed_frames.min(usize::MAX as u128) as usize
}

/// Считает frame deficit, который adaptive catch-up должен попытаться закрыть.
pub(super) fn adaptive_catch_up_frame_need(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_late_by: Duration,
) -> usize {
    let queue_depth = session.pipeline.video_present_queue_len();
    let target_queue_depth = video_present_queue_target(tick_config);
    let target_deficit = target_queue_depth.saturating_sub(queue_depth);
    let min_deficit = video_present_queue_min(tick_config).saturating_sub(queue_depth);
    let delayed_frames = delayed_frame_count(session, tick_late_by);
    let delayed_target_deficit = target_queue_depth
        .saturating_add(delayed_frames)
        .min(video_present_queue_limit(tick_config))
        .saturating_sub(queue_depth);

    target_deficit.max(min_deficit).max(delayed_target_deficit)
}

/// Проверяет, нужен ли adaptive catch-up и есть ли куда складывать decoded frames.
pub(super) fn adaptive_catch_up_needed(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_late_by: Duration,
) -> bool {
    if !session.can_present_video() {
        return false;
    }

    if available_video_present_slots(session, tick_config) == 0 {
        return false;
    }

    adaptive_catch_up_frame_need(session, tick_config, tick_late_by) > 0
}

/// Формирует operation budgets для catch-up из user-configured базовых budgets.
pub(super) fn adaptive_catch_up_budget(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    tick_late_by: Duration,
) -> AdaptiveCatchUpBudget {
    let frame_need = adaptive_catch_up_frame_need(session, tick_config, tick_late_by)
        .min(available_video_present_slots(session, tick_config));

    AdaptiveCatchUpBudget {
        demux_packets: tick_config
            .max_demux_packets_per_tick
            .saturating_add(frame_need),
        video_packets: tick_config
            .max_video_packets_sent_per_tick
            .saturating_add(frame_need)
            .min(available_video_present_slots(session, tick_config)),
        decoded_frames: tick_config
            .max_decoded_video_frames_drained_per_tick
            .saturating_add(frame_need)
            .min(available_video_present_slots(session, tick_config)),
    }
}

/// Проверяет, есть ли запас surface/import slots для дополнительного decode work.
pub(super) fn has_texture_capacity_for_catch_up(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    if session
        .pipeline
        .video_decoder_send_backpressure(host_upload_ready_queue_capacity(tick_config))
        .is_some()
    {
        return false;
    }

    let Some(stats) = session.pipeline.video_decoder_resource_snapshot() else {
        return true;
    };

    stats.available_slots() > texture_slot_target_watermark(tick_config)
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

    let presentation_now = session.presentation_clock_position_at(tick_context.now);
    let target_media_time = target_media_time_for_present(session, tick_config, presentation_now);
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
        clock_ms = presentation_now.as_millis(),
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
