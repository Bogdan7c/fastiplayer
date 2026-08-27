//! Чистые расчёты времени, admission limits и diagnostics presentation scheduler-а.
//!
//! Модуль читает session/config snapshots, но не меняет presentation queue,
//! render resources, decoder accounting или playback clocks.

use std::time::{Duration, Instant};

use super::super::{
    PlayerTickConfig,
    demux_admission::seek_preroll_pending_video_limit,
    video_decoder_io::{
        VideoDecoderDecodeAheadLimits, VideoDecoderIoLimits, VideoDecoderTextureLimits,
    },
};
use crate::{WorkerFrameTimingSnapshot, session::PlayerSession};

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
    let target_media_time_for_present = target_media_time_for_present(session, tick_config, now);

    SchedulerTimingDiagnosticsSnapshot {
        audio_clock: session.audio_clock_now(),
        presentation_clock_position,
        target_media_time_for_present,
    }
}

/// Возвращает количество свободных мест в presentation queue.
pub(in crate::session::tick) fn available_video_present_slots(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> usize {
    video_present_queue_limit(tick_config)
        .saturating_sub(session.pipeline.video_present_queue_len())
}

/// Возвращает безопасный лимит presentation queue.
pub(in crate::session::tick) fn video_present_queue_limit(tick_config: &PlayerTickConfig) -> usize {
    tick_config.max_video_present_queue.max(1)
}

/// Возвращает безопасный минимум presentation queue.
pub(in crate::session::tick) fn video_present_queue_min(tick_config: &PlayerTickConfig) -> usize {
    tick_config
        .min_video_present_queue
        .max(1)
        .min(video_present_queue_limit(tick_config))
}

/// Возвращает codec-neutral target presentation queue для steady-state playback.
pub(in crate::session::tick) fn video_present_queue_target(
    tick_config: &PlayerTickConfig,
) -> usize {
    tick_config
        .target_video_present_queue
        .max(video_present_queue_min(tick_config))
        .min(video_present_queue_limit(tick_config))
}

/// Возвращает bounded capacity software decoded-frame ready queue.
pub(in crate::session::tick) fn host_upload_ready_queue_capacity(
    tick_config: &PlayerTickConfig,
) -> usize {
    tick_config.decoder_ready_queue_frames.max(1)
}

/// Проверяет, что seek transaction сейчас должен продвигаться независимо от обычных present slots.
pub(in crate::session::tick) fn seek_admission_active(session: &PlayerSession) -> bool {
    session.has_active_seek_commit()
}

/// Проверяет, блокирует ли normal playback admission работу video pipeline-а.
pub(in crate::session::tick) fn normal_present_queue_blocks_video_admission(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    !seek_admission_active(session)
        && session.can_present_video()
        && available_video_present_slots(session, tick_config) == 0
}

/// Возвращает безопасный максимум decode-ahead относительно audio clock.
pub(in crate::session::tick) fn video_decode_ahead_limit(
    tick_config: &PlayerTickConfig,
) -> Duration {
    tick_config
        .max_video_decode_ahead
        .max(Duration::from_millis(1))
}

/// Возвращает steady-state target decode-ahead относительно audio clock.
pub(in crate::session::tick) fn video_decode_ahead_target(
    tick_config: &PlayerTickConfig,
) -> Duration {
    tick_config
        .target_video_decode_ahead
        .max(Duration::from_millis(1))
        .min(video_decode_ahead_limit(tick_config))
}

/// Возвращает безопасный минимальный reserve surface/import slots.
pub(in crate::session::tick) fn texture_slot_min_watermark(
    tick_config: &PlayerTickConfig,
) -> usize {
    tick_config.min_texture_slots_available_for_decode
}

/// Возвращает безопасный target reserve surface/import slots.
pub(in crate::session::tick) fn texture_slot_target_watermark(
    tick_config: &PlayerTickConfig,
) -> usize {
    tick_config
        .target_texture_slots_available_for_decode
        .max(texture_slot_min_watermark(tick_config))
}

/// Формирует typed texture limits для decoder I/O без передачи всего config-а.
pub(in crate::session::tick) fn video_decoder_texture_limits(
    tick_config: &PlayerTickConfig,
) -> VideoDecoderTextureLimits {
    VideoDecoderTextureLimits::new(texture_slot_min_watermark(tick_config))
}

/// Формирует typed decode-ahead limits для конкретного режима admission.
pub(in crate::session::tick) fn video_decoder_decode_ahead_limits(
    tick_config: &PlayerTickConfig,
    admission_limit: Duration,
) -> VideoDecoderDecodeAheadLimits {
    VideoDecoderDecodeAheadLimits::new(admission_limit, video_decode_ahead_limit(tick_config))
}

/// Формирует полный набор лимитов для одного decoder I/O прохода.
pub(in crate::session::tick) fn video_decoder_io_limits(
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
pub(in crate::session::tick) fn seek_fast_preroll_active(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
) -> bool {
    session.active_accurate_seek_needs_fast_video_preroll(
        tick_config.effective_seek_resume_video_min_ready_frames(),
    )
}

/// Возвращает bounded decoder I/O budget для seek preroll burst-а.
pub(in crate::session::tick) fn seek_preroll_decoder_io_budget(
    tick_config: &PlayerTickConfig,
) -> usize {
    seek_preroll_pending_video_limit(tick_config)
        .max(tick_config.max_video_packets_sent_per_tick)
        .max(tick_config.max_decoded_video_frames_drained_per_tick)
}

/// Возвращает достижимый video preroll для seek resume с учётом размера presentation queue.
#[cfg(test)]
pub(in crate::session::tick) fn effective_seek_resume_video_min_ready_frames(
    tick_config: &PlayerTickConfig,
) -> usize {
    tick_config.effective_seek_resume_video_min_ready_frames()
}

/// Добавляет duration без panic при переполнении.
pub(in crate::session::tick) fn saturating_duration_add(
    timestamp: Duration,
    offset: Duration,
) -> Duration {
    timestamp.checked_add(offset).unwrap_or(Duration::MAX)
}

/// Возвращает безопасный неотрицательный множитель для `Duration::mul_f64`.
pub(in crate::session::tick) fn finite_non_negative_factor(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value >= 0.0 {
        value
    } else {
        fallback
    }
}

/// Возвращает signed delta между двумя media timestamps в микросекундах.
pub(in crate::session::tick) fn duration_delta_micros(left: Duration, right: Duration) -> i128 {
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
pub(in crate::session::tick) fn target_media_time_for_present(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    now: Instant,
) -> Duration {
    session
        .presentation_media_position_after_wall_delay(now, video_present_lead(session, tick_config))
}

/// Возвращает lead scheduler-а перед PTS кадра.
///
/// Это не video cadence и не fixed display tick: lead считается как доля
/// наблюдаемой длительности кадра, чтобы worker успел передать frame render
/// thread-у до ближайшего vsync.
pub(in crate::session::tick) fn video_present_lead(
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
pub(in crate::session::tick) fn front_frame_timing(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    now: Instant,
) -> Option<WorkerFrameTimingSnapshot> {
    let front_frame = session.pipeline.front_queued_video_frame()?;
    let target_media_time = target_media_time_for_present(session, tick_config, now);

    Some(WorkerFrameTimingSnapshot {
        front_frame_pts: front_frame.pts,
        target_media_time,
        front_frame_delta_from_target_us: duration_delta_micros(front_frame.pts, target_media_time),
    })
}

/// Проверяет, должен ли scheduler немедленно обработать первый queued frame.
pub(in crate::session::tick) fn front_frame_ready_for_scheduler(
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

    let target_media_time = target_media_time_for_present(session, tick_config, now);
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
pub(in crate::session::tick) fn front_frame_scheduler_delay(
    session: &PlayerSession,
    tick_config: &PlayerTickConfig,
    now: Instant,
) -> Option<Duration> {
    let front_frame = session.pipeline.front_queued_video_frame()?;
    let wall_delay_until_pts = session.wall_delay_until_media_deadline(now, front_frame.pts);

    Some(wall_delay_until_pts.saturating_sub(video_present_lead(session, tick_config)))
}

/// Возвращает допустимое окно выбора кадра вокруг target media time.
pub(in crate::session::tick) fn video_present_window(
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
pub(in crate::session::tick) fn video_late_drop_grace(
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
pub(in crate::session::tick) fn should_drop_front_frame_as_late(
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
pub(in crate::session::tick) fn should_wait_for_front_frame(
    frame_pts: Duration,
    target_media_time: Duration,
    _present_window: Duration,
) -> bool {
    frame_pts > target_media_time
}

/// Остаток bounded adaptive work внутри одного tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::session::tick) struct AdaptiveCatchUpBudget {
    /// Сколько дополнительных demux packets ещё можно прочитать.
    pub(in crate::session::tick) demux_packets: usize,

    /// Сколько дополнительных video packets ещё можно отправить decoder thread-у.
    pub(in crate::session::tick) video_packets: usize,

    /// Сколько дополнительных decoded frames ещё можно принять из decoder thread-а.
    pub(in crate::session::tick) decoded_frames: usize,
}

impl AdaptiveCatchUpBudget {
    /// Проверяет, остался ли хоть один вид catch-up work.
    #[must_use]
    pub(super) const fn has_work(self) -> bool {
        self.demux_packets > 0 || self.video_packets > 0 || self.decoded_frames > 0
    }
}

/// Возвращает deadline дополнительного catch-up окна.
pub(in crate::session::tick) fn adaptive_catch_up_deadline(
    now: Instant,
    tick_config: &PlayerTickConfig,
) -> Option<Instant> {
    if tick_config.adaptive_catch_up_time_budget.is_zero() {
        return None;
    }

    now.checked_add(tick_config.adaptive_catch_up_time_budget)
}

/// Считает, сколько frame intervals worker потерял из-за задержки tick-а.
pub(in crate::session::tick) fn delayed_frame_count(
    session: &PlayerSession,
    tick_late_by: Duration,
) -> usize {
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
pub(in crate::session::tick) fn adaptive_catch_up_frame_need(
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
pub(in crate::session::tick) fn adaptive_catch_up_needed(
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
pub(in crate::session::tick) fn adaptive_catch_up_budget(
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
pub(in crate::session::tick) fn has_texture_capacity_for_catch_up(
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
