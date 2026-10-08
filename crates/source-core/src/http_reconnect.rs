//! Политика переподключения HTTP Range чтения после обрыва сети (сессия 16).
//!
//! Модуль владеет только решением «ждать ли ещё и сколько» и отменяемым ожиданием.
//! Сам повтор запроса (с какого байта, какие заголовки, проверка «тот же ли файл»)
//! остаётся у [`crate::HttpRangeSource`], а классификация ошибок — у
//! [`crate::SourceError::is_transient_network_failure`].
//!
//! Решения владельца (8 октября 2026):
//! - паузы между попытками растут 0,5 → 1 → 2 → 4 → 8 с и дальше остаются 8 с;
//! - общий бюджет ожидания по умолчанию 60 с и настраивается в `[network]`;
//! - `Retry-After` сервера уважается, но не дольше 60 с (как у HLS/DASH);
//! - после бюджета чтение возвращает последнюю ошибку, и пользователь видит
//!   понятный текст; бесконечных повторов нет.

use std::thread;
use std::time::{Duration, Instant};

use crate::{CancellationToken, HttpRetryAfter, SourceError, SourceResult};

/// Первая пауза после сбоя: короткий «блип» сети переживается почти незаметно.
const FIRST_RECONNECT_DELAY: Duration = Duration::from_millis(500);

/// Потолок экспоненциальной паузы: дальше ждать между попытками дольше нет смысла,
/// иначе вернувшуюся сеть плеер заметит слишком поздно.
const MAXIMUM_RECONNECT_DELAY: Duration = Duration::from_secs(8);

/// Потолок для `Retry-After`: тот же нейтральный предел 60 с, что у adaptive транспорта.
const MAXIMUM_SERVER_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Шаг опроса отмены во время паузы: закрытие файла прерывает ожидание за ≤ 10 мс.
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Сколько всего можно ждать восстановления связи для одного логического чтения.
///
/// Нулевой бюджет означает «не переподключаться»: первая сетевая ошибка сразу
/// уходит вызывающему коду.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpReconnectPolicy {
    /// Общее время от первого сбоя, после которого чтение сдаётся.
    budget: Duration,
}

impl HttpReconnectPolicy {
    /// Создаёт политику с бюджетом из пользовательского config.
    #[must_use]
    pub const fn with_budget(budget: Duration) -> Self {
        Self { budget }
    }

    /// Политика без переподключения: первая ошибка сразу видна вызывающему коду.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            budget: Duration::ZERO,
        }
    }

    /// Возвращает общий бюджет ожидания восстановления.
    #[must_use]
    pub const fn budget(self) -> Duration {
        self.budget
    }
}

/// Решение после очередной временной ошибки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HttpReconnectDecision {
    /// Подождать указанное время и повторить запрос.
    RetryAfter(Duration),
    /// Бюджет исчерпан: вернуть ошибку вызывающему коду.
    GiveUp,
}

/// Состояние серии повторов одного логического чтения.
///
/// Чистая логика без часов и сна: момент «сейчас» передаёт вызывающий код, поэтому
/// расписание проверяется тестами на фальшивом времени.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HttpReconnectSchedule {
    /// Политика владельца config.
    policy: HttpReconnectPolicy,
    /// Момент первого сбоя текущей серии; бюджет считается от него.
    first_failure_at: Option<Instant>,
    /// Сколько неудачных попыток уже было в текущей серии.
    failed_attempts: u32,
}

impl HttpReconnectSchedule {
    /// Начинает пустую серию: сбоев ещё не было.
    pub(crate) const fn new(policy: HttpReconnectPolicy) -> Self {
        Self {
            policy,
            first_failure_at: None,
            failed_attempts: 0,
        }
    }

    /// Номер следующей попытки (для логов): 1 — первый повтор после сбоя.
    pub(crate) const fn failed_attempts(&self) -> u32 {
        self.failed_attempts
    }

    /// Учитывает очередной сбой и решает, ждать ли дальше.
    ///
    /// Пауза = max(экспоненциальная пауза, `Retry-After` с потолком), но не дольше
    /// остатка бюджета: ждать за пределами бюджета бессмысленно, следующая попытка
    /// всё равно была бы последней.
    pub(crate) fn record_failure(
        &mut self,
        now: Instant,
        retry_after: HttpRetryAfter,
    ) -> HttpReconnectDecision {
        let first_failure_at = *self.first_failure_at.get_or_insert(now);
        self.failed_attempts = self.failed_attempts.saturating_add(1);
        let elapsed = now.saturating_duration_since(first_failure_at);
        let Some(remaining_budget) = self.policy.budget.checked_sub(elapsed) else {
            return HttpReconnectDecision::GiveUp;
        };
        if remaining_budget.is_zero() {
            return HttpReconnectDecision::GiveUp;
        }

        let server_delay = retry_after.delay().map_or(Duration::ZERO, |delay| {
            delay.min(MAXIMUM_SERVER_RETRY_AFTER)
        });
        let delay = exponential_delay(self.failed_attempts)
            .max(server_delay)
            .min(remaining_budget);
        HttpReconnectDecision::RetryAfter(delay)
    }
}

/// 0,5 с после первого сбоя, затем удвоение до потолка 8 с.
fn exponential_delay(failed_attempts: u32) -> Duration {
    let doublings = failed_attempts.saturating_sub(1).min(31);
    FIRST_RECONNECT_DELAY
        .saturating_mul(1_u32 << doublings)
        .min(MAXIMUM_RECONNECT_DELAY)
}

/// Ждёт паузу перед повтором, просыпаясь каждые 10 мс проверить отмену.
///
/// Отмена (закрытие/смена media, перемотка) прерывает ожидание сразу и
/// возвращает `Cancelled`, чтобы повтор не ушёл в сеть.
pub(crate) fn wait_before_reconnect(
    delay: Duration,
    cancellation: &CancellationToken,
) -> SourceResult<()> {
    let deadline = Instant::now() + delay;
    loop {
        if cancellation.is_cancelled() {
            return Err(SourceError::Cancelled);
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        thread::sleep((deadline - now).min(CANCELLATION_POLL_INTERVAL));
    }
}

#[cfg(test)]
mod tests;
