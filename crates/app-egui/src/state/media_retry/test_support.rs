//! Фальшивый порт повтора для тестов: записывает вызовы и держит настоящий центр уведомлений.

use std::path::PathBuf;
use std::time::Instant;

use playlist_core::PlaylistItemId;

use super::executor::{LocalOpenSlot, MediaRetryPort, RETRY_TARGET_GONE_MESSAGE, RetryStart};
use super::{ReopenPosition, RetryPlan};
use crate::external_open::request::DroppedWebUrl;
use crate::state::notifications::{
    MediaFailureOrigin, NotificationCenter, OPEN_STILL_IN_PROGRESS_MESSAGE,
};

/// Что сделал исполнитель повтора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RetryCall {
    ReportStillOpening,
    ClearFailure,
    OpenLocalFile(PathBuf),
    OpenWebUrl(DroppedWebUrl),
    PlayQueueItem(PlaylistItemId),
    ResumeBlockedQueue,
    ReopenFailedPlayback(ReopenPosition),
    ReportNothingToRetry,
}

/// Новая ошибка, которую фальшивое открытие «выдаёт» при повторном отказе.
#[derive(Debug, Clone)]
pub(crate) struct FailureOnOpen {
    pub(crate) message: String,
    pub(crate) retry: RetryPlan,
}

/// Порт, который записывает вызовы; ошибка живёт в настоящем `NotificationCenter`.
pub(crate) struct RecordingRetryPort {
    pub(crate) slot: LocalOpenSlot,
    pub(crate) row_play: RetryStart,
    pub(crate) calls: Vec<RetryCall>,
    pub(crate) center: NotificationCenter,
    /// Если задано, открытие ссылки или файла снова падает с этой ошибкой.
    pub(crate) failure_on_open: Option<FailureOnOpen>,
}

impl RecordingRetryPort {
    /// Свободный слот, Play строки стартует, повторных отказов нет.
    pub(crate) fn new(center: NotificationCenter) -> Self {
        Self {
            slot: LocalOpenSlot::Free,
            row_play: RetryStart::Started,
            calls: Vec::new(),
            center,
            failure_on_open: None,
        }
    }

    fn fail_again_if_configured(&mut self) {
        if let Some(failure) = self.failure_on_open.clone() {
            self.center.show_media_failure_planned(
                failure.message,
                MediaFailureOrigin::MediaOpen,
                failure.retry,
            );
        }
    }
}

impl MediaRetryPort for RecordingRetryPort {
    fn local_open_slot(&self) -> LocalOpenSlot {
        self.slot
    }

    fn report_still_opening(&mut self) {
        self.calls.push(RetryCall::ReportStillOpening);
        self.center
            .notify_transient(OPEN_STILL_IN_PROGRESS_MESSAGE, Instant::now());
    }

    fn clear_failure(&mut self) {
        self.calls.push(RetryCall::ClearFailure);
        self.center.resolve_media_failure();
    }

    fn open_local_file(&mut self, path: PathBuf) {
        self.calls.push(RetryCall::OpenLocalFile(path));
        self.fail_again_if_configured();
    }

    fn open_web_url(&mut self, url: &DroppedWebUrl) {
        self.calls.push(RetryCall::OpenWebUrl(url.clone()));
        self.fail_again_if_configured();
    }

    fn play_queue_item(&mut self, item_id: PlaylistItemId) -> RetryStart {
        self.calls.push(RetryCall::PlayQueueItem(item_id));
        self.row_play
    }

    fn resume_blocked_queue(&mut self) -> RetryStart {
        self.calls.push(RetryCall::ResumeBlockedQueue);
        self.row_play
    }

    fn reopen_failed_playback(&mut self, position: ReopenPosition) -> RetryStart {
        self.calls.push(RetryCall::ReopenFailedPlayback(position));
        self.row_play
    }

    fn report_nothing_to_retry(&mut self) {
        self.calls.push(RetryCall::ReportNothingToRetry);
        self.center
            .notify_transient(RETRY_TARGET_GONE_MESSAGE, Instant::now());
    }
}
