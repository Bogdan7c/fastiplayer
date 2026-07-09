//! Audio clock — source of truth для A/V синхронизации.
//!
//! Отслеживает время воспроизведения по CPAL playback timestamp-ам,
//! потому что callback заранее заполняет будущий output buffer.
//! `samples_played` нужен для buffer accounting, а A/V sync берёт
//! только безопасную оценку того, что уже дошло до playback clock.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

mod lifecycle;

/// Clock для audio playback.
///
/// Основные счётчики:
/// - samples_written: сколько сэмплов записано в ring buffer (из main thread)
/// - samples_played: сколько interleaved сэмплов прочитано из ring buffer (из CPAL callback)
/// - last_stable_played_samples: что безопасно отдавать наружу как media time
///
/// Разница между ними = уровень заполнения buffer.
pub struct AudioClock {
    /// Локальный origin для перевода `std::time::Instant` в атомарные наносекунды.
    origin: Instant,

    /// Sample rate в Гц (например, 48000).
    sample_rate: u32,

    /// Количество каналов (1 = mono, 2 = stereo).
    channels: u32,

    /// Общее количество interleaved сэмплов, записанных в buffer.
    samples_written: AtomicU64,

    /// Общее количество interleaved сэмплов, прочитанных из buffer.
    samples_played: AtomicU64,

    /// Последняя монотонная оценка media sample index, безопасная для `now()`.
    last_stable_played_samples: AtomicU64,

    /// Logical freeze: clock не интерполирует wall-time, пока output на паузе.
    playback_frozen: AtomicBool,

    /// Audible sample index, атомарно зафиксированный владельцем output-а.
    frozen_audible_samples: AtomicU64,

    /// Конец принятого PCM, зафиксированный в том же pause snapshot-е.
    frozen_submitted_end_samples: AtomicU64,

    /// Реальный monotonic момент freeze в наносекундах от `origin`.
    frozen_at_origin_ns: AtomicU64,

    /// Суммарный wall-time пауз, исключённый из clock interpolation.
    paused_origin_offset_ns: AtomicU64,

    /// Флаг: есть валидный CPAL playback anchor.
    playback_anchor_valid: AtomicBool,

    /// Seqlock-счётчик для атомарного чтения нескольких playback anchor полей.
    playback_anchor_generation: AtomicU64,

    /// Флаг: есть предыдущий CPAL playback anchor, который ещё может быть активен.
    previous_playback_anchor_valid: AtomicBool,

    /// `std::time::Instant` предыдущего playback anchor, в наносекундах от `origin`.
    previous_playback_anchor_ns: AtomicU64,

    /// Media sample index первого сэмпла предыдущего CPAL callback.
    previous_playback_anchor_samples: AtomicU64,

    /// Media sample index конца реально заполненной audio-части предыдущего callback.
    previous_playback_anchor_end_samples: AtomicU64,

    /// Время конца всего предыдущего output callback, включая silence.
    previous_playback_anchor_output_end_ns: AtomicU64,

    /// `std::time::Instant` предполагаемого playback свежего anchor, в наносекундах от `origin`.
    playback_anchor_ns: AtomicU64,

    /// Media sample index первого сэмпла свежего CPAL callback.
    playback_anchor_samples: AtomicU64,

    /// Media sample index конца реально заполненной audio-части свежего callback.
    playback_anchor_end_samples: AtomicU64,

    /// Время конца всего свежего output callback, включая silence.
    playback_anchor_output_end_ns: AtomicU64,

    /// Количество CPAL callbacks, где пришлось дописать silence.
    underrun_callbacks: AtomicU64,

    /// Количество silence samples, добавленных из-за underrun.
    underrun_samples: AtomicU64,
}

