//! Сквозной путь B9: ошибка открытия → «Повторить» в центре → исполнитель → тот же путь.
//!
//! Кадр egui рисуется настоящий (см. `state::notifications::render_tests` для кнопки);
//! здесь проверяется связка владельцев: причина → план повтора → центр → нажатие → цель.

use std::time::Instant;

use super::test_support::{FailureOnOpen, RecordingRetryPort, RetryCall};
use super::*;
use crate::media_open::{LocalOpenFailureReason, WebOpenFailureReason};
use crate::state::notifications::{
    CenterNotice, MediaFailureOrigin, NotificationCenter, OpenProgress, RetryOffer,
    RetryRequestOutcome,
};
use crate::ui::animation::UiMotion;
use crate::web_open_message::web_open_failure_message;

fn url() -> DroppedWebUrl {
    DroppedWebUrl::new("http://media.test/video.mp4".to_owned())
}

/// Ошибка, как её публикует владелец URL-открытия.
fn show_web_failure(center: &mut NotificationCenter, reason: WebOpenFailureReason) {
    center.show_media_failure_planned(
        web_open_failure_message(Some("media.test"), reason),
        MediaFailureOrigin::MediaOpen,
        RetryPlan::for_reason(reason.into(), MediaRetryTarget::WebUrl(url())),
    );
}

fn center_view(center: &mut NotificationCenter) -> Option<(NotificationId, RetryOffer)> {
    match center
        .frame(OpenProgress::Idle, UiMotion::Standard, Instant::now())
        .center
    {
        Some(CenterNotice::MediaFailure(view)) => Some((view.id, view.retry)),
        Some(CenterNotice::Progress(_)) | None => None,
    }
}

use crate::state::NotificationId;

/// Сеть пропала при открытии URL: кнопка есть, нажатие открывает тот же URL.
#[test]
fn network_error_offers_retry_and_click_reopens_the_same_url() {
    let mut center = NotificationCenter::default();
    show_web_failure(&mut center, WebOpenFailureReason::NoConnection);
    let (id, retry) = center_view(&mut center).expect("lapse");
    assert_eq!(retry, RetryOffer::Available);

    assert_eq!(center.request_retry(id), RetryRequestOutcome::Requested);
    let target = center.take_retry_request().expect("target accepted");
    let mut port = RecordingRetryPort::new(center);
    execute_media_retry_for_test(&mut port, target);

    assert_eq!(
        port.calls,
        [RetryCall::ClearFailure, RetryCall::OpenWebUrl(url())]
    );
    assert!(
        center_view(&mut port.center).is_none(),
        "Error removed: the outcome will show a new discovery"
    );
    assert!(
        port.center.take_retry_request().is_none(),
        "request is executed exactly once"
    );
}

/// «Формат не поддерживается»: повтор бессмыслен, кнопки нет, запрос отклоняется.
#[test]
fn unsupported_format_has_no_retry_and_a_forced_request_is_refused() {
    let mut center = NotificationCenter::default();
    show_web_failure(&mut center, WebOpenFailureReason::UnrecognizedFormat);
    let (id, retry) = center_view(&mut center).expect("lapse");

    assert_eq!(retry, RetryOffer::Unavailable);
    assert_eq!(center.request_retry(id), RetryRequestOutcome::NotOffered);
    assert!(center.take_retry_request().is_none());
}

/// Локальный файл «не найден»: повтор осмыслен (флешку могли вернуть), формат — нет.
#[test]
fn local_file_not_found_offers_retry_but_unrecognized_format_does_not() {
    let path = PathBuf::from("/media/usb/clip.mkv");
    for (reason, expected) in [
        (LocalOpenFailureReason::FileNotFound, RetryOffer::Available),
        (
            LocalOpenFailureReason::UnrecognizedFormat,
            RetryOffer::Unavailable,
        ),
    ] {
        let mut center = NotificationCenter::default();
        center.show_media_failure_planned(
            "error",
            MediaFailureOrigin::MediaOpen,
            RetryPlan::for_reason(reason.into(), MediaRetryTarget::LocalFile(path.clone())),
        );
        assert_eq!(center_view(&mut center).expect("error").1, expected);
    }
}

/// Повтор снова упал: в центре новая ошибка с той же кнопкой, и её тоже можно повторить.
#[test]
fn failed_retry_shows_a_new_error_with_the_same_button() {
    let mut center = NotificationCenter::default();
    show_web_failure(&mut center, WebOpenFailureReason::NoConnection);
    let (first_id, _) = center_view(&mut center).expect("first-time");
    center.request_retry(first_id);
    let target = center.take_retry_request().expect("goal");
    let mut port = RecordingRetryPort::new(center);
    port.failure_on_open = Some(FailureOnOpen {
        message: web_open_failure_message(Some("media.test"), WebOpenFailureReason::NoConnection),
        retry: RetryPlan::for_reason(
            WebOpenFailureReason::NoConnection.into(),
            MediaRetryTarget::WebUrl(url()),
        ),
    });

    execute_media_retry_for_test(&mut port, target);

    let (second_id, retry) = center_view(&mut port.center).expect("blunder");
    assert_ne!(
        second_id, first_id,
        "This is a new mistake, not an old one."
    );
    assert_eq!(retry, RetryOffer::Available);
    // И вторую попытку тоже можно повторить: тот же URL.
    assert_eq!(
        port.center.request_retry(second_id),
        RetryRequestOutcome::Requested
    );
    assert_eq!(
        port.center.take_retry_request(),
        Some(MediaRetryTarget::WebUrl(url()))
    );
}

/// Нажатие по уже исчезнувшей ошибке — штатный no-op.
#[test]
fn retry_on_a_gone_failure_is_refused_without_side_effects() {
    let mut center = NotificationCenter::default();
    show_web_failure(&mut center, WebOpenFailureReason::NoConnection);
    let (id, _) = center_view(&mut center).expect("error");
    center.resolve_media_failure();

    assert_eq!(center.request_retry(id), RetryRequestOutcome::AlreadyGone);
    assert!(center.take_retry_request().is_none());
}

fn execute_media_retry_for_test(port: &mut RecordingRetryPort, target: MediaRetryTarget) {
    super::executor::execute_media_retry(port, target);
}

#[test]
fn retry_plan_follows_the_advice_of_the_failure_reason() {
    let target = MediaRetryTarget::QueueItem(
        playlist_core::PlaylistItemId::from_persistence_value(3).expect("non-zero"),
    );
    assert_eq!(
        RetryPlan::for_reason(WebOpenFailureReason::ServerError.into(), target.clone()),
        RetryPlan::Offered(target.clone())
    );
    assert_eq!(
        RetryPlan::for_reason(WebOpenFailureReason::NotFound.into(), target),
        RetryPlan::NotOffered
    );
}
