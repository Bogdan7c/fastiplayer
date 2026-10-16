//! Политика time-based decode-ahead для отправки video packets в decoder.
//!
//! Обычно video packet не уходит в decoder, если он опережает audio clock больше, чем на
//! лимит (`min(admission, max)`): так decoder не раздувает очередь кадров далеко вперёд
//! звука. Но пока стартовый preroll голоден по видео (первого кадра ещё нет), audio clock
//! не запущен и стоит на 0. Frame-threaded software decoder (FFmpeg, `thread_count` ≈
//! cores-2) выдаёт первый кадр только после ~`thread_count` packets; при низком fps
//! (например 10 fps) в окно 500 мс помещается всего 5-6 packets — кадр не появляется,
//! audio ждёт кадр, часы не идут, окно не сдвигается: вечный `Buffering`.
//!
//! Поэтому, пока первый кадр текущего preroll-а не получен, временное окно не удерживает
//! packets. Остальные ограничения (ёмкость decoder channel, pending packets, texture
//! slots, host-upload backpressure) продолжают действовать и держат память ограниченной.
//! После появления первого кадра окно действует ровно как раньше.

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
    /// Допускает ли решение packet, опережающий audio clock на `packet_lead`.
    #[must_use]
    pub(super) fn admits(self, packet_lead: Duration) -> bool {
        match self {
            Self::Windowed { limit } => packet_lead <= limit,
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
