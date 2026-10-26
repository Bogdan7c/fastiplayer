//! Политика time-based decode-ahead для отправки video packets в decoder.
//!
//! Что ограничивает окно. Video packet не уходит в decoder, если *выход decoder-а*
//! (готовые кадры) опередит audio clock больше, чем на лимит (`min(admission, max)`): так
//! decoder не раздувает очередь кадров далеко вперёд звука. Опережение меряется от
//! прогноза следующего кадра на выходе decoder-а (`projected_next_video_decoder_output_pts`),
//! а не от PTS самого packet-а.
//!
//! Почему не от PTS packet-а. Frame-threaded software decoder (FFmpeg, `thread_count` ≈
//! cores-2, плюс задержка reorder `has_b_frames`) выдаёт кадр k только после ~N следующих
//! packets. Packets внутри его конвейера кадров ещё не дали, но по старому правилу уже
//! «съедали» окно: при `N × длительность кадра` больше окна (30 fps × 16 ≈ 533 мс против
//! 250/500 мс) decoder голодал, кадры приходили поздно и терялись (замер 780M: 463/600).
//! Прогноз выхода от числа потоков и fps не зависит, поэтому окно снова ограничивает
//! ровно то, что должно, — готовые кадры впереди звука. Для decoder-а без задержки
//! выхода (VA-API) прогноз совпадает с PTS packet-а и поведение прежнее.
//!
//! Граница сверху: опережение берётся как `min(прогноз выхода, PTS packet-а)`, то есть
//! новое правило никогда не строже старого (скачок PTS назад, decoder, глотающий кадры,
//! не могут заморить отправку сильнее, чем раньше). Пока в эпохе decoder-а кадров ещё
//! не было (после seek, замены decoder-а), прогноза нет — действует старая мера по PTS
//! packet-а. Память держат и остальные count-лимиты: ёмкость decoder channel, pending
//! packets, presentation queue, texture/host-upload slots, backpressure.
//!
//! Старт. Пока стартовый preroll голоден по видео (первого кадра ещё нет), audio clock
//! не запущен и стоит на 0, а прогноза выхода ещё нет. При низком fps (например 10 fps)
//! в окно 500 мс помещается всего 5-6 packets — кадр не появляется, audio ждёт кадр,
//! часы не идут, окно не сдвигается: вечный `Buffering`. Поэтому, пока первый кадр
//! текущего preroll-а не получен, временное окно не удерживает packets вовсе
//! (`WaivedUntilFirstFrame`). После появления первого кадра окно действует как выше.

use std::time::Duration;

use crate::session::PlayerSession;

/// Typed лимит decode-ahead относительно audio clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session::tick) struct VideoDecoderDecodeAheadLimits {
    /// Лимит текущего режима admission: steady-state target или catch-up max.
    admission_limit: Duration,

    /// Жёсткий максимум из scheduler policy.
    max_limit: Duration,
}

impl VideoDecoderDecodeAheadLimits {
    /// Создаёт decode-ahead limits для текущего режима отправки packets.
    pub(in crate::session::tick) const fn new(
        admission_limit: Duration,
        max_limit: Duration,
    ) -> Self {
        Self {
            admission_limit,
            max_limit,
        }
    }

    /// Лимит текущего режима admission (для diagnostics).
    pub(super) const fn admission_limit(self) -> Duration {
        self.admission_limit
    }

    /// Жёсткий максимум scheduler policy (для diagnostics).
    pub(super) const fn max_limit(self) -> Duration {
        self.max_limit
    }

    /// Возвращает эффективный лимит, сохраняя старое fail-closed ограничение max.
    fn effective_limit(self) -> Duration {
        self.admission_limit.min(self.max_limit)
    }
}

/// Какое решение о time-based decode-ahead действует прямо сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VideoDecodeAheadAllowance {
    /// Обычный режим: packet не должен опережать audio clock больше лимита.
    Windowed {
        /// Эффективный лимит опережения.
        limit: Duration,
    },

    /// Первый кадр preroll-а ещё не получен: временное окно не применяется.
    WaivedUntilFirstFrame,
}

impl VideoDecodeAheadAllowance {
    /// Допускает ли решение packet, после которого выход decoder-а опередит audio clock
    /// на `decode_ahead_lead` (см. `video_decode_ahead_lead`).
    #[must_use]
    pub(super) fn admits(self, decode_ahead_lead: Duration) -> bool {
        match self {
            Self::Windowed { limit } => decode_ahead_lead <= limit,
            Self::WaivedUntilFirstFrame => true,
        }
    }
}

/// Выбирает действующее решение по состоянию session.
///
/// Решение «первый кадр ещё ждём» принадлежит session
/// (`video_first_frame_is_awaited`); здесь только применяются переданные лимиты.
#[must_use]
pub(super) fn video_decode_ahead_allowance(
    session: &PlayerSession,
    limits: VideoDecoderDecodeAheadLimits,
) -> VideoDecodeAheadAllowance {
    if session.video_first_frame_is_awaited() {
        VideoDecodeAheadAllowance::WaivedUntilFirstFrame
    } else {
        VideoDecodeAheadAllowance::Windowed {
            limit: limits.effective_limit(),
        }
    }
}

/// Насколько выход decoder-а опередит audio clock, если отправить packet с `packet_pts`.
///
/// Читает только прогноз выхода у владельца (pipeline) и audio clock; состояние не меняет.
#[must_use]
pub(super) fn video_decode_ahead_lead(session: &PlayerSession, packet_pts: Duration) -> Duration {
    let media_audio_now = session.pipeline.media_position_from_audio_clock();
    let front_pts = decode_ahead_front_pts(
        packet_pts,
        session.pipeline.projected_next_video_decoder_output_pts(),
    );

    front_pts.saturating_sub(media_audio_now)
}

/// PTS, опережение которого ограничивает окно: прогноз выхода decoder-а, но не позже packet-а.
///
/// `min` гарантирует, что правило не строже прежнего «по PTS packet-а», а без прогноза
/// (кадров в этой эпохе decoder-а ещё нет) остаётся ровно прежняя мера.
fn decode_ahead_front_pts(
    packet_pts: Duration,
    projected_next_output_pts: Option<Duration>,
) -> Duration {
    projected_next_output_pts.map_or(packet_pts, |projected| projected.min(packet_pts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_without_projection_is_packet_pts() {
        assert_eq!(
            decode_ahead_front_pts(Duration::from_millis(700), None),
            Duration::from_millis(700)
        );
    }

    #[test]
    fn front_uses_projected_output_when_decoder_holds_packets() {
        // Frame-threaded decoder: packet на 700 мс, а выйдет кадр на 200 мс.
        assert_eq!(
            decode_ahead_front_pts(Duration::from_millis(700), Some(Duration::from_millis(200))),
            Duration::from_millis(200)
        );
    }

    #[test]
    fn front_never_exceeds_packet_pts() {
        // Скачок PTS назад: старый прогноз больше packet-а — правило не строже прежнего.
        assert_eq!(
            decode_ahead_front_pts(Duration::from_millis(100), Some(Duration::from_secs(9))),
            Duration::from_millis(100)
        );
    }
}
