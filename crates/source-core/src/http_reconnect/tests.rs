//! Расписание переподключения на фальшивом времени: никаких реальных пауз.

use std::time::{Duration, Instant};

use super::{
    HttpReconnectDecision, HttpReconnectPolicy, HttpReconnectSchedule, wait_before_reconnect,
};
use crate::{CancellationToken, HttpRetryAfter, SourceError};

fn retry_after(decision: HttpReconnectDecision) -> Duration {
    match decision {
        HttpReconnectDecision::RetryAfter(delay) => delay,
        HttpReconnectDecision::GiveUp => panic!("ожидался повтор, а расписание сдалось"),
    }
}

/// Паузы растут 0,5 → 1 → 2 → 4 → 8 и дальше упираются в 8 с.
#[test]
fn backoff_doubles_from_half_second_and_caps_at_eight_seconds() {
    let start = Instant::now();
    let mut schedule =
        HttpReconnectSchedule::new(HttpReconnectPolicy::with_budget(Duration::from_secs(600)));
    let mut now = start;
    let mut delays = Vec::new();
    for _ in 0..7 {
        let delay = retry_after(schedule.record_failure(now, HttpRetryAfter::Unavailable));
        delays.push(delay.as_millis());
        now += delay;
    }

    assert_eq!(delays, [500, 1_000, 2_000, 4_000, 8_000, 8_000, 8_000]);
    assert_eq!(schedule.failed_attempts(), 7);
}

/// Бюджет считается от первого сбоя; последняя пауза обрезается остатком бюджета,
/// а после него расписание сдаётся.
#[test]
fn budget_counts_from_first_failure_and_trims_last_delay() {
    let start = Instant::now();
    let mut schedule =
        HttpReconnectSchedule::new(HttpReconnectPolicy::with_budget(Duration::from_secs(3)));

    assert_eq!(
        schedule.record_failure(start, HttpRetryAfter::Unavailable),
        HttpReconnectDecision::RetryAfter(Duration::from_millis(500))
    );
    // Попытка сама по себе заняла 2 с (например, таймаут чтения).
    let second_failure = start + Duration::from_millis(2_500);
    assert_eq!(
        schedule.record_failure(second_failure, HttpRetryAfter::Unavailable),
        HttpReconnectDecision::RetryAfter(Duration::from_millis(500)),
        "пауза 1 с обрезана до остатка бюджета 0,5 с"
    );
    assert_eq!(
        schedule.record_failure(start + Duration::from_secs(3), HttpRetryAfter::Unavailable),
        HttpReconnectDecision::GiveUp
    );
}

/// Нулевой бюджет — повторов нет совсем.
#[test]
fn disabled_policy_gives_up_on_first_failure() {
    let mut schedule = HttpReconnectSchedule::new(HttpReconnectPolicy::disabled());

    assert_eq!(
        schedule.record_failure(Instant::now(), HttpRetryAfter::Unavailable),
        HttpReconnectDecision::GiveUp
    );
}

/// `Retry-After` длиннее своей паузы побеждает, но не выходит за потолок 60 с
/// и за остаток бюджета.
#[test]
fn server_retry_after_wins_but_is_capped() {
    let start = Instant::now();
    let mut generous =
        HttpReconnectSchedule::new(HttpReconnectPolicy::with_budget(Duration::from_secs(600)));
    assert_eq!(
        generous.record_failure(start, HttpRetryAfter::Delay(Duration::from_secs(5))),
        HttpReconnectDecision::RetryAfter(Duration::from_secs(5))
    );
    assert_eq!(
        generous.record_failure(start, HttpRetryAfter::Delay(Duration::from_secs(3_600))),
        HttpReconnectDecision::RetryAfter(Duration::from_secs(60)),
        "server hint ограничен 60 с"
    );

    let mut short =
        HttpReconnectSchedule::new(HttpReconnectPolicy::with_budget(Duration::from_secs(2)));
    assert_eq!(
        short.record_failure(start, HttpRetryAfter::Delay(Duration::from_secs(30))),
        HttpReconnectDecision::RetryAfter(Duration::from_secs(2)),
        "server hint не выводит ожидание за бюджет"
    );
}

/// Короткий `Retry-After` не ускоряет собственную паузу.
#[test]
fn short_retry_after_does_not_shorten_local_backoff() {
    let mut schedule =
        HttpReconnectSchedule::new(HttpReconnectPolicy::with_budget(Duration::from_secs(60)));

    assert_eq!(
        schedule.record_failure(Instant::now(), HttpRetryAfter::Delay(Duration::ZERO)),
        HttpReconnectDecision::RetryAfter(Duration::from_millis(500))
    );
}

/// Отмена во время паузы прерывает ожидание сразу, а не по её окончании.
#[test]
fn cancellation_interrupts_wait_promptly() {
    let cancellation = CancellationToken::new();
    let canceller = cancellation.clone();
    let cancel_thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        canceller.cancel();
    });
    let started = Instant::now();

    let result = wait_before_reconnect(Duration::from_secs(30), &cancellation);

    assert!(matches!(result, Err(SourceError::Cancelled)));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "ожидание должно оборваться по отмене, а не через 30 с"
    );
    cancel_thread.join().expect("cancel thread");
}

/// Без отмены пауза выдерживается полностью.
#[test]
fn wait_without_cancellation_lasts_full_delay() {
    let started = Instant::now();

    wait_before_reconnect(Duration::from_millis(40), &CancellationToken::new())
        .expect("ожидание без отмены завершается успешно");

    assert!(started.elapsed() >= Duration::from_millis(40));
}
