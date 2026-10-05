//! Тесты чистой математики проявления toast-а.
//!
//! Отрисовка и клики по × проверяются в `state/notifications/render_tests.rs`, где есть
//! доступ к настоящему владельцу уведомлений.

use std::time::Duration;

use super::*;

#[test]
fn toast_fades_in_from_transparent_to_opaque_in_standard_motion() {
    assert_eq!(toast_opacity(Duration::ZERO, UiMotion::Standard), 0.0);

    let halfway = toast_opacity(TOAST_FADE_IN / 2, UiMotion::Standard);
    assert!(halfway > 0.0 && halfway < 1.0, "{halfway}");

    assert_eq!(toast_opacity(TOAST_FADE_IN, UiMotion::Standard), 1.0);
    assert_eq!(
        toast_opacity(Duration::from_secs(3), UiMotion::Standard),
        1.0
    );
}

#[test]
fn reduced_motion_shows_toast_fully_opaque_immediately() {
    assert_eq!(toast_opacity(Duration::ZERO, UiMotion::Reduced), 1.0);
    assert_eq!(toast_opacity(TOAST_FADE_IN / 2, UiMotion::Reduced), 1.0);
}
