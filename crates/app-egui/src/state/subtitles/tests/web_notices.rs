//! Тексты и доставка уведомлений о сбое скачивания web-субтитров: точные английские фразы,
//! по одному toast на попытку, без ссылок.

use std::time::{Duration, Instant};

use media_source_open::web_subtitle_fetch::WebSubtitleFetchError;

use super::super::app_state_api::deliver_download_failures;
use crate::state::notifications::{NotificationCenter, OpenProgress};
use crate::subtitle_message::subtitle_download_failure_message;
use crate::ui::animation::UiMotion;

fn toast_texts(center: &mut NotificationCenter, now: Instant) -> Vec<String> {
    center
        .frame(OpenProgress::Idle, UiMotion::Standard, now)
        .toasts
        .iter()
        .map(|toast| toast.message.to_string())
        .collect()
}

#[test]
fn every_download_error_has_its_own_plain_english_message() {
    let cases = [
        (
            WebSubtitleFetchError::RateLimited { retry_after: None },
            "YouTube is limiting subtitle requests. Try again in a minute.",
        ),
        (
            WebSubtitleFetchError::NetworkUnavailable,
            "Could not download subtitles. Check your connection and try again.",
        ),
        (
            WebSubtitleFetchError::Timeout,
            "Could not download subtitles. Check your connection and try again.",
        ),
        (
            WebSubtitleFetchError::NotFound,
            "These subtitles are no longer available.",
        ),
        (
            WebSubtitleFetchError::LinkExpired,
            "These subtitles are no longer available.",
        ),
        (
            WebSubtitleFetchError::RefreshFailed,
            "These subtitles are no longer available.",
        ),
        (
            WebSubtitleFetchError::UnexpectedStatus { status: 500 },
            "Could not load these subtitles.",
        ),
        (
            WebSubtitleFetchError::TooLarge { limit_bytes: 1 },
            "Could not load these subtitles.",
        ),
        (
            WebSubtitleFetchError::InvalidResponse,
            "Could not load these subtitles.",
        ),
        (
            WebSubtitleFetchError::UnknownTrack,
            "Could not load these subtitles.",
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(
            subtitle_download_failure_message(&error),
            expected,
            "{error:?}"
        );
    }
}

#[test]
fn a_failed_attempt_becomes_one_toast_with_the_exact_text() {
    let mut center = NotificationCenter::default();
    let now = Instant::now();

    deliver_download_failures(
        &mut center,
        &[WebSubtitleFetchError::RateLimited {
            retry_after: Some(Duration::from_secs(30)),
        }],
        now,
    );

    assert_eq!(
        toast_texts(&mut center, now),
        ["YouTube is limiting subtitle requests. Try again in a minute."]
    );
}

#[test]
fn a_new_failure_replaces_the_previous_download_toast_instead_of_stacking() {
    let mut center = NotificationCenter::default();
    let now = Instant::now();

    deliver_download_failures(&mut center, &[WebSubtitleFetchError::Timeout], now);
    deliver_download_failures(
        &mut center,
        &[WebSubtitleFetchError::RateLimited { retry_after: None }],
        now + Duration::from_millis(500),
    );

    assert_eq!(
        toast_texts(&mut center, now + Duration::from_millis(600)),
        ["YouTube is limiting subtitle requests. Try again in a minute."]
    );
}

#[test]
fn no_failures_means_no_toast() {
    let mut center = NotificationCenter::default();
    let now = Instant::now();

    deliver_download_failures(&mut center, &[], now);

    assert!(toast_texts(&mut center, now).is_empty());
}
