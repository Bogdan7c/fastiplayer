//! Audio clock — source of truth для A/V синхронизации.
//!
//! Отслеживает время воспроизведения на основе количества сэмплов,
//! отправленных в audio output. Использует AtomicU64 для thread-safe
//! доступа из CPAL callback потока.
//!
//! Формула: current_time = samples_played / (sample_rate * channels)

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Clock для audio playback.
///
/// Два счётчика:
/// - samples_written: сколько сэмплов записано в ring buffer (из main thread)
/// - samples_played: сколько interleaved сэмплов прочитано из ring buffer (из CPAL callback)
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

    /// Флаг: есть валидный CPAL playback anchor.
    playback_anchor_valid: AtomicBool,

    /// Seqlock-счётчик для атомарного чтения нескольких playback anchor полей.
    playback_anchor_generation: AtomicU64,

    /// `std::time::Instant` предполагаемого playback, в наносекундах от `origin`.
    playback_anchor_ns: AtomicU64,

    /// Media sample index первого сэмпла текущего CPAL callback.
    playback_anchor_samples: AtomicU64,

    /// Media sample index конца реально заполненной audio-части callback.
    playback_anchor_end_samples: AtomicU64,

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
            playback_anchor_valid: AtomicBool::new(false),
            playback_anchor_generation: AtomicU64::new(0),
            playback_anchor_ns: AtomicU64::new(0),
            playback_anchor_samples: AtomicU64::new(0),
            playback_anchor_end_samples: AtomicU64::new(0),
            underrun_callbacks: AtomicU64::new(0),
            underrun_samples: AtomicU64::new(0),
        }
    }

    /// Возвращает текущее время воспроизведения.
    ///
    /// Основано на samples_played — реально "сыгранных" сэмплах,
    /// а не записанных. Это даёт точное время для A/V sync.
    ///
    /// samples_played = total interleaved samples (frames * channels).
    /// time = samples_played / (sample_rate * channels).
    pub fn now(&self) -> Duration {
        self.samples_to_duration(self.smoothed_played_samples())
    }

    /// Возвращает плавную оценку media sample index, который сейчас слышит пользователь.
    fn smoothed_played_samples(&self) -> u64 {
        let samples_per_sec = self.samples_per_second();
        if samples_per_sec == 0 {
            return 0;
        }

        if !self.playback_anchor_valid.load(Ordering::Acquire) {
            return self.samples_played.load(Ordering::Relaxed);
        }

        let generation_before = self.playback_anchor_generation.load(Ordering::Acquire);
        if generation_before % 2 != 0 {
            return self.samples_played.load(Ordering::Relaxed);
        }

        let anchor_ns = self.playback_anchor_ns.load(Ordering::Relaxed);
        let anchor_samples = self.playback_anchor_samples.load(Ordering::Relaxed);
        let anchor_end_samples = self.playback_anchor_end_samples.load(Ordering::Relaxed);
        let generation_after = self.playback_anchor_generation.load(Ordering::Acquire);
        if generation_before != generation_after {
            return self.samples_played.load(Ordering::Relaxed);
        }

        let current_ns = self.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let elapsed_ns = current_ns.saturating_sub(anchor_ns);
        let progressed_samples =
            ((elapsed_ns as u128 * samples_per_sec as u128) / 1_000_000_000u128) as u64;

        anchor_samples
            .saturating_add(progressed_samples)
            .min(anchor_end_samples)
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
        self.samples_played.fetch_add(samples, Ordering::Relaxed);
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
        let playback_ns = playback_at
            .saturating_duration_since(self.origin)
            .as_nanos()
            .min(u64::MAX as u128) as u64;

        self.playback_anchor_generation
            .fetch_add(1, Ordering::AcqRel);
        self.playback_anchor_samples
            .store(samples_before_callback, Ordering::Relaxed);
        self.playback_anchor_end_samples
            .store(samples_after_callback, Ordering::Relaxed);
        self.playback_anchor_ns
            .store(playback_ns, Ordering::Relaxed);
        self.playback_anchor_valid.store(true, Ordering::Relaxed);
        self.playback_anchor_generation
            .fetch_add(1, Ordering::Release);
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
        self.playback_anchor_valid.store(false, Ordering::Release);
        self.playback_anchor_generation.store(0, Ordering::Relaxed);
        self.playback_anchor_ns.store(0, Ordering::Relaxed);
        self.playback_anchor_samples.store(0, Ordering::Relaxed);
        self.playback_anchor_end_samples.store(0, Ordering::Relaxed);
        self.underrun_callbacks.store(0, Ordering::Relaxed);
        self.underrun_samples.store(0, Ordering::Relaxed);
    }
}
