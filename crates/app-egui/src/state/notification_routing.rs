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
//! - [`AppState::notify_open_still_in_progress`] — повторный Open во время открытия;
//! - [`AppState::show_playlist_queue_notices`] — итоги автоматических пропусков в очереди.

use std::sync::Arc;
use std::time::Instant;

use player_core::{PlayerEvent, PlayerSnapshot};
use tracing::debug;

use super::AppState;
use super::notifications::{NotificationsFrame, OPEN_STILL_IN_PROGRESS_MESSAGE, OpenProgress};
use crate::playlist_runtime::AutomaticQueueNotice;
use crate::playlist_skip_message::{PlaylistQueueNoticeDelivery, playlist_queue_notice_delivery};
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
        debug!("Открытие media уже идёт, повторный запрос отклонён с уведомлением");
        self.notify_transient(OPEN_STILL_IN_PROGRESS_MESSAGE);
    }

    /// Показывает итоги цепочек автоматических пропусков битых файлов очереди (сессия 07).
    ///
    /// Вид показа и текст выбирает `playlist_queue_notice_delivery`; здесь только маршрут
    /// к соответствующему intent-методу.
    pub(crate) fn show_playlist_queue_notices(&mut self, notices: Vec<AutomaticQueueNotice>) {
        for notice in &notices {
            debug!(?notice, "Итог автоматического пропуска в очереди");
            match playlist_queue_notice_delivery(notice) {
                PlaylistQueueNoticeDelivery::InfoToast(text) => self.notify_info(text),
                PlaylistQueueNoticeDelivery::TransientToast(text) => self.notify_transient(text),
                PlaylistQueueNoticeDelivery::MediaFailure(text) => self.set_startup_error(text),
            }
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
                NotificationAction::Dismiss(id) => {
                    // «Уже нет» — штатный no-op: плашка могла истечь в том же кадре.
                    let outcome = self.notifications.dismiss(id);
                    debug!(?outcome, "Уведомление закрыто пользователем");
                    self.mark_pending_worker_redraw();
                }
            }
        }
    }
}
