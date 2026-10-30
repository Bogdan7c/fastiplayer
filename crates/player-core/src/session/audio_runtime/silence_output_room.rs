//! Влезет ли тишина, растянутая скоростью, в свободное место вывода звука.
//!
//! Тишина перед звуком (начало отрезка — `leading_silence.rs`, дыра посреди отрезка —
//! `mid_stream_gap.rs`) пишется в output одним куском. Кусок идёт через tempo processor
//! и растягивается скоростью: 500 мс тишины при 0,25× — это 2 с вывода. Ring buffer
//! вывода держит ~2 с (`audio::output`), а запись, не влезшая в свободное место, —
//! частичная запись, которую плеер считает фатальной (`ensure_audio_output_written`).
//!
//! Поэтому до записи тишины session спрашивает здесь, влезет ли она. Не влезает — тишина
//! не пишется вовсе: звук ждёт своего PTS в очереди (удержание до PTS при старте отрезка
//! и передача часов разрыва посреди отрезка), а не роняет воспроизведение.
//!
//! Предел заполнения — тот же, что у стартового read-ahead
//! ([`fastiplayer_config::MAX_AUDIO_STARTUP_READ_AHEAD_CAP_MS`], 1,8 с): ёмкость ring-а
//! минус запас под декодированный packet, который пишется сразу за тишиной.

use std::time::Duration;

use crate::PlaybackRate;

use super::super::PlayerSession;

/// Сколько вывода (уже лежащего в ring-е плюс тишина) можно держать, оставив место
/// под packet, записываемый сразу после тишины.
const MAX_BUFFERED_OUTPUT_WITH_SILENCE: Duration =
    Duration::from_millis(fastiplayer_config::MAX_AUDIO_STARTUP_READ_AHEAD_CAP_MS);

/// Влезает ли тишина в свободное место вывода.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::session) enum SilenceOutputRoom {
    /// Тишину можно записать одним куском.
    Fits,

    /// Растянутая тишина больше свободного места: запись была бы частичной.
    Exceeds {
        /// Длина тишины в выводе (после tempo processor-а).
        stretched_silence: Duration,

        /// Сколько вывода ещё можно записать.
        free_room: Duration,
    },
}

impl SilenceOutputRoom {
    /// `true`, если тишину можно записать.
    #[must_use]
    pub(in crate::session) const fn fits(self) -> bool {
        matches!(self, Self::Fits)
    }
}

/// Чистый расчёт: влезет ли `silence` media-времени при скорости `playback_rate`, если
/// в выводе уже лежит `buffered_output`.
#[must_use]
pub(in crate::session) fn plan_silence_output_room(
    silence: Duration,
    playback_rate: PlaybackRate,
    buffered_output: Duration,
) -> SilenceOutputRoom {
    // Скорость 0,25× растягивает media-время в 4 раза: вывод длиннее тишины в 1/rate раз.
    let stretched_silence = silence.div_f64(f64::from(playback_rate.as_f32()));
    let free_room = MAX_BUFFERED_OUTPUT_WITH_SILENCE.saturating_sub(buffered_output);
    if stretched_silence <= free_room {
        SilenceOutputRoom::Fits
    } else {
        SilenceOutputRoom::Exceeds {
            stretched_silence,
            free_room,
        }
    }
}

impl PlayerSession {
    /// Влезет ли тишина длиной `silence` (media-время) в вывод прямо сейчас.
    ///
    /// Output-а ещё нет — вывод пуст: первый PCM создаст output с пустым ring-ом.
    #[must_use]
    pub(in crate::session) fn silence_output_room(&self, silence: Duration) -> SilenceOutputRoom {
        let buffered_output_ms = self.audio_buffer_level_ms().unwrap_or(0.0);
        let buffered_output = if buffered_output_ms.is_finite() && buffered_output_ms > 0.0 {
            Duration::from_secs_f64(buffered_output_ms / 1_000.0)
        } else {
            Duration::ZERO
        };
        plan_silence_output_room(silence, self.snapshot.playback_rate, buffered_output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Скорость из теста (все значения в допустимом диапазоне).
    fn rate(multiplier: f32) -> PlaybackRate {
        PlaybackRate::new(multiplier).expect("valid test rate")
    }

    #[test]
    fn silence_at_normal_rate_fits_an_empty_output_up_to_the_fill_limit() {
        assert_eq!(
            plan_silence_output_room(
                MAX_BUFFERED_OUTPUT_WITH_SILENCE,
                PlaybackRate::NORMAL,
                Duration::ZERO
            ),
            SilenceOutputRoom::Fits
        );
        assert!(
            !plan_silence_output_room(
                MAX_BUFFERED_OUTPUT_WITH_SILENCE + Duration::from_millis(1),
                PlaybackRate::NORMAL,
                Duration::ZERO
            )
            .fits()
        );
    }

    #[test]
    fn low_rate_stretches_the_silence_before_comparing() {
        // 500 мс при 0,25× = 2 с вывода: больше пустого ring-а с запасом.
        assert_eq!(
            plan_silence_output_room(Duration::from_millis(500), rate(0.25), Duration::ZERO),
            SilenceOutputRoom::Exceeds {
                stretched_silence: Duration::from_secs(2),
                free_room: MAX_BUFFERED_OUTPUT_WITH_SILENCE,
            }
        );
        // 400 мс при 0,25× = 1,6 с: в пустой вывод влезает.
        assert!(
            plan_silence_output_room(Duration::from_millis(400), rate(0.25), Duration::ZERO).fits()
        );
        // 1 с при 0,5× = 2 с: потолок тишины начала отрезка больше не влезает.
        assert!(
            !plan_silence_output_room(Duration::from_secs(1), rate(0.5), Duration::ZERO).fits()
        );
    }

    #[test]
    fn audio_already_in_the_output_takes_room_from_the_silence() {
        let buffered = Duration::from_millis(1_500);

        assert_eq!(
            plan_silence_output_room(Duration::from_millis(400), PlaybackRate::NORMAL, buffered),
            SilenceOutputRoom::Exceeds {
                stretched_silence: Duration::from_millis(400),
                free_room: Duration::from_millis(300),
            }
        );
        assert!(
            plan_silence_output_room(Duration::from_millis(300), PlaybackRate::NORMAL, buffered)
                .fits()
        );
    }

    #[test]
    fn high_rate_shrinks_the_silence() {
        // 1 с при 2× — 0,5 с вывода: влезает даже рядом с полным read-ahead.
        assert!(
            plan_silence_output_room(
                Duration::from_secs(1),
                rate(2.0),
                Duration::from_millis(1_300)
            )
            .fits()
        );
    }
}
