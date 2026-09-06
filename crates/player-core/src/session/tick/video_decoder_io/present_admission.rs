//! Ограничивает decoder I/O свободными местами presentation queue во время live drag.
//! Очередь принадлежит pipeline; этот модуль вычисляет бюджеты без изменения состояния.

use super::{PlayerSession, VideoDecoderIoLimits};

/// Возвращает количество свободных мест в presentation queue.
fn available_video_present_slots(session: &PlayerSession, limits: VideoDecoderIoLimits) -> usize {
    limits
        .present_queue_limit
        .saturating_sub(session.pipeline.video_present_queue_len())
}

/// Возвращает bounded budget приёма decoded frames для текущего admission mode.
pub(super) fn decoded_frame_receive_budget(
    session: &PlayerSession,
    limits: VideoDecoderIoLimits,
) -> usize {
    // Live scrub держит landing до движения мыши: unread кадры обязаны остаться
    // в decoder channel. Перезапись полной очереди теряет ближайшую новую цель.
    if session.has_active_seek_commit() && !session.seek_runtime.active_seek_landing_is_live_scrub()
    {
        return limits.max_frames_to_drain;
    }

    if session.can_present_video() {
        return available_video_present_slots(session, limits).min(limits.max_frames_to_drain);
    }

    limits.max_frames_to_drain
}

/// Возвращает bounded budget отправки packets в decoder для текущего admission mode.
pub(super) fn video_packet_send_present_admission_budget(
    session: &PlayerSession,
    limits: VideoDecoderIoLimits,
) -> usize {
    // Тот же bounded admission сохраняет backpressure до demux, пока landing
    // удерживается; progressive preroll освобождает очередь через scheduler.
    if session.has_active_seek_commit() && !session.seek_runtime.active_seek_landing_is_live_scrub()
    {
        return limits.max_packets_to_send;
    }

    available_video_present_slots(session, limits).min(limits.max_packets_to_send)
}
