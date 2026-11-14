//! Юнит-тесты ожидания сети посреди эфира (решение 334).

use std::time::{Duration, Instant};

use source_core::{HttpReconnectPolicy, HttpRetryAfter, SecretHttpUrl, SourceError};
use web_media_adaptive::AdaptiveTransportError;

use super::*;
use crate::HlsVodOpenError;

/// Бюджет ожидания сети в тестах: короткий, чтобы проверить и «ждём», и «сдались».
const TEST_BUDGET: Duration = Duration::from_secs(10);
/// Первая пауза общего расписания (0,5 → 8 с).
const FIRST_DELAY: Duration = Duration::from_millis(500);

fn identity(media_sequence: u64) -> HlsLiveSegmentIdentity {
    HlsLiveSegmentIdentity {
        media_sequence,
        discontinuity_sequence: 0,
    }
}

fn retry() -> HlsLiveSegmentNetworkRetry {
    HlsLiveSegmentNetworkRetry::new(HttpReconnectPolicy::with_budget(TEST_BUDGET))
}

fn timeout_error() -> AdaptiveTransportError {
    AdaptiveTransportError::Source(SourceError::HttpTimeout {
        operation: "read",
        url: SecretHttpUrl::from_secret_for_open("https://live.example.invalid/s1.ts"),
    })
}

#[test]
fn a_failed_segment_waits_for_the_network_and_then_becomes_due() {
    let mut retry = retry();
    let failed_at = Instant::now();

    assert!(retry.defer(identity(7), 3, HttpRetryAfter::Unavailable, failed_at));

    // До срока повтора читать нечего, повтор не снимается.
    assert_eq!(
        retry.take_due(failed_at + FIRST_DELAY / 2),
        HlsLiveSegmentRetryState::Waiting
    );
    // После срока — повтор того же сегмента с учётом уже отданных пакетов.
    let HlsLiveSegmentRetryState::Due(pending) = retry.take_due(failed_at + FIRST_DELAY) else {
        panic!("the retry must be due after the first delay");
    };
    assert_eq!(pending.identity, identity(7));
    assert_eq!(pending.packets_already_delivered, 3);
    // Повтор снят: второй раз его нет.
    assert_eq!(
        retry.take_due(failed_at + FIRST_DELAY),
        HlsLiveSegmentRetryState::NotPending
    );
}

#[test]
fn a_reopened_segment_drops_exactly_the_packets_the_player_already_has() {
    let mut retry = retry();

    retry.note_segment_opened(2);

    // Первые два пакета плеер уже получил до обрыва — их нет на выходе.
    assert!(retry.is_already_delivered_packet());
    assert!(retry.is_already_delivered_packet());
    // Дальше — новые пакеты, их отдаём.
    assert!(!retry.is_already_delivered_packet());
    assert!(!retry.is_already_delivered_packet());
    // Счёт прочитанного включает пропущенные: при новом обрыве их снова пропустим.
    assert_eq!(retry.packets_read_in_segment(), 4);
}

#[test]
fn the_network_wait_gives_up_after_the_budget() {
    let mut retry = retry();
    let first_failure = Instant::now();
    assert!(retry.defer(identity(1), 0, HttpRetryAfter::Unavailable, first_failure));

    // Сбои продолжаются дольше бюджета: эфир сдаётся, повтора больше нет.
    let after_budget = first_failure + TEST_BUDGET + Duration::from_millis(1);
    assert!(!retry.defer(identity(1), 0, HttpRetryAfter::Unavailable, after_budget));
    assert_eq!(
        retry.take_due(after_budget),
        HlsLiveSegmentRetryState::NotPending
    );
}

#[test]
fn a_delivered_packet_proves_the_network_is_back_and_resets_the_budget() {
    let mut retry = retry();
    let first_failure = Instant::now();
    assert!(retry.defer(identity(1), 0, HttpRetryAfter::Unavailable, first_failure));
    retry.note_segment_opened(0);
    assert!(!retry.is_already_delivered_packet());

    // Новый обрыв позже бюджета первой серии — это новая серия, а не продолжение.
    let next_failure = first_failure + TEST_BUDGET * 2;
    assert!(retry.defer(identity(2), 0, HttpRetryAfter::Unavailable, next_failure));
    assert_eq!(
        retry.take_due(next_failure + FIRST_DELAY / 2),
        HlsLiveSegmentRetryState::Waiting
    );
}

#[test]
fn a_disabled_wait_keeps_the_old_fail_fast_behaviour() {
    let mut retry = HlsLiveSegmentNetworkRetry::new(HttpReconnectPolicy::disabled());

    assert!(!retry.defer(identity(1), 0, HttpRetryAfter::Unavailable, Instant::now()));
}

#[test]
fn the_server_retry_after_hint_is_respected() {
    let mut outage = HlsLiveNetworkOutage::new(HttpReconnectPolicy::with_budget(TEST_BUDGET));
    let now = Instant::now();

    let decision = outage.record_failure(now, HttpRetryAfter::Delay(Duration::from_secs(3)));

    assert_eq!(
        decision,
        HlsLiveOutageDecision::RetryAt(now + Duration::from_secs(3))
    );
}

#[test]
fn only_transient_transport_failures_of_a_playlist_wait_for_the_network() {
    assert_eq!(
        transient_manifest_failure(&HlsLiveOpenError::Transport(timeout_error())),
        Some(HttpRetryAfter::Unavailable)
    );
    // Отмена — не сетевой сбой: ждать нечего.
    assert_eq!(
        transient_manifest_failure(&HlsLiveOpenError::Transport(
            AdaptiveTransportError::Cancelled
        )),
        None
    );
    // Испорченный плейлист — честная ошибка, а не обрыв сети.
    assert_eq!(
        transient_manifest_failure(&HlsVodOpenError::NestedMasterPlaylist.into()),
        None
    );
}

#[test]
fn a_retry_with_nothing_to_resume_from_keeps_waiting_without_spending_the_budget() {
    let mut retry = retry();
    let failed_at = Instant::now();
    assert!(retry.defer(identity(7), 3, HttpRetryAfter::Unavailable, failed_at));
    let HlsLiveSegmentRetryState::Due(pending) = retry.take_due(failed_at + FIRST_DELAY) else {
        panic!("the retry must be due");
    };

    // Продолжать пока не с чего: повтор возвращается и снова «пора» при следующем чтении.
    retry.keep_waiting_for_segment(pending);

    assert_eq!(
        retry.take_due(failed_at + FIRST_DELAY),
        HlsLiveSegmentRetryState::Due(pending)
    );
}
