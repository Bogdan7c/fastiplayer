//! Передача звука со старого output-а на новый без рассинхрона A/V (UX edge cases 2, C3).
//!
//! # Корень проблемы
//!
//! Audio clock — это «сколько PCM уже проиграно»: позиция clock-а растёт только от
//! настоящих samples, тишина при underrun её не двигает. Media-время — это
//! `якорь + позиция clock-а`. К моменту пересоздания output-а в кольце старого выхода
//! лежит ещё не проигранный PCM (`submitted_output_tail`, около 200 мс), и decoder уже
//! ушёл дальше этого хвоста: следующий PCM, который session запишет в **новый** output,
//! принадлежит media-времени `T + хвост`.
//!
//! Старый код привязывал clock нового output-а к `T` (позиция слышимого сейчас звука),
//! а новый ring оставался пустым. Тогда следующий записанный PCM (`T + хвост`) проигрывался
//! при clock-е `T`: звук опережал видео на длину хвоста до ближайшей перемотки.
//!
//! # Решение
//!
//! Хвост старого output-а нельзя забрать (consumer кольца живёт внутри callback-а потока,
//! который к тому же мог умереть), поэтому его место в новом output-е занимает **тишина
//! той же длительности**. Тогда:
//! - clock нового output-а остаётся привязанным к `T` и семантику clock-а менять не нужно;
//! - первый настоящий PCM встаёт в clock-позицию `хвост`, то есть ровно в `T + хвост`;
//! - видео не прыгает и не ждёт, пользователь слышит короткую паузу вместо рассинхрона;
//! - уровень буфера и граница EOF-drain сохраняют прежнюю длительность хвоста.

use std::time::Duration;

use audio_core::{AudioOutputSpec, AudioOutputWriteIntent, PlayerAudioOutput};

use crate::{PlayerError, PlayerErrorKind, PlayerResult};

use super::PlayerSession;

/// Наносекунд в секунде для перевода длительности хвоста в кадры.
const NANOS_PER_SECOND: u128 = 1_000_000_000;

impl PlayerSession {
    /// Возвращает длительность PCM, который старый output уже принял, но ещё не проиграл.
    ///
    /// Нет clock-а (output-а не было) — хвоста нет: `Duration::ZERO`.
    #[must_use]
    pub(super) fn audio_output_unplayed_tail(&self) -> Duration {
        self.pipeline
            .audio_output_clock_timing()
            .submitted_output_tail()
    }
}

/// Сколько целых кадров тишины соответствует `tail` при `sample_rate`.
///
/// Округление к ближайшему кадру: ошибка не больше полукадра (десятки микросекунд).
#[must_use]
fn silence_frames_for_tail(tail: Duration, sample_rate: u32) -> usize {
    let scaled = tail.as_nanos() * u128::from(sample_rate) + NANOS_PER_SECOND / 2;
    usize::try_from(scaled / NANOS_PER_SECOND).unwrap_or(usize::MAX)
}

/// Записывает в ещё не запущенный `output` тишину длиной `tail`.
///
/// Возвращает число записанных кадров (0 — хвоста не было, в output ничего не писалось).
/// Ошибка или неполная запись означает, что новый output непригоден: вызывающий код
/// отказывается от замены и оставляет прежний output нетронутым.
pub(super) fn prefill_replacement_output_with_silence(
    output: &mut dyn PlayerAudioOutput,
    output_spec: AudioOutputSpec,
    tail: Duration,
) -> PlayerResult<usize> {
    let frames = silence_frames_for_tail(tail, output_spec.sample_rate);
    if frames == 0 {
        return Ok(0);
    }
    let Some(sample_count) = frames.checked_mul(output_spec.channels() as usize) else {
        return Err(handover_error("tail length overflows sample count"));
    };

    let silence = vec![0.0_f32; sample_count];
    let report = output
        .write_samples(&silence, AudioOutputWriteIntent::DirectDecodedPcm)
        .map_err(|error| handover_error(&format!("silence write failed: {error}")))?;
    if !report.is_complete() {
        return Err(handover_error(
            "replacement output accepted only part of silence",
        ));
    }
    Ok(frames)
}

/// Ошибка передачи звука: тот же вид, что у остальных отказов нового output-а.
fn handover_error(detail: &str) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::AudioDeviceUnavailable,
        format!("Audio output handover failed: {detail}"),
    )
}
