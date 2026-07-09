//! Pause/resume lifecycle для [`AudioClock`].
//!
//! Модуль отделён от interpolation math: здесь владелец output-а фиксирует
//! clock на pause и вычитает wall-time паузы из будущих CPAL timestamp-ов.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use audio_core::AudioOutputClockTiming;

use super::AudioClock;

impl AudioClock {
    /// Возвращает текущее время воспроизведения.
    ///
    /// Основано на стабильной оценке CPAL playback anchor, а не на конце
    /// свежего callback-buffer. Frozen clock возвращает сохранённую координату.
    pub fn now(&self) -> Duration {
        if self.output_timing_is_frozen() {
            return self.samples_to_duration(self.frozen_audible_samples.load(Ordering::Acquire));
        }
        let audible_output_position = self.samples_to_duration(self.smoothed_played_samples());
        if self.output_timing_is_frozen() {
            return self.samples_to_duration(self.frozen_audible_samples.load(Ordering::Acquire));
        }
        audible_output_position
    }

    /// Возвращает audible progress и конец всего принятого real PCM.
    ///
    /// `samples_written` включает ring buffer и callback→DAC PCM, но не silence.
    pub fn output_timing(&self) -> AudioOutputClockTiming {
        if self.output_timing_is_frozen() {
            return self.frozen_output_timing();
        }
        let audible_output_position = self.now();
        let submitted_output_end_position =
            self.samples_to_duration(self.samples_written.load(Ordering::Relaxed));

        // Freeze между первым flag read и парой координат не должен дать hybrid.
        if self.output_timing_is_frozen() {
            return self.frozen_output_timing();
        }

        AudioOutputClockTiming::new(audible_output_position, submitted_output_end_position)
    }

    /// Возвращает `true`, когда callback должен выдавать silence и не брать PCM.
    pub(crate) fn output_timing_is_frozen(&self) -> bool {
        self.playback_frozen.load(Ordering::Acquire)
    }

    /// Фиксирует audible/submitted координаты одним lifecycle snapshot-ом.
    ///
    /// `AudioOutput` вызывает метод только под consumer mutex: callback либо
    /// полностью опубликовал предыдущий anchor, либо увидит уже frozen state.
    pub(crate) fn freeze_output_timing(&self) -> AudioOutputClockTiming {
        if self.output_timing_is_frozen() {
            return self.frozen_output_timing();
        }

        let audible_samples = self.smoothed_played_samples();
        let submitted_end_samples = self.samples_written.load(Ordering::Relaxed);
        self.frozen_audible_samples
            .store(audible_samples, Ordering::Relaxed);
        self.frozen_submitted_end_samples
            .store(submitted_end_samples, Ordering::Relaxed);
        self.frozen_at_origin_ns
            .store(self.current_origin_ns(), Ordering::Relaxed);
        self.playback_frozen.store(true, Ordering::Release);

        self.frozen_output_timing()
    }

    /// Возобновляет interpolation без прыжка по устаревшему pre-pause anchor-у.
    ///
    /// Anchor timestamps остаются в logical clock coordinate. Поэтому wall-time,
    /// проведённый на pause, добавляется к offset до снятия frozen flag-а.
    pub(crate) fn resume_output_timing(&self) {
        if !self.output_timing_is_frozen() {
            return;
        }

        let frozen_at_origin_ns = self.frozen_at_origin_ns.load(Ordering::Relaxed);
        let paused_ns = self.current_origin_ns().saturating_sub(frozen_at_origin_ns);
        let _ = self.paused_origin_offset_ns.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |offset_ns| Some(offset_ns.saturating_add(paused_ns)),
        );
        self.playback_frozen.store(false, Ordering::Release);
    }

    /// Возвращает сохранённый neutral timing без повторной interpolation.
    pub(super) fn frozen_output_timing(&self) -> AudioOutputClockTiming {
        AudioOutputClockTiming::new(
            self.samples_to_duration(self.frozen_audible_samples.load(Ordering::Acquire)),
            self.samples_to_duration(self.frozen_submitted_end_samples.load(Ordering::Acquire)),
        )
    }

    /// Возвращает реальный monotonic offset от origin в наносекундах.
    pub(super) fn current_origin_ns(&self) -> u64 {
        self.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64
    }

    /// Возвращает clock coordinate, из которой исключён wall-time всех пауз.
    pub(super) fn current_logical_origin_ns(&self) -> u64 {
        self.current_origin_ns()
            .saturating_sub(self.paused_origin_offset_ns.load(Ordering::Acquire))
    }

    /// Переводит backend timestamp в ту же logical coordinate.
    pub(super) fn logical_origin_ns_for_instant(&self, instant: Instant) -> u64 {
        let origin_ns = instant
            .saturating_duration_since(self.origin)
            .as_nanos()
            .min(u64::MAX as u128) as u64;
        origin_ns.saturating_sub(self.paused_origin_offset_ns.load(Ordering::Acquire))
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::AudioClock;

    #[test]
    fn frozen_clock_ignores_wall_time_until_resume() {
        let clock = AudioClock::new(1_000, 1);
        clock.record_written(200);
        clock.record_played(100);

        let frozen = clock.freeze_output_timing();
        thread::sleep(Duration::from_millis(5));

        assert_eq!(clock.now(), frozen.audible_output_position());
        assert_eq!(clock.output_timing(), frozen);
    }

    #[test]
    fn reset_while_frozen_keeps_clock_frozen_at_zero() {
        let clock = AudioClock::new(1_000, 1);
        clock.record_written(200);
        clock.record_played(100);
        clock.freeze_output_timing();

        clock.reset();

        assert_eq!(clock.now(), Duration::ZERO);
        assert_eq!(
            clock.output_timing().submitted_output_end_position(),
            Duration::ZERO
        );
        assert!(clock.output_timing_is_frozen());
    }

    #[test]
    fn resume_preserves_frozen_position_without_stale_wall_time_jump() {
        let clock = AudioClock::new(1_000, 1);
        clock.record_played(100);
        let frozen = clock.freeze_output_timing();
        thread::sleep(Duration::from_millis(5));

        clock.resume_output_timing();

        assert_eq!(clock.now(), frozen.audible_output_position());
        assert!(!clock.output_timing_is_frozen());
    }
}