impl AudioClock {
    /// Создаёт новый clock с заданной частотой дискретизации.
    pub fn new(sample_rate: u32, channels: u32) -> Self {
        Self {
            origin: Instant::now(),
            sample_rate,
            channels,
            samples_written: AtomicU64::new(0),
            samples_played: AtomicU64::new(0),
            last_stable_played_samples: AtomicU64::new(0),
            playback_frozen: AtomicBool::new(false),
            frozen_audible_samples: AtomicU64::new(0),
            frozen_submitted_end_samples: AtomicU64::new(0),
            frozen_at_origin_ns: AtomicU64::new(0),
            paused_origin_offset_ns: AtomicU64::new(0),
            playback_anchor_valid: AtomicBool::new(false),
            playback_anchor_generation: AtomicU64::new(0),
            previous_playback_anchor_valid: AtomicBool::new(false),
            previous_playback_anchor_ns: AtomicU64::new(0),
            previous_playback_anchor_samples: AtomicU64::new(0),
            previous_playback_anchor_end_samples: AtomicU64::new(0),
            previous_playback_anchor_output_end_ns: AtomicU64::new(0),
            playback_anchor_ns: AtomicU64::new(0),
            playback_anchor_samples: AtomicU64::new(0),
            playback_anchor_end_samples: AtomicU64::new(0),
            playback_anchor_output_end_ns: AtomicU64::new(0),
            underrun_callbacks: AtomicU64::new(0),
            underrun_samples: AtomicU64::new(0),
        }
    }

    /// Возвращает плавную оценку media sample index, который сейчас слышит пользователь.
    fn smoothed_played_samples(&self) -> u64 {
        let samples_per_sec = self.samples_per_second();
        if samples_per_sec == 0 {
            return 0;
        }

        let generation_before = self.playback_anchor_generation.load(Ordering::Acquire);
        if generation_before % 2 != 0 {
            return self.stable_played_samples();
        }

        let previous_anchor_valid = self.previous_playback_anchor_valid.load(Ordering::Relaxed);
        let previous_anchor = PlaybackAnchor {
            playback_ns: self.previous_playback_anchor_ns.load(Ordering::Relaxed),
            start_samples: self
                .previous_playback_anchor_samples
                .load(Ordering::Relaxed),
            end_samples: self
                .previous_playback_anchor_end_samples
                .load(Ordering::Relaxed),
            output_end_ns: self
                .previous_playback_anchor_output_end_ns
                .load(Ordering::Relaxed),
        };
        let latest_anchor_valid = self.playback_anchor_valid.load(Ordering::Relaxed);
        let latest_anchor = PlaybackAnchor {
            playback_ns: self.playback_anchor_ns.load(Ordering::Relaxed),
            start_samples: self.playback_anchor_samples.load(Ordering::Relaxed),
            end_samples: self.playback_anchor_end_samples.load(Ordering::Relaxed),
            output_end_ns: self.playback_anchor_output_end_ns.load(Ordering::Relaxed),
        };
        let generation_after = self.playback_anchor_generation.load(Ordering::Acquire);
        let current_ns = self.current_logical_origin_ns();

        let estimated_samples = estimate_samples_from_anchor_snapshot(
            generation_before,
            generation_after,
            previous_anchor_valid.then_some(previous_anchor),
            latest_anchor_valid.then_some(latest_anchor),
            current_ns,
            samples_per_sec,
            self.stable_played_samples(),
        );

        self.publish_stable_played_samples(estimated_samples)
    }

    /// Возвращает последний sample index, который уже был получен из
    /// согласованного playback anchor или из legacy path без CPAL timestamp.
    fn stable_played_samples(&self) -> u64 {
        self.last_stable_played_samples.load(Ordering::Acquire)
    }

    /// Публикует безопасную оценку монотонно: clock может стоять, но не
    /// должен откатываться назад из-за jitter-а backend timestamp.
    fn publish_stable_played_samples(&self, candidate_samples: u64) -> u64 {
        let mut stable_samples = self.last_stable_played_samples.load(Ordering::Acquire);

        while candidate_samples > stable_samples {
            match self.last_stable_played_samples.compare_exchange(
                stable_samples,
                candidate_samples,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return candidate_samples,
                Err(observed_samples) => stable_samples = observed_samples,
            }
        }

        stable_samples
    }

    /// Возвращает число interleaved samples в секунду.
    fn samples_per_second(&self) -> u64 {
        (self.sample_rate as u64) * (self.channels as u64)
    }

