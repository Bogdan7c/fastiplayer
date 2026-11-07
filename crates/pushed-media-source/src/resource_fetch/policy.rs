//! Пределы очереди запросов ресурсов. Значения даёт приложение: crate не знает
//! договора протокола, а только держит то, что ему сказали.

use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

/// Пределы очереди.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceFetchPolicy {
    /// Сколько запросов одновременно у внешней стороны (у моста — договор протокола).
    max_in_flight: NonZeroUsize,
    /// Сколько внешняя сторона может молчать по отправленному запросу.
    idle_timeout: Duration,
    /// Самый большой ресурс, который движок вправе попросить, байт.
    max_resource_bytes: NonZeroU64,
}

/// Недопустимая политика.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ResourceFetchPolicyError {
    /// Нулевой срок молчания: любой запрос умирал бы сразу после отправки.
    #[error("the resource fetch idle timeout must be greater than zero")]
    ZeroIdleTimeout,
}

impl ResourceFetchPolicy {
    /// Политика.
    ///
    /// - `max_in_flight` — сколько запросов одновременно отправлено и ещё не
    ///   закончено внешней стороной; следующий [`start`](super::ResourceFetchClient::start)
    ///   ждёт места, поэтому очередь отправки приложения не переполняется.
    /// - `idle_timeout` — сколько внешняя сторона может молчать (ни байта с отправки
    ///   запроса или между кусками); дольше — запрос считается сетевой ошибкой.
    /// - `max_resource_bytes` — потолок `max_bytes` одного запроса.
    ///
    /// # Errors
    /// [`ResourceFetchPolicyError::ZeroIdleTimeout`] при нулевом сроке молчания.
    pub const fn new(
        max_in_flight: NonZeroUsize,
        idle_timeout: Duration,
        max_resource_bytes: NonZeroU64,
    ) -> Result<Self, ResourceFetchPolicyError> {
        if idle_timeout.is_zero() {
            return Err(ResourceFetchPolicyError::ZeroIdleTimeout);
        }
        Ok(Self {
            max_in_flight,
            idle_timeout,
            max_resource_bytes,
        })
    }

    /// Сколько запросов одновременно у внешней стороны.
    #[must_use]
    pub const fn max_in_flight(&self) -> NonZeroUsize {
        self.max_in_flight
    }

    /// Сколько внешняя сторона может молчать.
    #[must_use]
    pub const fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    /// Самый большой ресурс одного запроса, байт.
    #[must_use]
    pub const fn max_resource_bytes(&self) -> NonZeroU64 {
        self.max_resource_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_idle_timeout_is_refused_and_valid_values_are_kept() {
        let in_flight = NonZeroUsize::new(8).expect("non-zero");
        let ceiling = NonZeroU64::new(1 << 20).expect("non-zero");
        assert_eq!(
            ResourceFetchPolicy::new(in_flight, Duration::ZERO, ceiling),
            Err(ResourceFetchPolicyError::ZeroIdleTimeout)
        );
        let policy = ResourceFetchPolicy::new(in_flight, Duration::from_secs(20), ceiling)
            .expect("valid policy");
        assert_eq!(policy.max_in_flight(), in_flight);
        assert_eq!(policy.idle_timeout(), Duration::from_secs(20));
        assert_eq!(policy.max_resource_bytes(), ceiling);
    }
}
