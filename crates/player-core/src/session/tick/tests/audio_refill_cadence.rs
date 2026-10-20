//! Момент audio refill wakeup-а по ритму callback-а (cpu-parity 02b).
//!
//! Уровень буфера падает ступеньками в callback-е. Тесты закрепляют два обещания:
//! worker не просыпается раньше ступеньки, на которой уровень дойдёт до high-water
//! (иначе пробуждение пустое), и не просыпается настолько поздно, чтобы съесть запас
//! буфера. Без ритма (или с устаревшим ритмом) действует прежняя формула.

use audio_core::AudioOutputCallbackCadence;

use crate::session::tick::audio_refill_cadence::audio_refill_delay_above_high_water;

use super::*;

/// High-water по умолчанию (`audio.buffer_target_ms`).
const HIGH_WATER: Duration = Duration::from_millis(200);

/// Ритм «последний callback был `since_ms` назад, период `period_ms`».
fn observed(since_ms: f64, period_ms: f64) -> AudioOutputCallbackCadence {
    AudioOutputCallbackCadence::Observed {
        since_last_callback: Duration::from_secs_f64(since_ms / 1000.0),
        callback_period: Duration::from_secs_f64(period_ms / 1000.0),
    }
}

/// Задержка для превышения `excess_ms` над high-water.
fn delay_for(excess_ms: f64, cadence: AudioOutputCallbackCadence) -> Duration {
    audio_refill_delay_above_high_water(
        Duration::from_secs_f64(excess_ms / 1000.0),
        HIGH_WATER,
        cadence,
    )
}

#[test]
fn unknown_cadence_keeps_continuous_drain_formula() {
    let delay = delay_for(10.0, AudioOutputCallbackCadence::NotObserved);

    assert_eq!(delay, Duration::from_millis(10));
}

#[test]
fn wakes_just_after_next_callback_instead_of_before_it() {
    // Превышение 10 мс, а callback забирает 40 мс раз в 40 мс; последний был 5 мс назад.
    // Прежняя формула будила через 10 мс — за 25 мс до ступеньки, уровень тот же.
    let delay = delay_for(10.0, observed(5.0, 40.0));

    // Следующий callback через 35 мс плюс 1/8 периода запаса на дрожание.
    assert_eq!(delay, Duration::from_millis(40));
}

#[test]
fn large_excess_waits_for_as_many_callbacks_as_needed() {
    // 50 мс превышения при периоде 20 мс: нужны 3 callback-а (60 мс снимают 50).
    let delay = delay_for(50.0, observed(5.0, 20.0));

    assert_eq!(delay, Duration::from_micros(57_500));
}

#[test]
fn excess_equal_to_whole_periods_does_not_wait_an_extra_callback() {
    // Ровно два периода превышения: после второго callback-а уровень = high-water.
    let delay = delay_for(40.0, observed(0.0, 20.0));

    assert_eq!(delay, Duration::from_micros(42_500));
}

#[test]
fn overdue_callback_is_treated_as_imminent() {
    // Callback опаздывает на 5 мс: будим через запас на дрожание, а не «через ноль».
    let one_callback_needed = delay_for(10.0, observed(25.0, 20.0));
    // Если нужно два callback-а, остаётся один полный период после опоздавшего.
    let two_callbacks_needed = delay_for(30.0, observed(25.0, 20.0));

    assert_eq!(one_callback_needed, Duration::from_micros(2_500));
    assert_eq!(two_callbacks_needed, Duration::from_micros(22_500));
}

#[test]
fn stale_cadence_falls_back_to_continuous_formula() {
    // Больше двух периодов без callback-а: пауза/удержанный output/зависшее устройство.
    let delay = delay_for(10.0, observed(41.0, 20.0));

    assert_eq!(delay, Duration::from_millis(10));
}

#[test]
fn callback_period_above_half_high_water_is_not_trusted() {
    let delay = delay_for(10.0, observed(5.0, 101.0));

    assert_eq!(delay, Duration::from_millis(10));
}

#[test]
fn zero_callback_period_is_not_trusted() {
    let delay = delay_for(10.0, observed(0.0, 0.0));

    assert_eq!(delay, Duration::from_millis(10));
}

/// Перебор превышений, периодов и фаз callback-а: оба обещания держатся везде.
#[test]
fn cadence_wakeup_is_never_empty_and_never_eats_the_buffer() {
    let periods_ms = [2.0, 5.3, 10.0, 21.333, 42.7, 85.3, 100.0];
    for period_ms in periods_ms {
        let period = Duration::from_secs_f64(period_ms / 1000.0);
        for excess_tenths in 1..=1_200_u32 {
            let excess_ms = f64::from(excess_tenths) / 10.0;
            for phase_step in 0..=8_u32 {
                // Фаза от «callback только что был» до «опаздывает на период».
                let since_ms = period_ms * f64::from(phase_step) / 4.0;
                let delay = delay_for(excess_ms, observed(since_ms, period_ms));

                // Сколько callback-ов успеет пройти к пробуждению, если опоздавший придёт сразу.
                let since = Duration::from_secs_f64(since_ms / 1000.0);
                let callbacks_by_wakeup = if since >= period {
                    1 + (delay.as_nanos() / period.as_nanos())
                } else {
                    (delay + since).as_nanos() / period.as_nanos()
                };
                let drained_ms = callbacks_by_wakeup as f64 * period_ms;
                assert!(
                    drained_ms + 1e-6 >= excess_ms,
                    "empty wakeup: excess {excess_ms} ms, period {period_ms} ms, \
                     since {since_ms} ms -> {delay:?} drains only {drained_ms} ms"
                );

                // Опоздание относительно непрерывной формулы — не больше периода с запасом,
                // то есть к пробуждению в буфере остаётся больше 40 % high-water.
                let latest_allowed =
                    Duration::from_secs_f64(excess_ms / 1000.0) + period.mul_f64(1.125);
                assert!(
                    delay <= latest_allowed,
                    "late wakeup: excess {excess_ms} ms, period {period_ms} ms, \
                     since {since_ms} ms -> {delay:?} > {latest_allowed:?}"
                );
            }
        }
    }
}
