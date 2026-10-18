//! Учёт сетевого перебоя ленты: ограниченные повторы с нарастающей паузой и общий бюджет.
//!
//! Видео при обрыве сети продолжает играть из своего буфера, а лента смотрит вперёд всего
//! на `ahead`, поэтому короткий перебой (10-30 секунд Wi-Fi) НЕ должен убивать дорожку:
//! временные (сетевые) сбои повторяются позже, а дорожка объявляется недоступной, только если
//! перебой не прошёл за весь бюджет. Постоянные сбои (404, битые данные) сюда не попадают -
//! они идут по прежнему пути «пропуск сегмента + серия до порога».

use std::time::{Duration, Instant};

/// Пауза перед первым повтором после сетевого сбоя.
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
/// Потолок паузы между повторами: пауза удваивается, но не дольше этого.
const MAXIMUM_BACKOFF: Duration = Duration::from_secs(8);

/// Параметры повторов; в production берутся из констант, тесты ставят короткие значения.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OutagePolicy {
    /// Пауза перед первым повтором.
    pub(crate) initial_backoff: Duration,
    /// Потолок паузы между повторами.
    pub(crate) maximum_backoff: Duration,
    /// Общий срок перебоя, после которого дорожка недоступна.
    pub(crate) budget: Duration,
}

impl OutagePolicy {
    /// Политика приложения: каденция повторов - константы выше, срок перебоя - из конфига.
    pub(crate) const fn with_budget(budget: Duration) -> Self {
        Self {
            initial_backoff: INITIAL_BACKOFF,
            maximum_backoff: MAXIMUM_BACKOFF,
            budget,
        }
    }
}

/// Что делать после очередного сетевого сбоя.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OutageVerdict {
    /// Повторить не раньше, чем через указанную паузу.
    RetryAfter(Duration),
    /// Бюджет перебоя исчерпан: сдаёмся.
    BudgetExhausted {
        /// Сколько сетевых сбоев подряд было за перебой.
        failures: u32,
    },
}

/// Состояние текущего перебоя. Один на ленту: индекс и сегменты страдают от одной и той же сети.
#[derive(Debug)]
pub(super) struct OutageTracker {
    policy: OutagePolicy,
    /// Когда начался перебой (первый сбой после последнего успеха); `None` - сеть в порядке.
    started_at: Option<Instant>,
    failures: u32,
}

impl OutageTracker {
    pub(super) const fn new(policy: OutagePolicy) -> Self {
        Self {
            policy,
            started_at: None,
            failures: 0,
        }
    }

    /// Любой успешный сетевой запрос заканчивает перебой.
    pub(super) fn record_success(&mut self) {
        self.started_at = None;
        self.failures = 0;
    }

    /// Учитывает сетевой сбой, случившийся в момент `now`.
    pub(super) fn record_failure(&mut self, now: Instant) -> OutageVerdict {
        let started_at = *self.started_at.get_or_insert(now);
        self.failures = self.failures.saturating_add(1);
        if now.saturating_duration_since(started_at) >= self.policy.budget {
            return OutageVerdict::BudgetExhausted {
                failures: self.failures,
            };
        }
        OutageVerdict::RetryAfter(self.backoff_for(self.failures))
    }

    /// Пауза после `failures`-го сбоя подряд: initial * 2^(n-1), не больше потолка.
    fn backoff_for(&self, failures: u32) -> Duration {
        let doublings = failures.saturating_sub(1).min(16);
        self.policy
            .initial_backoff
            .saturating_mul(1_u32 << doublings)
            .min(self.policy.maximum_backoff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> OutagePolicy {
        OutagePolicy {
            initial_backoff: Duration::from_millis(100),
            maximum_backoff: Duration::from_millis(350),
            budget: Duration::from_secs(1),
        }
    }

    #[test]
    fn backoff_doubles_up_to_the_cap_then_budget_is_exhausted() {
        let mut tracker = OutageTracker::new(policy());
        let start = Instant::now();
        let waits: Vec<_> = [0, 100, 300, 600, 900]
            .iter()
            .map(|ms| tracker.record_failure(start + Duration::from_millis(*ms)))
            .collect();
        assert_eq!(
            waits,
            vec![
                OutageVerdict::RetryAfter(Duration::from_millis(100)),
                OutageVerdict::RetryAfter(Duration::from_millis(200)),
                OutageVerdict::RetryAfter(Duration::from_millis(350)),
                OutageVerdict::RetryAfter(Duration::from_millis(350)),
                OutageVerdict::RetryAfter(Duration::from_millis(350)),
            ]
        );
        assert_eq!(
            tracker.record_failure(start + Duration::from_millis(1_000)),
            OutageVerdict::BudgetExhausted { failures: 6 }
        );
    }

    #[test]
    fn success_starts_a_fresh_outage_with_a_fresh_budget() {
        let mut tracker = OutageTracker::new(policy());
        let start = Instant::now();
        let _ = tracker.record_failure(start);
        tracker.record_success();
        // Через минуту после старого перебоя новый начинается с нуля, а не сдаётся сразу.
        assert_eq!(
            tracker.record_failure(start + Duration::from_secs(60)),
            OutageVerdict::RetryAfter(Duration::from_millis(100))
        );
    }
}
