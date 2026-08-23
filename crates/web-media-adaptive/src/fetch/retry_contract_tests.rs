use std::num::NonZeroU8;
use std::time::Duration;

use source_core::{CancellationToken, HttpRetryAfter, SourceError};

use super::{AdaptiveTransportError, wait_for_retry};
use crate::{AdaptiveRetryPolicy, AdaptiveRetryPolicyError};

/// `UnexpectedEof` остаётся transient независимо от поведения локального socket server-а.
#[test]
fn unexpected_eof_is_retryable_without_network_timing() {
    // Конструируем exact source outcome напрямую, не воспроизводя обрыв TCP по таймеру.
    let error = AdaptiveTransportError::Source(SourceError::UnexpectedEof {
        offset: 128,
        expected_bytes: 64,
        actual_bytes: 31,
    });

    assert!(error.is_retryable());
}

/// Уже подтверждённая отмена завершает backoff до первого sleep.
#[test]
fn pre_cancelled_retry_wait_returns_typed_cancellation_immediately() {
    // Отмена происходит до входа в wait, поэтому test не зависит от scheduler interleaving.
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let result = wait_for_retry(&cancellation, Duration::from_secs(1));

    assert!(matches!(result, Err(AdaptiveTransportError::Cancelled)));
}

/// Валидный server hint доминирует над коротким local backoff.
#[test]
fn retry_policy_uses_server_delay_when_it_is_longer() {
    let policy = AdaptiveRetryPolicy::new(
        NonZeroU8::new(3).expect("attempt budget"),
        Duration::from_millis(5),
        Duration::from_millis(20),
        Duration::from_secs(2),
    )
    .expect("retry policy");

    let delay = policy.retry_delay_after(
        NonZeroU8::MIN,
        HttpRetryAfter::Delay(Duration::from_secs(1)),
    );

    assert_eq!(delay, Duration::from_secs(1));
}

/// Огромный server hint не может обойти caller-owned policy cap.
#[test]
fn retry_policy_caps_server_delay() {
    let policy = AdaptiveRetryPolicy::new(
        NonZeroU8::new(3).expect("attempt budget"),
        Duration::from_millis(5),
        Duration::from_millis(20),
        Duration::from_secs(2),
    )
    .expect("retry policy");

    let delay = policy.retry_delay_after(
        NonZeroU8::MIN,
        HttpRetryAfter::Delay(Duration::from_secs(300)),
    );

    assert_eq!(delay, Duration::from_secs(2));
}

/// Отсутствующая или malformed подсказка сохраняет прежний local backoff.
#[test]
fn unavailable_server_hint_preserves_local_backoff() {
    let policy = AdaptiveRetryPolicy::new(
        NonZeroU8::new(3).expect("attempt budget"),
        Duration::from_millis(5),
        Duration::from_millis(20),
        Duration::from_secs(2),
    )
    .expect("retry policy");

    let delay = policy.retry_delay_after(NonZeroU8::MIN, HttpRetryAfter::Unavailable);

    assert_eq!(delay, Duration::from_millis(5));
}

/// Нулевой cap не может молча превратить поддержку `Retry-After` в no-op.
#[test]
fn retry_policy_rejects_zero_server_hint_cap() {
    let result = AdaptiveRetryPolicy::new(
        NonZeroU8::new(3).expect("attempt budget"),
        Duration::from_millis(5),
        Duration::from_millis(20),
        Duration::ZERO,
    );

    assert_eq!(result, Err(AdaptiveRetryPolicyError::ZeroMaximumRetryAfter));
}

/// Server hint cap остаётся внутри общего bounded readiness contract-а.
#[test]
fn retry_policy_rejects_server_hint_cap_above_readiness_bound() {
    let result = AdaptiveRetryPolicy::new(
        NonZeroU8::new(3).expect("attempt budget"),
        Duration::from_millis(5),
        Duration::from_millis(20),
        Duration::from_secs(61),
    );

    assert_eq!(
        result,
        Err(AdaptiveRetryPolicyError::RetryAfterExceedsReadinessBound)
    );
}