    /// Переводит interleaved sample index в media time.
    fn samples_to_duration(&self, samples: u64) -> Duration {
        let samples_per_sec = self.samples_per_second();
        if samples_per_sec == 0 {
            return Duration::ZERO;
        }

        let secs = samples / samples_per_sec;
        let remainder = samples % samples_per_sec;
        // Конвертируем remainder в наносекунды для более плавного video scheduling.
        let nanos = (remainder * 1_000_000_000) / samples_per_sec;
        Duration::from_secs(secs) + Duration::from_nanos(nanos)
    }

    /// Возвращает текущее время в секундах как f64.
    pub fn now_secs(&self) -> f64 {
        self.now().as_secs_f64()
    }

    /// Записывает количество сэмплов, добавленных в buffer.
    ///
    /// Вызывается из main thread после decode → write_samples.
    /// `samples` — это total interleaved samples (frames * channels).
    pub fn record_written(&self, samples: u64) {
        self.samples_written.fetch_add(samples, Ordering::Relaxed);
    }

    /// Записывает количество сэмплов, прочитанных из buffer.
    ///
    /// Вызывается из CPAL callback при заполнении output buffer.
    /// `samples` — это total interleaved samples.
    pub fn record_played(&self, samples: u64) {
        let samples_before = self.samples_played.fetch_add(samples, Ordering::Relaxed);
        let samples_after = samples_before.saturating_add(samples);

        self.publish_stable_played_samples(samples_after);
    }

    /// Записывает результат одного CPAL output callback вместе с playback timestamp.
    ///
    /// CPAL сообщает два времени: когда callback вызван и когда записанные сэмплы
    /// предположительно дойдут до DAC. Мы используем эту пару как anchor, чтобы
    /// render thread видел плавный audio clock внутри callback interval, а не ступеньки.
    pub fn record_output_callback(
        &self,
        played_samples: u64,
        silence_samples: u64,
        callback_info: &cpal::OutputCallbackInfo,
        callback_observed_at: Instant,
    ) {
        if silence_samples > 0 {
            self.underrun_callbacks.fetch_add(1, Ordering::Relaxed);
            self.underrun_samples
                .fetch_add(silence_samples, Ordering::Relaxed);
        }

        let samples_before_callback = if played_samples > 0 {
            self.samples_played
                .fetch_add(played_samples, Ordering::Relaxed)
        } else {
            self.samples_played.load(Ordering::Relaxed)
        };
        let samples_after_callback = samples_before_callback.saturating_add(played_samples);

        if played_samples == 0 {
            return;
        }

        let timestamp = callback_info.timestamp();
        let playback_delay = timestamp
            .playback
            .duration_since(&timestamp.callback)
            .unwrap_or(Duration::ZERO);
        let playback_at = callback_observed_at
            .checked_add(playback_delay)
            .unwrap_or(callback_observed_at);
        let reported_playback_ns = self.logical_origin_ns_for_instant(playback_at);
        let callback_output_samples = played_samples.saturating_add(silence_samples);
        let callback_output_duration_ns =
            samples_to_nanos(callback_output_samples, self.samples_per_second());

        self.begin_playback_anchor_write();

        let previous_anchor_valid = self.playback_anchor_valid.load(Ordering::Relaxed);
        let previous_anchor = PlaybackAnchor {
            playback_ns: self.playback_anchor_ns.load(Ordering::Relaxed),
            start_samples: self.playback_anchor_samples.load(Ordering::Relaxed),
            end_samples: self.playback_anchor_end_samples.load(Ordering::Relaxed),
            output_end_ns: self.playback_anchor_output_end_ns.load(Ordering::Relaxed),
        };
        let latest_playback_ns = chain_reported_playback_ns(
            reported_playback_ns,
            previous_anchor_valid.then_some(previous_anchor),
        );
        let latest_output_end_ns = latest_playback_ns.saturating_add(callback_output_duration_ns);

        self.previous_playback_anchor_ns
            .store(previous_anchor.playback_ns, Ordering::Relaxed);
        self.previous_playback_anchor_samples
            .store(previous_anchor.start_samples, Ordering::Relaxed);
        self.previous_playback_anchor_end_samples
            .store(previous_anchor.end_samples, Ordering::Relaxed);
        self.previous_playback_anchor_output_end_ns
            .store(previous_anchor.output_end_ns, Ordering::Relaxed);
        self.previous_playback_anchor_valid
            .store(previous_anchor_valid, Ordering::Relaxed);

        self.playback_anchor_samples
            .store(samples_before_callback, Ordering::Relaxed);
        self.playback_anchor_end_samples
            .store(samples_after_callback, Ordering::Relaxed);
        self.playback_anchor_ns
            .store(latest_playback_ns, Ordering::Relaxed);
        self.playback_anchor_output_end_ns
            .store(latest_output_end_ns, Ordering::Relaxed);
        self.playback_anchor_valid.store(true, Ordering::Relaxed);
        self.finish_playback_anchor_write();
    }

