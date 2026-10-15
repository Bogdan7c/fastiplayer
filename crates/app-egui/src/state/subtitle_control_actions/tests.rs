//! Обратная связь действий субтитров доходит до центра уведомлений как настоящие toast-ы.

use std::time::{Duration, Instant};

use super::*;
use crate::state::notifications::{NotificationsFrame, OpenProgress, TransientReplaceKey};
use crate::ui::animation::UiMotion;

fn toast_texts(center: &mut NotificationCenter, now: Instant) -> Vec<String> {
    let frame: NotificationsFrame = center.frame(OpenProgress::Idle, UiMotion::Standard, now);
    frame
        .toasts
        .iter()
        .map(|toast| toast.message.to_string())
        .collect()
}

#[test]
fn silent_feedback_shows_nothing() {
    let mut center = NotificationCenter::default();
    let now = Instant::now();
    deliver_feedback(&mut center, ControlActionFeedback::Silent, now);
    assert!(toast_texts(&mut center, now).is_empty());
}

#[test]
fn plain_toast_is_shown_and_expires_by_itself() {
    let mut center = NotificationCenter::default();
    let now = Instant::now();
    deliver_feedback(
        &mut center,
        ControlActionFeedback::Toast("No subtitles for this video".to_owned()),
        now,
    );
    assert_eq!(
        toast_texts(&mut center, now),
        ["No subtitles for this video"]
    );
    assert!(toast_texts(&mut center, now + Duration::from_secs(6)).is_empty());
}

#[test]
fn repeated_replacing_feedback_keeps_one_toast_with_the_latest_text() {
    let mut center = NotificationCenter::default();
    let now = Instant::now();
    for (step, text) in [
        "Subtitle delay: +0.1 s",
        "Subtitle delay: +0.2 s",
        "Subtitle delay: +0.3 s",
    ]
    .into_iter()
    .enumerate()
    {
        deliver_feedback(
            &mut center,
            ControlActionFeedback::ReplacingToast {
                key: TransientReplaceKey::SubtitleSync,
                text: text.to_owned(),
            },
            now + Duration::from_millis(200 * step as u64),
        );
    }
    assert_eq!(
        toast_texts(&mut center, now + Duration::from_millis(400)),
        ["Subtitle delay: +0.3 s"]
    );
}
