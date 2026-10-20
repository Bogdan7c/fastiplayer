//! Момент пробуждения для audio refill по ритму callback-а звуковой карты.
//!
//! Правило подпитки не меняется: worker доливает PCM, когда уровень буфера опустился
//! до high-water или ниже (`audio_runtime::process_pending_audio_packets_with_buffer_limit`).
//! Здесь решается только, **когда** проснуться, чтобы застать этот момент.
//!
//! Уровень буфера падает не плавно, а ступеньками: только когда output callback забирает
//! очередной период (размер выбирает PipeWire/ALSA). Прежняя формула
//! `уровень − high-water` считала, что звук утекает непрерывно, и будила worker раньше
//! следующей ступеньки: он видел тот же уровень, работы не было, и он снова засыпал на
//! такой же короткий срок — ~4 пустых пробуждения на один полезный долив (cpu-parity 02b).
//!
//! Если output сообщает ритм callback-а, будим сразу после того callback-а, на котором
//! уровень опустится до high-water. Если ритм неизвестен, устарел (callback-и не идут)
//! или период слишком велик для гарантии запаса, остаётся прежняя формула.

use std::time::Duration;

use audio_core::AudioOutputCallbackCadence;

/// После скольких периодов без callback-а ритм считается устаревшим.
///
/// Один пропущенный период — обычное дрожание планировщика ОС; два подряд означают,
/// что callback-и не идут (пауза, удержанный output, зависшее устройство) и предсказывать
/// следующую ступеньку не по чему.
const STALE_CADENCE_PERIODS: u32 = 2;

/// Наибольший период callback-а как доля high-water, при котором ритму можно доверять.
///
/// Пробуждение по ритму может оказаться позже прежней формулы почти на период и восьмую
/// (см. `CALLBACK_JITTER_SLACK_DIVISOR`). С периодом не больше половины high-water в буфере
/// к пробуждению остаётся не меньше ~44 % high-water. Больший период — экзотика, там
/// надёжнее прежняя формула.
const MAX_CALLBACK_PERIOD_HIGH_WATER_FRACTION: f64 = 0.5;

/// Запас на дрожание callback-а: пробуждение на 1/8 периода позже ожидаемого callback-а.
///
/// Проснувшись чуть раньше callback-а, worker увидел бы старый уровень и потратил бы
/// пробуждение впустую; восьмая часть периода (~2,7 мс при 1024 кадрах на 48 кГц)
/// перекрывает обычное дрожание и ничтожна рядом с запасом буфера.
const CALLBACK_JITTER_SLACK_DIVISOR: u32 = 8;

/// Возвращает задержку до audio refill, когда уровень выше high-water.
///
/// `excess_over_high_water` — насколько наблюдённый уровень выше high-water (> 0);
/// `high_water` — сам порог. Результат всегда конечен: при уровне выше порога у worker-а
/// обязан быть дедлайн подпитки (инвариант бэклога AC-3 #19).
#[must_use]
pub(super) fn audio_refill_delay_above_high_water(
    excess_over_high_water: Duration,
    high_water: Duration,
    callback_cadence: AudioOutputCallbackCadence,
) -> Duration {
    match trusted_callback_rhythm(high_water, callback_cadence) {
        Some(rhythm) => rhythm.delay_until_level_reaches_high_water(excess_over_high_water),
        // Прежняя формула: звук будто утекает непрерывно.
        None => excess_over_high_water,
    }
}

/// Ритм callback-а, которому можно доверить предсказание следующей ступеньки уровня.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TrustedCallbackRhythm {
    /// Сколько прошло с начала последнего callback-а (не больше двух периодов).
    since_last_callback: Duration,

    /// Сколько звука забирает один callback (ненулевой).
    callback_period: Duration,
}

/// Проверяет, можно ли планировать по ритму, иначе `None` (→ прежняя формула).
fn trusted_callback_rhythm(
    high_water: Duration,
    callback_cadence: AudioOutputCallbackCadence,
) -> Option<TrustedCallbackRhythm> {
    let AudioOutputCallbackCadence::Observed {
        since_last_callback,
        callback_period,
    } = callback_cadence
    else {
        return None;
    };

    if callback_period.is_zero()
        || callback_period > high_water.mul_f64(MAX_CALLBACK_PERIOD_HIGH_WATER_FRACTION)
        || since_last_callback > callback_period.saturating_mul(STALE_CADENCE_PERIODS)
    {
        return None;
    }

    Some(TrustedCallbackRhythm {
        since_last_callback,
        callback_period,
    })
}

impl TrustedCallbackRhythm {
    /// Задержка до callback-а, после которого уровень опустится до high-water.
    ///
    /// Каждый callback уменьшает уровень на период, поэтому нужно
    /// `k = ⌈превышение / период⌉` callback-ов. k-й из них наступит через
    /// `k × период − прошло_с_последнего`. Если ожидаемый callback уже опаздывает
    /// (прошло больше периода), считаем его неизбежным прямо сейчас: остаются
    /// `(k − 1)` полных периодов. К результату добавляется запас на дрожание.
    fn delay_until_level_reaches_high_water(self, excess_over_high_water: Duration) -> Duration {
        let callbacks_needed =
            callbacks_needed_to_drain(excess_over_high_water, self.callback_period);
        let until_last_needed_callback = self
            .callback_period
            .saturating_mul(callbacks_needed)
            .saturating_sub(self.since_last_callback);
        let overdue_callback_floor = self
            .callback_period
            .saturating_mul(callbacks_needed.saturating_sub(1));
        let jitter_slack = self.callback_period / CALLBACK_JITTER_SLACK_DIVISOR;

        until_last_needed_callback
            .max(overdue_callback_floor)
            .saturating_add(jitter_slack)
    }
}

/// Сколько callback-ов (не меньше одного) нужно, чтобы снять превышение над high-water.
fn callbacks_needed_to_drain(excess_over_high_water: Duration, callback_period: Duration) -> u32 {
    let callbacks_needed = excess_over_high_water
        .as_nanos()
        .div_ceil(callback_period.as_nanos())
        .max(1);
    // Реальные значения — единицы callback-ов; насыщение до u32::MAX — только защита
    // от абсурдных входов (например, периода в наносекунду).
    u32::try_from(callbacks_needed).unwrap_or(u32::MAX)
}