    /// Возвращает количество сэмплов, ожидающих в buffer.
    ///
    /// Положительное значение = buffer заполнен.
    /// Ноль или близкое к нулю = buffer пуст (risk of underrun).
    pub fn buffer_level(&self) -> i64 {
        let written = self.samples_written.load(Ordering::Relaxed);
        let played = self.samples_played.load(Ordering::Relaxed);
        written as i64 - played as i64
    }

    /// Возвращает sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Возвращает количество каналов.
    pub fn channels(&self) -> u32 {
        self.channels
    }

    /// Возвращает количество callbacks с audio underrun.
    pub fn underrun_callbacks(&self) -> u64 {
        self.underrun_callbacks.load(Ordering::Relaxed)
    }

    /// Возвращает количество silence samples, добавленных из-за underrun.
    pub fn underrun_samples(&self) -> u64 {
        self.underrun_samples.load(Ordering::Relaxed)
    }

    /// Сбрасывает clock в начальное состояние.
    pub fn reset(&self) {
        self.samples_written.store(0, Ordering::Relaxed);
        self.samples_played.store(0, Ordering::Relaxed);
        self.last_stable_played_samples.store(0, Ordering::Release);
        self.frozen_audible_samples.store(0, Ordering::Relaxed);
        self.frozen_submitted_end_samples
            .store(0, Ordering::Relaxed);

        self.begin_playback_anchor_write();
        self.playback_anchor_valid.store(false, Ordering::Relaxed);
        self.previous_playback_anchor_valid
            .store(false, Ordering::Relaxed);
        self.previous_playback_anchor_ns.store(0, Ordering::Relaxed);
        self.previous_playback_anchor_samples
            .store(0, Ordering::Relaxed);
        self.previous_playback_anchor_end_samples
            .store(0, Ordering::Relaxed);
        self.previous_playback_anchor_output_end_ns
            .store(0, Ordering::Relaxed);
        self.playback_anchor_ns.store(0, Ordering::Relaxed);
        self.playback_anchor_samples.store(0, Ordering::Relaxed);
        self.playback_anchor_end_samples.store(0, Ordering::Relaxed);
        self.playback_anchor_output_end_ns
            .store(0, Ordering::Relaxed);
        self.finish_playback_anchor_write();

        self.underrun_callbacks.store(0, Ordering::Relaxed);
        self.underrun_samples.store(0, Ordering::Relaxed);
    }

    /// Начинает короткую запись группы playback anchor полей.
    fn begin_playback_anchor_write(&self) {
        loop {
            let generation = self.playback_anchor_generation.load(Ordering::Acquire);
            if generation % 2 != 0 {
                std::hint::spin_loop();
                continue;
            }

            if self
                .playback_anchor_generation
                .compare_exchange(
                    generation,
                    generation.wrapping_add(1),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return;
            }
        }
    }

    /// Завершает запись группы playback anchor полей.
    fn finish_playback_anchor_write(&self) {
        self.playback_anchor_generation
            .fetch_add(1, Ordering::Release);
    }
}

/// Непрерывный отрезок audio samples, привязанный к CPAL playback timestamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlaybackAnchor {
    /// Момент, когда первый sample отрезка должен быть услышан пользователем.
    playback_ns: u64,

    /// Первый interleaved sample отрезка.
    start_samples: u64,

    /// Конец реально заполненной audio-части отрезка.
    end_samples: u64,

    /// Момент, когда весь output callback уже должен выйти из устройства.
    output_end_ns: u64,
}

