//! Тестовая переменная процесса: короткий срок застоя доигрывания.
//!
//! Сквозной тест «вкладка ушла, окно на паузе ждёт человека» (04b) должен показать, что
//! процесс жив ДОЛЬШЕ срока застоя. Настоящий срок — 15 с ([`DrainPolicy::recommended`]);
//! ждать его в тесте — долго. Переменная укорачивает только срок застоя, правила
//! остаются настоящими. По образцу `FASTIPLAYER_TEST_BROWSER_VIDEO` (`video_mode.rs`):
//! пользователю не нужна и не документируется в справке.

use std::ffi::OsStr;
use std::time::Duration;

use tracing::{info, warn};

use super::session_media::{DrainPolicy, PausedDrain};

/// Переменная окружения ТОЛЬКО для тестов: срок застоя доигрывания в миллисекундах.
pub(crate) const TEST_BROWSER_DRAIN_STALL_VARIABLE: &str =
    "FASTIPLAYER_TEST_BROWSER_DRAIN_STALL_MS";

/// Значение переменной не число миллисекунд больше нуля.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{TEST_BROWSER_DRAIN_STALL_VARIABLE} must be a positive number of milliseconds")]
pub(crate) struct InvalidStallOverride;

/// Чистый разбор значения (`None` — переменной нет → срок не меняется).
pub(crate) fn stall_timeout_override(
    value: Option<&OsStr>,
) -> Result<Option<Duration>, InvalidStallOverride> {
    let Some(text) = value else {
        return Ok(None);
    };
    let milliseconds: u64 = text
        .to_str()
        .and_then(|text| text.parse().ok())
        .ok_or(InvalidStallOverride)?;
    if milliseconds == 0 {
        return Err(InvalidStallOverride);
    }
    Ok(Some(Duration::from_millis(milliseconds)))
}

/// Политика доигрывания процесса: правило паузы — от вида процесса, срок застоя — из
/// тестовой переменной, если она есть. Незнакомое значение не ломает запуск: настоящий
/// срок и предупреждение.
pub(crate) fn drain_policy_from_environment(paused: PausedDrain) -> DrainPolicy {
    let policy = DrainPolicy::recommended(paused);
    let raw_value = std::env::var_os(TEST_BROWSER_DRAIN_STALL_VARIABLE);
    match stall_timeout_override(raw_value.as_deref()) {
        Ok(None) => policy,
        Ok(Some(stall_timeout)) => {
            info!(
                variable = TEST_BROWSER_DRAIN_STALL_VARIABLE,
                stall_timeout_ms = u64::try_from(stall_timeout.as_millis()).unwrap_or(u64::MAX),
                "Playback drain stall timeout shortened (test-only override)"
            );
            policy.with_stall_timeout(stall_timeout)
        }
        Err(error) => {
            warn!(%error, "Ignoring the test drain stall override");
            policy
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_the_variable_the_real_stall_timeout_stays() {
        assert_eq!(stall_timeout_override(None), Ok(None));
    }

    #[test]
    fn milliseconds_shorten_the_stall_timeout() {
        assert_eq!(
            stall_timeout_override(Some(OsStr::new("1200"))),
            Ok(Some(Duration::from_millis(1200)))
        );
    }

    #[test]
    fn zero_and_garbage_are_reported_not_guessed() {
        for value in ["0", "", "1.5", "-3", "fast"] {
            assert_eq!(
                stall_timeout_override(Some(OsStr::new(value))),
                Err(InvalidStallOverride),
                "{value:?}"
            );
        }
    }
}
