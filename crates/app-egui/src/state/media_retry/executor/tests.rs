//! Исполнитель повтора: правило «занято», выбор пути под цель, реакция на «нечего повторять».

use std::time::Duration;

use super::*;
use crate::state::media_retry::RetryPlan;
use crate::state::media_retry::test_support::{RecordingRetryPort, RetryCall};
use crate::state::notifications::{
    NotificationCenter, OPEN_STILL_IN_PROGRESS_MESSAGE, OpenProgress,
};
use crate::ui::animation::UiMotion;

fn item(id: u64) -> PlaylistItemId {
    PlaylistItemId::from_persistence_value(id).expect("test item id is non-zero")
}

fn url() -> DroppedWebUrl {
    DroppedWebUrl::new("http://media.test/video.mp4".to_owned())
}

/// Тексты toast-ов, которые увидел бы пользователь.
fn toast_texts(port: &mut RecordingRetryPort) -> Vec<String> {
    port.center
        .frame(
            OpenProgress::Idle,
            UiMotion::Standard,
            std::time::Instant::now(),
        )
        .toasts
        .into_iter()
        .map(|toast| toast.message.to_string())
        .collect()
}

#[test]
fn web_url_target_reopens_exactly_that_url_through_the_url_path() {
    let mut port = RecordingRetryPort::new(NotificationCenter::default());

    execute_media_retry(&mut port, MediaRetryTarget::WebUrl(url()));

    assert_eq!(
        port.calls,
        [RetryCall::ClearFailure, RetryCall::OpenWebUrl(url())]
    );
}

#[test]
fn local_file_target_reopens_exactly_that_file_through_the_open_path() {
    let mut port = RecordingRetryPort::new(NotificationCenter::default());
    let path = PathBuf::from("/media/usb/clip.mkv");

    execute_media_retry(&mut port, MediaRetryTarget::LocalFile(path.clone()));

    assert_eq!(
        port.calls,
        [RetryCall::ClearFailure, RetryCall::OpenLocalFile(path)]
    );
}

#[test]
fn queue_item_resume_and_reopen_targets_use_the_queue_play_path() {
    let mut port = RecordingRetryPort::new(NotificationCenter::default());

    execute_media_retry(&mut port, MediaRetryTarget::QueueItem(item(7)));
    execute_media_retry(&mut port, MediaRetryTarget::ResumeBlockedQueue);
    execute_media_retry(
        &mut port,
        MediaRetryTarget::ReopenFailedPlayback {
            position: ReopenPosition::At(Duration::from_secs(83)),
        },
    );

    assert_eq!(
        port.calls,
        [
            RetryCall::ClearFailure,
            RetryCall::PlayQueueItem(item(7)),
            RetryCall::ClearFailure,
            RetryCall::ResumeBlockedQueue,
            RetryCall::ClearFailure,
            RetryCall::ReopenFailedPlayback(ReopenPosition::At(Duration::from_secs(83))),
        ],
        "The cliff position reaches the opening path without distortion"
    );
}

/// Правило «занято» (сессия 18): повтор не стартует, ошибка остаётся, пользователь видит
/// «Файл ещё открывается».
#[test]
fn busy_open_slot_keeps_the_failure_and_reports_still_opening() {
    let mut center = NotificationCenter::default();
    center.show_media_failure_planned(
        "Could not open the link: cannot connect to the server; check your internet connection",
        crate::state::notifications::MediaFailureOrigin::MediaOpen,
        RetryPlan::Offered(MediaRetryTarget::WebUrl(url())),
    );
    let mut port = RecordingRetryPort::new(center);
    port.slot = LocalOpenSlot::Busy;

    execute_media_retry(&mut port, MediaRetryTarget::WebUrl(url()));

    assert_eq!(port.calls, [RetryCall::ReportStillOpening]);
    assert_eq!(toast_texts(&mut port), [OPEN_STILL_IN_PROGRESS_MESSAGE]);
    let frame = port.center.frame(
        OpenProgress::Idle,
        UiMotion::Standard,
        std::time::Instant::now(),
    );
    assert!(
        frame.center.is_some(),
        "The error is not lost; it can be repeated later."
    );
}

/// Элемента уже нет: пользователь узнаёт об этом, а не получает молчаливый no-op.
#[test]
fn nothing_to_open_is_reported_after_failure_was_cleared() {
    let mut port = RecordingRetryPort::new(NotificationCenter::default());
    port.row_play = RetryStart::NothingToOpen;

    execute_media_retry(&mut port, MediaRetryTarget::ResumeBlockedQueue);

    assert_eq!(
        port.calls,
        [
            RetryCall::ClearFailure,
            RetryCall::ResumeBlockedQueue,
            RetryCall::ReportNothingToRetry
        ]
    );
    assert_eq!(toast_texts(&mut port), [RETRY_TARGET_GONE_MESSAGE]);
}
