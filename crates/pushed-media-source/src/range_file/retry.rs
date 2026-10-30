//! Повтор запроса диапазона после сетевого сбоя страницы.
//!
//! Правила те же, что у переподключения HTTP Range (`source-core`, сессия 16):
//! паузы растут 0,5 → 1 → 2 → 4 → 8 с и дальше остаются 8 с, общий бюджет считается
//! от первого сбоя серии, после бюджета — окончательная ошибка. Планировщик
//! `source-core` скрыт внутри crate, поэтому здесь свой, на тех же числах; число
//! бюджета приходит от приложения (`network.reconnect_wait_ms`).

use std::time::{Duration, Instant};

/// Первая пауза: короткий «блип» сети переживается почти незаметно.
const FIRST_RETRY_DELAY: Duration = Duration::from_millis(500);
/// Потолок экспоненциальной паузы: вернувшуюся сеть не заметим слишком поздно.
const MAXIMUM_RETRY_DELAY: Duration = Duration::from_secs(8);

/// Политика повторов запроса диапазона.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeRetryPolicy {
    /// Общее время от первого сбоя серии, после которого источник сдаётся.
    budget: Duration,
    /// Первая пауза серии.
    first_delay: Duration,
    /// Потолок паузы.
    maximum_delay: Duration,
}

impl RangeRetryPolicy {
    /// Политика с бюджетом из настроек и паузами 0,5 → 8 с.
    #[must_use]
    pub const fn with_budget(budget: Duration) -> Self {
        Self {
            budget,
            first_delay: FIRST_RETRY_DELAY,
            maximum_delay: MAXIMUM_RETRY_DELAY,
        }
    }

    /// Без повторов: первый же сбой сети окончателен.
    #[must_use]
    pub const fn disabled() -> Self {
        Self::with_budget(Duration::ZERO)
    }

    /// Свои паузы (в тестах — короткие, чтобы не ждать секундами).
    #[must_use]
    pub const fn with_delays(mut self, first_delay: Duration, maximum_delay: Duration) -> Self {
        self.first_delay = first_delay;
        self.maximum_delay = maximum_delay;
        self
    }

    /// Общий бюджет ожидания.
    #[must_use]
    pub const fn budget(self) -> Duration {
        self.budget
    }
}

/// Решение после очередного сбоя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RetryDecision {
    /// Подождать и повторить.
    After(Duration),
    /// Бюджет исчерпан.
    GiveUp,
}

/// Состояние серии повторов. Чистая логика: «сейчас» передаёт вызывающий.
#[derive(Debug, Clone, Copy)]
pub(super) struct RetrySchedule {
    /// Политика.
    policy: RangeRetryPolicy,
    /// Момент первого сбоя текущей серии.
    first_failure_at: Option<Instant>,
    /// Сбоев в текущей серии.
    failed_attempts: u32,
}

impl RetrySchedule {
    /// Пустая серия.
    pub(super) const fn new(policy: RangeRetryPolicy) -> Self {
        Self {
            policy,
            first_failure_at: None,
            failed_attempts: 0,
        }
    }

    /// Сбоев в текущей серии (для журнала).
    pub(super) const fn failed_attempts(&self) -> u32 {
        self.failed_attempts
    }

    /// Учитывает сбой. Пауза не длиннее остатка бюджета: ждать дольше нет смысла.
    pub(super) fn record_failure(&mut self, now: Instant) -> RetryDecision {
        let first_failure_at = *self.first_failure_at.get_or_insert(now);
        self.failed_attempts = self.failed_attempts.saturating_add(1);
        let elapsed = now.saturating_duration_since(first_failure_at);
        let remaining = self.policy.budget.saturating_sub(elapsed);
        if remaining.is_zero() {
            return RetryDecision::GiveUp;
        }
        let doublings = self.failed_attempts.saturating_sub(1).min(31);
        let delay = self
            .policy
            .first_delay
            .saturating_mul(1_u32 << doublings)
            .min(self.policy.maximum_delay)
            .min(remaining);
        RetryDecision::After(delay)
    }

    /// Пришли байты или запрос завершился: серия кончилась, бюджет — заново.
    pub(super) fn record_progress(&mut self) {
        self.first_failure_at = None;
        self.failed_attempts = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_double_up_to_the_ceiling_and_stop_at_the_budget() {
        let start = Instant::now();
        let mut schedule =
            RetrySchedule::new(RangeRetryPolicy::with_budget(Duration::from_secs(60)));
        let mut now = start;
        let mut delays = Vec::new();
        for _ in 0..6 {
            let RetryDecision::After(delay) = schedule.record_failure(now) else {
                panic!("budget must still allow a retry");
            };
            delays.push(delay);
            now += delay;
        }
        assert_eq!(
            delays,
            [500, 1000, 2000, 4000, 8000, 8000].map(Duration::from_millis)
        );
        assert_eq!(schedule.failed_attempts(), 6);
        // Бюджет считается от первого сбоя: через 60 с источник сдаётся.
        assert_eq!(
            schedule.record_failure(start + Duration::from_secs(61)),
            RetryDecision::GiveUp
        );
    }

    #[test]
    fn last_pause_is_cut_to_the_remaining_budget() {
        let start = Instant::now();
        let mut schedule =
            RetrySchedule::new(RangeRetryPolicy::with_budget(Duration::from_millis(700)));
        assert_eq!(
            schedule.record_failure(start),
            RetryDecision::After(Duration::from_millis(500))
        );
        assert_eq!(
            schedule.record_failure(start + Duration::from_millis(500)),
            RetryDecision::After(Duration::from_millis(200))
        );
    }

    #[test]
    fn disabled_policy_gives_up_on_the_first_failure() {
        let mut schedule = RetrySchedule::new(RangeRetryPolicy::disabled());
        assert_eq!(
            schedule.record_failure(Instant::now()),
            RetryDecision::GiveUp
        );
    }

    #[test]
    fn progress_restarts_the_series() {
        let start = Instant::now();
        let mut schedule =
            RetrySchedule::new(RangeRetryPolicy::with_budget(Duration::from_secs(1)));
        let _ = schedule.record_failure(start);
        schedule.record_progress();
        // Спустя долгое время после «старой» серии новая получает полный бюджет.
        assert_eq!(
            schedule.record_failure(start + Duration::from_secs(100)),
            RetryDecision::After(Duration::from_millis(500))
        );
    }
}
