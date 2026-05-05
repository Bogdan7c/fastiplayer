//! Audio clock — source of truth для A/V синхронизации.
//!
//! Отслеживает время воспроизведения на основе количества сэмплов,
//! отправленных в audio output. Использует AtomicU64 для thread-safe
//! доступа из CPAL callback потока.
//!
//! Формула: current_time = samples_played / (sample_rate * channels)

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Clock для audio playback.
///
/// Два счётчика:
/// - samples_written: сколько сэмплов записано в ring buffer (из main thread)
/// - samples_played: сколько сэмплов прочитано из ring buffer (из CPAL callback)
///
/// Разница между ними = уровень заполнения buffer.
pub struct AudioClock {
    /// Sample rate в Гц (например, 48000).
    sample_rate: u32,

    /// Количество каналов (1 = mono, 2 = stereo).
    channels: u32,

    /// Общее количество сэмплов (per-channel), записанных в buffer.
    samples_written: AtomicU64,

    /// Общее количество сэмплов (per-channel), прочитанных из buffer.
    samples_played: AtomicU64,
}

impl AudioClock {
    /// Создаёт новый clock с заданной частотой дискретизации.
    pub fn new(sample_rate: u32, channels: u32) -> Self {
        Self {
            sample_rate,
            channels,
            samples_written: AtomicU64::new(0),
            samples_played: AtomicU64::new(0),
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
        let played = self.samples_played.load(Ordering::Relaxed);
        let samples_per_sec = (self.sample_rate as u64) * (self.channels as u64);
        if samples_per_sec == 0 {
            return Duration::ZERO;
        }
        let secs = played / samples_per_sec;
        let remainder = played % samples_per_sec;
        // Конвертируем remainder в микросекунды:
        // remainder / samples_per_sec * 1_000_000
        let micros = (remainder * 1_000_000) / samples_per_sec;
        Duration::from_secs(secs) + Duration::from_micros(micros)
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

    /// Сбрасывает clock в начальное состояние.
    pub fn reset(&self) {
        self.samples_written.store(0, Ordering::Relaxed);
        self.samples_played.store(0, Ordering::Relaxed);
    }
}
