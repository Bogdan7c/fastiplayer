//! Тестовая переменная процесса: малый потолок удерживаемых байтов полки.
//!
//! Сквозной тест «плеер сам оборвал связь по ошибке потока» (04d, решение 132) должен
//! честно упереться в потолок полки (`IntakeLimitExceeded`: страница игнорирует «полон»).
//! Настоящий потолок — `[browser_session] max_held_mb` (по умолчанию 320 МиБ); слать их в тесте незачем. Переменная меняет только
//! число потолка, правила остаются настоящими. По образцу
//! `FASTIPLAYER_TEST_BROWSER_DRAIN_STALL_MS` (`drain_override.rs`): пользователю не
//! нужна и не документируется. Без переменной поведение не меняется.

use std::ffi::OsStr;
use std::num::NonZeroUsize;

use pushed_media_source::PushedSourcePolicy;
use tracing::{info, warn};

/// Переменная окружения ТОЛЬКО для тестов: потолок удерживаемых байтов потока.
pub(crate) const TEST_BROWSER_MAX_HELD_STREAM_BYTES_VARIABLE: &str =
    "FASTIPLAYER_TEST_BROWSER_MAX_HELD_STREAM_BYTES";

/// Значение переменной не число байтов больше нуля.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{TEST_BROWSER_MAX_HELD_STREAM_BYTES_VARIABLE} must be a positive number of bytes")]
pub(crate) struct InvalidHeldBytesOverride;

/// Чистый разбор значения (`None` — переменной нет → потолок не меняется).
pub(crate) fn held_bytes_override(
    value: Option<&OsStr>,
) -> Result<Option<NonZeroUsize>, InvalidHeldBytesOverride> {
    let Some(text) = value else {
        return Ok(None);
    };
    text.to_str()
        .and_then(|text| text.parse::<NonZeroUsize>().ok())
        .map(Some)
        .ok_or(InvalidHeldBytesOverride)
}

/// Потолок из тестовой переменной окружения, если она задана и верна (`None` — потолок
/// из config-а). Незнакомое значение не ломает запуск: предупреждение и `None`.
pub(crate) fn held_bytes_override_from_environment() -> Option<NonZeroUsize> {
    let raw_value = std::env::var_os(TEST_BROWSER_MAX_HELD_STREAM_BYTES_VARIABLE);
    match held_bytes_override(raw_value.as_deref()) {
        Ok(ceiling) => ceiling,
        Err(error) => {
            warn!(%error, "Ignoring the test held stream bytes override");
            None
        }
    }
}

/// Те же пределы, но с потолком из тестовой переменной, если она есть. Незнакомое
/// значение не ломает запуск: настоящий потолок и предупреждение.
pub(crate) fn policy_with_held_bytes_from_environment(
    policy: PushedSourcePolicy,
) -> PushedSourcePolicy {
    let raw_value = std::env::var_os(TEST_BROWSER_MAX_HELD_STREAM_BYTES_VARIABLE);
    policy_with_held_bytes_override(policy, raw_value.as_deref())
}

/// То же для уже прочитанного значения переменной (отдельно ради тестов).
fn policy_with_held_bytes_override(
    policy: PushedSourcePolicy,
    raw_value: Option<&OsStr>,
) -> PushedSourcePolicy {
    match held_bytes_override(raw_value) {
        Ok(None) => policy,
        Ok(Some(ceiling)) => {
            info!(
                variable = TEST_BROWSER_MAX_HELD_STREAM_BYTES_VARIABLE,
                ceiling_bytes = ceiling.get(),
                "Held stream bytes ceiling lowered (test-only override)"
            );
            policy.with_max_held_stream_bytes(ceiling)
        }
        Err(error) => {
            warn!(%error, "Ignoring the test held stream bytes override");
            policy
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Порог «полон» зависит от потолка: по нему видно, какой потолок остался.
    fn full_at(policy: PushedSourcePolicy) -> usize {
        policy.buffer_full_at_held_bytes()
    }

    #[test]
    fn without_the_variable_the_real_ceiling_stays() {
        let real = PushedSourcePolicy::recommended().expect("policy");
        assert_eq!(held_bytes_override(None), Ok(None));
        assert_eq!(
            full_at(policy_with_held_bytes_override(real, None)),
            full_at(real)
        );
    }

    #[test]
    fn a_number_of_bytes_lowers_the_ceiling() {
        let real = PushedSourcePolicy::recommended().expect("policy");
        let lowered = policy_with_held_bytes_override(real, Some(OsStr::new("28000")));
        assert_eq!(
            full_at(lowered),
            full_at(real.with_max_held_stream_bytes(NonZeroUsize::new(28_000).expect("non-zero")))
        );
        assert!(full_at(lowered) < full_at(real));
    }

    #[test]
    fn garbage_keeps_the_real_ceiling_instead_of_panicking() {
        let real = PushedSourcePolicy::recommended().expect("policy");
        let kept = policy_with_held_bytes_override(real, Some(OsStr::new("lots")));
        assert_eq!(full_at(kept), full_at(real));
    }

    #[test]
    fn zero_and_garbage_are_reported_not_guessed() {
        for value in ["0", "", "1.5", "-3", "lots"] {
            assert_eq!(
                held_bytes_override(Some(OsStr::new(value))),
                Err(InvalidHeldBytesOverride),
                "{value:?}"
            );
        }
    }
}
