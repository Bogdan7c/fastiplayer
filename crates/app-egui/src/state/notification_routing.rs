//! Связка `AppState` ↔ владелец уведомлений.
//!
//! Здесь только маршрутизация: откуда берётся «сейчас», когда просить перерисовку и как
//! действия UI возвращаются владельцу. Решения о жизненном цикле принимает
//! `state::notifications::NotificationCenter`.
//!
//! Intent-API для других модулей и следующих сессий (05, 07, 10, 15…):
//! - [`AppState::notify_transient`] — временное сообщение (исчезнет само через 5 с);
//! - [`AppState::notify_info`] — информационное (8 с);
//! - [`AppState::set_startup_error`] (в `state.rs`) — фатальная ошибка открытия в центре;
//! - [`AppState::report_open_failure`] — то же, но владелец ошибки решает, будет ли
//!   «Повторить» и что повторять (B9, `state::media_retry`);
//! - [`AppState::notify_open_still_in_progress`] — повторный Open во время открытия;
//! - [`AppState::show_playlist_queue_notices`] — итоги автоматических пропусков в очереди.
//! - [`AppState::show_playlist_import_notices`] — как прочитан импортированный плейлист.

use std::sync::Arc;
use std::time::Instant;

use player_core::{PlayerEvent, PlayerSnapshot};
use tracing::debug;

use super::AppState;
use super::media_retry::{MediaRetryTarget, RetryPlan};
use super::notifications::{
    MediaFailureOrigin, NotificationsFrame, OPEN_STILL_IN_PROGRESS_MESSAGE, OpenProgress,
    RetryRequestOutcome,
};
use crate::playlist_runtime::{AutomaticQueueNotice, PlaylistImportNotice};
use crate::playlist_skip_message::{PlaylistQueueNoticeDelivery, playlist_queue_notice_delivery};
use crate::startup_readiness::StartupReadinessAbortReason;
use crate::ui::animation::UiMotion;
use crate::ui::notifications::{NotificationAction, NotificationUiOutput};

impl AppState {
    /// Показывает временное уведомление в углу (исчезнет само).
    pub(crate) fn notify_transient(&mut self, message: impl Into<Arc<str>>) {
        self.notifications.notify_transient(message, Instant::now());
        self.mark_pending_worker_redraw();
    }

    /// Показывает информационное уведомление в углу (исчезнет само).
    pub(crate) fn notify_info(&mut self, message: impl Into<Arc<str>>) {
        self.notifications.notify_info(message, Instant::now());
        self.mark_pending_worker_redraw();
    }

    /// Сообщает пользователю, что повторный Open отклонён, пока идёт прошлое открытие.
    pub(crate) fn notify_open_still_in_progress(&mut self) {
        debug!("Opening media is already underway, re-request rejected with notification");
        self.notify_transient(OPEN_STILL_IN_PROGRESS_MESSAGE);
    }

    /// Показывает ошибку открытия в центре; `retry` — решение владельца ошибки о «Повторить».
    ///
    /// Общая точка для `set_startup_error` (без повтора) и ошибок, у которых повтор осмыслен.
    /// Ошибка висит до ×, успешного открытия или нового открытия, как и раньше.
    pub(crate) fn report_open_failure(&mut self, message: String, retry: RetryPlan) {
        self.abort_startup_readiness(StartupReadinessAbortReason::PreparationFailed);
        self.startup_pending = None;
        self.notifications.show_media_failure_planned(
            message,
            MediaFailureOrigin::MediaOpen,
            retry,
        );
        self.mark_pending_worker_redraw();
    }

    /// Показывает итоги цепочек автоматических пропусков битых файлов очереди (сессия 07).
    ///
    /// Вид показа и текст выбирает `playlist_queue_notice_delivery`; здесь только маршрут
    /// к соответствующему intent-методу.
    pub(crate) fn show_playlist_queue_notices(&mut self, notices: Vec<AutomaticQueueNotice>) {
        for notice in &notices {
            debug!(?notice, "The result of automatic pass in the queue");
            match playlist_queue_notice_delivery(notice) {
                PlaylistQueueNoticeDelivery::InfoToast(text) => self.notify_info(text),
                PlaylistQueueNoticeDelivery::TransientToast(text) => self.notify_transient(text),
                PlaylistQueueNoticeDelivery::MediaFailure(text) => self.set_startup_error(text),
                // Очередь стоит из-за сети: «Повторить» = Play на остановленном элементе (B9).
                PlaylistQueueNoticeDelivery::ResumableMediaFailure(text) => self
                    .report_open_failure(
                        text,
                        RetryPlan::Offered(MediaRetryTarget::ResumeBlockedQueue),
                    ),
            }
        }
    }

    /// Показывает факты о чтении импортированных плейлистов (B1: «прочитан в Windows-1251»).
    ///
    /// Это информация, а не ошибка: плейлист уже разобран, поэтому инфо-toast (8 с).
    pub(crate) fn show_playlist_import_notices(&mut self, notices: Vec<PlaylistImportNotice>) {
        for notice in notices {
            debug!(?notice, "Notice of reading the playlist");
            self.notify_info(notice.message());
        }
    }

    /// Передаёт событие player-а владельцу уведомлений (recoverable-отказ → toast).
    pub(crate) fn handle_notification_player_event(&mut self, event: &PlayerEvent) {
        self.notifications
            .record_player_event(event, Instant::now());
    }

    /// Ближайший момент, когда окно надо перерисовать: истёкший toast исчезнет,
    /// спиннер ожидания появится.
    pub(crate) fn next_notification_wake_deadline(&self) -> Option<Instant> {
        self.notifications.next_wake_deadline()
    }

    /// Готовит проекцию уведомлений для текущего кадра.
    ///
    /// Сначала сверяет фатальную ошибку и ожидание данных с состоянием player-а, затем
    /// строит проекцию с учётом идущего открытия и настройки reduced motion.
    pub(super) fn notifications_frame(
        &mut self,
        player_snapshot: &PlayerSnapshot,
        now: Instant,
    ) -> NotificationsFrame {
        self.notifications.observe_player_snapshot(player_snapshot);
        self.notifications
            .observe_playback_waiting(player_snapshot.playback_state, now);
        let progress = self
            .startup_pending
            .as_deref()
            .map_or(OpenProgress::Idle, OpenProgress::InProgress);
        let motion = UiMotion::from_reduced_motion(self.committed_config_snapshot.reduced_motion());
        self.notifications.frame(progress, motion, now)
    }

    /// Применяет действия пользователя с уведомлениями после egui pass-а.
    pub(super) fn apply_notification_actions(&mut self, mut output: NotificationUiOutput) {
        for action in output.take_actions() {
            match action {
                NotificationAction::Retry(id) => {
                    // Исполняется в этом же кадре (`apply_requested_media_retry`): здесь нет
                    // ни очереди, ни renderer-а, поэтому цель только запоминается.
                    let outcome = self.notifications.request_retry(id);
                    debug!(?outcome, "Pressed \"Repeat.\"");
                    if outcome == RetryRequestOutcome::Requested {
                        self.mark_pending_worker_redraw();
                    }
                }
                NotificationAction::Dismiss(id) => {
                    // «Уже нет» — штатный no-op: плашка могла истечь в том же кадре.
                    let outcome = self.notifications.dismiss(id);
                    debug!(?outcome, "Notification closed by the user");
                    self.mark_pending_worker_redraw();
                }
            }
        }
    }
}