/// Защищает clock от backend timestamp, который приходит раньше конца предыдущего callback.
fn chain_reported_playback_ns(
    reported_playback_ns: u64,
    previous_anchor: Option<PlaybackAnchor>,
) -> u64 {
    previous_anchor
        .map(|anchor| reported_playback_ns.max(anchor.output_end_ns))
        .unwrap_or(reported_playback_ns)
}

/// Выбирает anchor, который уже можно показывать render thread.
fn select_playback_anchor(
    previous_anchor: Option<PlaybackAnchor>,
    latest_anchor: Option<PlaybackAnchor>,
    current_ns: u64,
) -> Option<PlaybackAnchor> {
    match (previous_anchor, latest_anchor) {
        (_, Some(latest_anchor)) if current_ns >= latest_anchor.playback_ns => Some(latest_anchor),
        (Some(previous_anchor), Some(_future_anchor)) => Some(previous_anchor),
        (None, Some(latest_anchor)) => Some(latest_anchor),
        (Some(previous_anchor), None) => Some(previous_anchor),
        (None, None) => None,
    }
}

/// Превращает согласованный seqlock snapshot в безопасную playback-оценку.
fn estimate_samples_from_anchor_snapshot(
    generation_before: u64,
    generation_after: u64,
    previous_anchor: Option<PlaybackAnchor>,
    latest_anchor: Option<PlaybackAnchor>,
    current_ns: u64,
    samples_per_sec: u64,
    stable_fallback_samples: u64,
) -> u64 {
    if generation_before % 2 != 0 || generation_before != generation_after {
        return stable_fallback_samples;
    }

    select_playback_anchor(previous_anchor, latest_anchor, current_ns)
        .map(|anchor| interpolate_anchor_samples(anchor, current_ns, samples_per_sec))
        .unwrap_or(stable_fallback_samples)
}

/// Интерполирует media sample index внутри выбранного CPAL playback anchor.
fn interpolate_anchor_samples(
    playback_anchor: PlaybackAnchor,
    current_ns: u64,
    samples_per_sec: u64,
) -> u64 {
    if current_ns < playback_anchor.playback_ns {
        let lead_ns = playback_anchor.playback_ns - current_ns;
        let lead_samples = nanos_to_samples(lead_ns, samples_per_sec);
        return playback_anchor.start_samples.saturating_sub(lead_samples);
    }

    let elapsed_ns = current_ns - playback_anchor.playback_ns;
    let progressed_samples = nanos_to_samples(elapsed_ns, samples_per_sec);

    playback_anchor
        .start_samples
        .saturating_add(progressed_samples)
        .min(playback_anchor.end_samples)
}

/// Переводит количество interleaved samples в наносекунды.
fn samples_to_nanos(samples: u64, samples_per_sec: u64) -> u64 {
    if samples_per_sec == 0 {
        return 0;
    }

    ((samples as u128 * 1_000_000_000u128) / samples_per_sec as u128).min(u64::MAX as u128) as u64
}

