use std::time::Duration;

use source_core::{CancellationToken, SourceError};

use super::{AdaptiveTransportError, wait_for_retry};

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
