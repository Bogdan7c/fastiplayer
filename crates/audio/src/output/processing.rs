//! Преобразование samples и output protection после channel mixing/resampling.
//!
//! Direct PCM остаётся прозрачным, а limiter/soft-clip применяются только
//! к intent-ам, которые уже использовали эту policy до decomposition.

use cpal::{FromSample, Sample};

use super::AudioOutputWriteIntent;

/// Нормализует decoder sample перед CPAL conversion.
pub(super) fn normalize_decoder_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Конвертирует внутренний f32 sample в concrete CPAL stream sample.
pub(super) fn convert_sample_for_output<T>(sample: f32) -> T
where
    T: Sample + FromSample<f32>,
{
    T::from_sample(normalize_decoder_sample(sample))
}

/// Потолок пик-лимитера: выше него огибающая давится к потолку.
pub(super) const PEAK_LIMITER_CEILING: f32 = 0.98;

/// Release пик-лимитера в секундах: восстановление gain после пика.
const PEAK_LIMITER_RELEASE_SECS: f32 = 0.150;

/// Считает per-frame затухание огибающей лимитера для stream rate.
pub(super) fn limiter_release_decay_for_rate(sample_rate: u32) -> f32 {
    if sample_rate == 0 {
        return 0.0;
    }
    (-1.0 / (PEAK_LIMITER_RELEASE_SECS * sample_rate as f32)).exp()
}

/// Продвигает огибающую лимитера: мгновенная атака, экспоненциальный release.
pub(super) fn advance_limiter_envelope(envelope: f32, frame_peak: f32, release_decay: f32) -> f32 {
    frame_peak.max(envelope * release_decay)
}

/// Возвращает gain, удерживающий огибающую под потолком лимитера.
///
/// Timestretch gain-компенсация разгоняет пики громкого материала до ~2x над
/// full scale; обычное насыщение/клиппинг на таких уровнях — слышимый треск.
/// Плавный gain с release вместо этого коротко приглушает пик.
pub(super) fn limiter_gain_for_envelope(envelope: f32) -> f32 {
    if envelope <= PEAK_LIMITER_CEILING {
        return 1.0;
    }
    PEAK_LIMITER_CEILING / envelope
}

/// Мягко ограничивает sample до (-1.0, 1.0) без жёстких клиппинг-фронтов.
///
/// Сигнал до колена 0.95 проходит линейно; выше — плавно насыщается к 1.0
/// через tanh. Жёсткий clamp дал бы те же щелчки, что и клиппинг на device.
/// Работает как последний рубеж после пик-лимитера.
pub(super) fn soft_clip_sample(sample: f32) -> f32 {
    const KNEE: f32 = 0.95;

    if !sample.is_finite() {
        return 0.0;
    }

    let magnitude = sample.abs();
    if magnitude <= KNEE {
        return sample;
    }

    let headroom = 1.0 - KNEE;
    let compressed = KNEE + headroom * ((magnitude - KNEE) / headroom).tanh();
    compressed.copysign(sample)
}

/// Применяет защиту только к samples, произведённым tempo processor-ом.
pub(super) fn output_sample_for_intent(
    sample: f32,
    volume: f32,
    limiter_gain: f32,
    intent: AudioOutputWriteIntent,
) -> f32 {
    let volume_adjusted_sample = sample * volume;
    match intent {
        AudioOutputWriteIntent::DirectDecodedPcm => volume_adjusted_sample,
        AudioOutputWriteIntent::TempoProcessed => {
            soft_clip_sample(volume_adjusted_sample * limiter_gain)
        }
    }
}

/// Обнуляет history-dependent protection state на lifecycle boundary.
pub(super) fn reset_output_protection(limiter_envelope: &mut f32) {
    *limiter_envelope = 0.0;
}