/// Переводит наносекунды в количество interleaved samples.
fn nanos_to_samples(nanos: u64, samples_per_sec: u64) -> u64 {
    if samples_per_sec == 0 {
        return 0;
    }

    ((nanos as u128 * samples_per_sec as u128) / 1_000_000_000u128).min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use super::{
        AudioClock, PlaybackAnchor, chain_reported_playback_ns,
        estimate_samples_from_anchor_snapshot, interpolate_anchor_samples, samples_to_nanos,
        select_playback_anchor,
    };

    #[test]
    fn keeps_previous_anchor_until_latest_playback_time() {
        let previous_anchor = PlaybackAnchor {
            playback_ns: 100,
            start_samples: 0,
            end_samples: 4_800,
            output_end_ns: 150,
        };
        let future_anchor = PlaybackAnchor {
            playback_ns: 150,
            start_samples: 4_800,
            end_samples: 9_600,
            output_end_ns: 200,
        };

        let selected_anchor =
            select_playback_anchor(Some(previous_anchor), Some(future_anchor), 140);

        assert_eq!(selected_anchor, Some(previous_anchor));
    }

    #[test]
    fn switches_to_latest_anchor_at_its_playback_time() {
        let previous_anchor = PlaybackAnchor {
            playback_ns: 100,
            start_samples: 0,
            end_samples: 4_800,
            output_end_ns: 150,
        };
        let latest_anchor = PlaybackAnchor {
            playback_ns: 150,
            start_samples: 4_800,
            end_samples: 9_600,
            output_end_ns: 200,
        };

        let selected_anchor =
            select_playback_anchor(Some(previous_anchor), Some(latest_anchor), 150);

        assert_eq!(selected_anchor, Some(latest_anchor));
    }

    #[test]
    fn unfinished_anchor_write_uses_stable_estimate_not_fresh_callback_end() {
        let clock = AudioClock::new(48_000, 2);
        clock.publish_stable_played_samples(4_800);
        clock.samples_played.store(9_600, Ordering::Relaxed);
        clock.playback_anchor_generation.store(1, Ordering::Release);

        let samples = clock.smoothed_played_samples();

        assert_eq!(samples, 4_800);
    }

    #[test]
    fn missing_anchor_uses_stable_estimate_not_raw_played_counter() {
        let clock = AudioClock::new(48_000, 2);
        clock.publish_stable_played_samples(4_800);
        clock.samples_played.store(9_600, Ordering::Relaxed);

        let samples = clock.smoothed_played_samples();

        assert_eq!(samples, 4_800);
    }

    #[test]
    fn generation_conflict_fallback_does_not_jump_to_fresh_callback_end() {
        let previous_anchor = PlaybackAnchor {
            playback_ns: 0,
            start_samples: 0,
            end_samples: 4_800,
            output_end_ns: 50_000_000,
        };
        let latest_anchor = PlaybackAnchor {
            playback_ns: 50_000_000,
            start_samples: 4_800,
            end_samples: 9_600,
            output_end_ns: 100_000_000,
        };

        let samples = estimate_samples_from_anchor_snapshot(
            2,
            4,
            Some(previous_anchor),
            Some(latest_anchor),
            75_000_000,
            96_000,
            4_800,
        );

        assert_eq!(samples, 4_800);
    }

    #[test]
    fn consistent_anchor_snapshot_preserves_normal_interpolation() {
        let latest_anchor = PlaybackAnchor {
            playback_ns: 0,
            start_samples: 4_800,
            end_samples: 9_600,
            output_end_ns: 50_000_000,
        };

        let samples = estimate_samples_from_anchor_snapshot(
            2,
            2,
            None,
            Some(latest_anchor),
            25_000_000,
            96_000,
            4_800,
        );

        assert_eq!(samples, 7_200);
    }

    #[test]
    fn clamps_interpolation_to_anchor_end() {
        let playback_anchor = PlaybackAnchor {
            playback_ns: 0,
            start_samples: 1_000,
            end_samples: 1_500,
            output_end_ns: 20_000_000,
        };

        let samples = interpolate_anchor_samples(playback_anchor, 20_000_000, 96_000);

        assert_eq!(samples, 1_500);
    }

    #[test]
    fn interpolates_before_future_anchor_start() {
        let playback_anchor = PlaybackAnchor {
            playback_ns: 50_000_000,
            start_samples: 4_800,
            end_samples: 9_600,
            output_end_ns: 100_000_000,
        };

        let samples = interpolate_anchor_samples(playback_anchor, 25_000_000, 96_000);

        assert_eq!(samples, 2_400);
    }

    #[test]
    fn future_anchor_interpolation_saturates_at_zero() {
        let playback_anchor = PlaybackAnchor {
            playback_ns: 50_000_000,
            start_samples: 1_000,
            end_samples: 5_800,
            output_end_ns: 100_000_000,
        };

        let samples = interpolate_anchor_samples(playback_anchor, 25_000_000, 96_000);

        assert_eq!(samples, 0);
    }

    #[test]
    fn silence_tail_does_not_advance_media_clock_past_real_audio() {
        let playback_anchor = PlaybackAnchor {
            playback_ns: 0,
            start_samples: 4_800,
            end_samples: 4_800,
            output_end_ns: 50_000_000,
        };

        let samples = interpolate_anchor_samples(playback_anchor, 25_000_000, 96_000);

        assert_eq!(samples, 4_800);
    }

    #[test]
    fn output_timing_includes_ring_and_pcm_waiting_for_future_dac_playback() {
        let clock = AudioClock::new(48_000, 2);
        let samples_per_second = 96_000u64;
        let callback_pcm_samples = 4_800u64;
        let ring_pcm_samples = 4_800u64;
        let submitted_pcm_samples = callback_pcm_samples + ring_pcm_samples;
        let future_playback_ns = clock
            .origin
            .elapsed()
            .saturating_add(Duration::from_secs(1))
            .as_nanos() as u64;

        clock.record_written(submitted_pcm_samples);
        clock
            .samples_played
            .store(callback_pcm_samples, Ordering::Relaxed);
        clock.playback_anchor_samples.store(0, Ordering::Relaxed);
        clock
            .playback_anchor_end_samples
            .store(callback_pcm_samples, Ordering::Relaxed);
        clock
            .playback_anchor_ns
            .store(future_playback_ns, Ordering::Relaxed);
        clock.playback_anchor_output_end_ns.store(
            future_playback_ns
                .saturating_add(samples_to_nanos(callback_pcm_samples, samples_per_second)),
            Ordering::Relaxed,
        );
        clock.playback_anchor_valid.store(true, Ordering::Relaxed);

        let timing = clock.output_timing();

        assert_eq!(timing.audible_output_position(), Duration::ZERO);
        assert_eq!(
            timing.submitted_output_end_position(),
            Duration::from_millis(100)
        );
        assert_eq!(timing.submitted_output_tail(), Duration::from_millis(100));
    }

    #[test]
    fn callback_silence_does_not_extend_submitted_pcm_end() {
        let clock = AudioClock::new(48_000, 2);
        let real_pcm_samples = 4_800u64;

        clock.record_written(real_pcm_samples);
        clock.underrun_callbacks.store(1, Ordering::Relaxed);
        clock.underrun_samples.store(9_600, Ordering::Relaxed);

        // Callback silence никогда не проходит через `record_written`: даже
        // полный будущий callback interval не является media PCM tail.
        let timing = clock.output_timing();

        assert_eq!(
            timing.submitted_output_end_position(),
            Duration::from_millis(50)
        );
    }

    #[test]
    fn reset_clears_stable_played_samples() {
        let clock = AudioClock::new(48_000, 2);
        clock.record_written(9_600);
        clock.record_played(9_600);

        clock.reset();

        assert_eq!(clock.smoothed_played_samples(), 0);
        let timing = clock.output_timing();
        assert_eq!(timing.audible_output_position(), Duration::ZERO);
        assert_eq!(timing.submitted_output_end_position(), Duration::ZERO);
        assert_eq!(timing.submitted_output_tail(), Duration::ZERO);
    }

    #[test]
    fn record_played_keeps_legacy_no_anchor_clock_moving() {
        let clock = AudioClock::new(48_000, 2);

        clock.record_played(9_600);

        assert_eq!(clock.smoothed_played_samples(), 9_600);
    }

    #[test]
    fn chains_early_backend_timestamp_after_previous_output_end() {
        let previous_anchor = PlaybackAnchor {
            playback_ns: 100,
            start_samples: 0,
            end_samples: 4_800,
            output_end_ns: 150,
        };

        let playback_ns = chain_reported_playback_ns(120, Some(previous_anchor));

        assert_eq!(playback_ns, 150);
    }

    #[test]
    fn keeps_backend_timestamp_when_it_is_after_previous_output_end() {
        let previous_anchor = PlaybackAnchor {
            playback_ns: 100,
            start_samples: 0,
            end_samples: 4_800,
            output_end_ns: 150,
        };

        let playback_ns = chain_reported_playback_ns(180, Some(previous_anchor));

        assert_eq!(playback_ns, 180);
    }
}
