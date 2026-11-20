//! Тестовая переменная процесса: короткий шаг сводки журнала плеера.
//!
//! Стенд моста (`browser-bridge-bench`) узнаёт «кадр дошёл до рендера» из строки
//! `Browser playback summary` журнала. Настоящий шаг сводки — 10 с
//! ([`BROWSER_PLAYBACK_SUMMARY_INTERVAL`]), и строки стенда длились 11, 31 и 51 с — шагами
//! сводки (решение 386, сессия 16b3c2). Переменная укорачивает только шаг сводки: что
//! пишется и когда сеанс кончается — без изменений. По образцу
//! `FASTIPLAYER_TEST_BROWSER_DRAIN_STALL_MS` (`drain_override.rs`): пользователю не нужна
//! и в справке не документируется.

use std::ffi::OsStr;
use std::time::Duration;

use tracing::{info, warn};

use super::journal::BROWSER_PLAYBACK_SUMMARY_INTERVAL;
use super::outbound::CLOCK_INTERVAL;

/// Переменная окружения ТОЛЬКО для тестов: шаг сводки журнала в миллисекундах.
pub(crate) const TEST_BROWSER_SUMMARY_INTERVAL_VARIABLE: &str =
    "FASTIPLAYER_TEST_BROWSER_SUMMARY_INTERVAL_MS";

/// Самый короткий допустимый шаг. Сводка пишется во время `pump` главного цикла, а цикл
/// без внешнего повода просыпается не чаще тика часов (`wake_schedule.rs`): шаг короче
/// тика выглядел бы честным, но строки шли бы всё равно раз в тик.
const SHORTEST_SUMMARY_INTERVAL: Duration = CLOCK_INTERVAL;

/// Значение переменной не число миллисекунд в допустимых пределах.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{TEST_BROWSER_SUMMARY_INTERVAL_VARIABLE} must be a whole number of milliseconds \
     from {shortest_ms} to {longest_ms}",
    shortest_ms = SHORTEST_SUMMARY_INTERVAL.as_millis(),
    longest_ms = BROWSER_PLAYBACK_SUMMARY_INTERVAL.as_millis()
)]
pub(crate) struct InvalidSummaryIntervalOverride;

/// Чистый разбор значения (`None` — переменной нет → шаг не меняется).
///
/// Переменная только укорачивает: шаг длиннее настоящего тесту не нужен, а сторожа и
/// подсказки журнала рассчитаны на настоящий.
pub(crate) fn summary_interval_override(
    value: Option<&OsStr>,
) -> Result<Option<Duration>, InvalidSummaryIntervalOverride> {
    let Some(text) = value else {
        return Ok(None);
    };
    let milliseconds: u64 = text
        .to_str()
        .and_then(|text| text.parse().ok())
        .ok_or(InvalidSummaryIntervalOverride)?;
    let interval = Duration::from_millis(milliseconds);
    if interval < SHORTEST_SUMMARY_INTERVAL || interval > BROWSER_PLAYBACK_SUMMARY_INTERVAL {
        return Err(InvalidSummaryIntervalOverride);
    }
    Ok(Some(interval))
}

/// Шаг сводки журнала этого процесса: настоящий или из тестовой переменной.
///
/// Одна точка решения для обоих путей — безоконного (`playback`) и окна (`window`), чтобы
/// они не разошлись. Незнакомое значение не ломает запуск: настоящий шаг и предупреждение
/// (само значение в журнал не пишется — его прислал не человек).
pub(crate) fn summary_interval_from_environment() -> Duration {
    let raw_value = std::env::var_os(TEST_BROWSER_SUMMARY_INTERVAL_VARIABLE);
    match summary_interval_override(raw_value.as_deref()) {
        Ok(None) => BROWSER_PLAYBACK_SUMMARY_INTERVAL,
        Ok(Some(interval)) => {
            info!(
                variable = TEST_BROWSER_SUMMARY_INTERVAL_VARIABLE,
                summary_interval_ms = u64::try_from(interval.as_millis()).unwrap_or(u64::MAX),
                "Browser playback summary interval shortened (test-only override)"
            );
            interval
        }
        Err(error) => {
            warn!(%error, "Ignoring the test summary interval override");
            BROWSER_PLAYBACK_SUMMARY_INTERVAL
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_the_variable_the_real_interval_stays() {
        assert_eq!(summary_interval_override(None), Ok(None));
    }

    #[test]
    fn milliseconds_shorten_the_interval() {
        assert_eq!(
            summary_interval_override(Some(OsStr::new("1000"))),
            Ok(Some(Duration::from_secs(1)))
        );
    }

    #[test]
    fn the_bounds_themselves_are_accepted() {
        assert_eq!(
            summary_interval_override(Some(OsStr::new("250"))),
            Ok(Some(CLOCK_INTERVAL))
        );
        assert_eq!(
            summary_interval_override(Some(OsStr::new("10000"))),
            Ok(Some(BROWSER_PLAYBACK_SUMMARY_INTERVAL))
        );
    }

    #[test]
    fn shorter_than_a_clock_tick_or_longer_than_real_is_refused() {
        for value in ["249", "1", "0", "10001", "60000"] {
            assert_eq!(
                summary_interval_override(Some(OsStr::new(value))),
                Err(InvalidSummaryIntervalOverride),
                "{value:?}"
            );
        }
    }

    #[test]
    fn garbage_is_reported_not_guessed() {
        for value in ["", "1.5", "-3", "fast", "1s"] {
            assert_eq!(
                summary_interval_override(Some(OsStr::new(value))),
                Err(InvalidSummaryIntervalOverride),
                "{value:?}"
            );
        }
    }
}
