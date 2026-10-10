//! Очередь упирается не в сам элемент, а в обстоятельство, общее для соседей (B8).
//!
//! Обычный отказ открытия — вина файла или ссылки: элемент получает бейдж, а политика
//! `skip` идёт к следующему. Но если сети нет, то следующая web-ссылка тоже не откроется, и
//! очередь перебрала бы все элементы, пометив каждый «битым». Решение владельца (UX edge
//! cases 2, B8): очередь **останавливается на первом элементе**, элемент битым не
//! помечается, пользователь видит причину, а после её устранения нажимает Play и
//! продолжает с того же элемента.
//!
//! Модуль владеет только типами решения и маршрутом отчёта об отказе к controller-у;
//! само состояние «остановились на элементе X» хранит controller
//! (`controller::automatic_lifecycle::queue_block`).

use super::controller::{
    AutomaticTargetBlockedOutcome, ManualNavigationFailureOutcome, PlannedPlaylistInstall,
};
use super::row_interactions::RuntimeRowPlayOutcome;
use super::{PlaylistRuntime, PlaylistTargetFailureSummary};
use crate::media_open::{MediaOpenRequestId, WebOpenFailureReason};

/// Почему очередь остановилась на элементе, который при этом не считается битым.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueueBlockReason {
    /// Нет сети: соединение с сервером не установилось (DNS, отказ соединения, TLS).
    NoNetwork,
    /// Сайт ограничил частоту запросов (HTTP 429): следующий элемент того же сайта получит
    /// тот же отказ, поэтому пропуск бессмыслен.
    SiteRateLimited,
}

impl QueueBlockReason {
    /// Решает, останавливает ли web-причина отказа всю очередь.
    ///
    /// `match` исчерпывающий намеренно: новая причина в `WebOpenFailureReason` ломает
    /// компиляцию и заставляет явно решить, чья это вина — элемента или обстоятельств.
    ///
    /// Только `NoConnection` считается «нет сети». Таймаут сервера, обрыв соединения и
    /// ответы 5xx остаются отказом элемента (skip): по ним нельзя честно отличить
    /// медленный или упавший сервер от пропавшей сети, а ложная остановка очереди хуже
    /// лишнего пропуска. 404/410, «видео недоступно» и «нужен вход» — вина конкретной
    /// ссылки.
    pub(crate) const fn from_web_open_failure(reason: WebOpenFailureReason) -> Option<Self> {
        match reason {
            WebOpenFailureReason::NoConnection => Some(Self::NoNetwork),
            WebOpenFailureReason::RateLimited => Some(Self::SiteRateLimited),
            WebOpenFailureReason::ExtractorNotInstalled
            | WebOpenFailureReason::ExtractorDisabled
            | WebOpenFailureReason::ExtractorTimedOut
            | WebOpenFailureReason::ExtractorFailed
            | WebOpenFailureReason::SitePrivateMedia
            | WebOpenFailureReason::SiteLoginRequired
            | WebOpenFailureReason::SiteGeoRestricted
            | WebOpenFailureReason::SiteMediaUnavailable
            | WebOpenFailureReason::SiteUnsupported
            | WebOpenFailureReason::SiteRejected
            | WebOpenFailureReason::CollectionLink
            | WebOpenFailureReason::CollectionTooLarge
            | WebOpenFailureReason::NoPlayableFormat
            | WebOpenFailureReason::NotFound
            | WebOpenFailureReason::Gone
            | WebOpenFailureReason::AuthenticationRequired
            | WebOpenFailureReason::AccessDenied
            | WebOpenFailureReason::ServerError
            | WebOpenFailureReason::TimedOut
            | WebOpenFailureReason::ConnectionInterrupted
            | WebOpenFailureReason::InvalidServerResponse
            | WebOpenFailureReason::UnrecognizedFormat
            | WebOpenFailureReason::UnsupportedLink
            | WebOpenFailureReason::UnsupportedLiveProfile
            | WebOpenFailureReason::Unclassified => None,
        }
    }
}

/// Отчёт владельцу очереди о том, почему элемент не открылся.
///
/// Тип различает «виноват сам элемент» и «виновато обстоятельство»: строкой или `bool`
/// это не выразить без потери смысла (правило 6 AGENTS.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlaylistTargetFailure {
    /// Отказ самого файла или ссылки: бейдж на строке, политика `error_behavior`.
    ItemFault(PlaylistTargetFailureSummary),
    /// Отказ из-за общих обстоятельств: без бейджа, очередь останавливается.
    QueueBlocked(QueueBlockReason),
}

impl PlaylistRuntime {
    /// Отчёт об отказе открытия элемента после того, как controller принял его план.
    ///
    /// Отказ элемента идёт прежним маршрутом; блокирующее обстоятельство останавливает
    /// автоматическую очередь (ручной выбор сохраняет D55-семантику без бейджа).
    pub(crate) fn report_playlist_target_failure(
        &mut self,
        request_id: MediaOpenRequestId,
        item_id: playlist_core::PlaylistItemId,
        failure: PlaylistTargetFailure,
    ) -> Option<PlannedPlaylistInstall> {
        match failure {
            PlaylistTargetFailure::ItemFault(summary) => {
                self.report_playlist_navigation_failure(request_id, item_id, summary)
            }
            PlaylistTargetFailure::QueueBlocked(reason) => {
                self.report_playlist_navigation_blocked(request_id, item_id, reason);
                // Остановленная очередь не продолжается: плана следующего элемента нет.
                None
            }
        }
    }

    /// Play после остановки очереди обстоятельством: открыть заново тот же элемент.
    ///
    /// `None` — очередь не заблокирована (или элемент уже удалён): вызывающий код ведёт
    /// Play обычным путём. Иначе это обычный Play строки (тот же маршрут и те же исходы),
    /// поэтому повторный отказ снова остановит очередь на этом же элементе.
    pub(crate) fn play_queue_blocked_item(&mut self) -> Option<RuntimeRowPlayOutcome> {
        let target = self.controller.as_ref()?.queue_blocked_target()?;
        tracing::debug!(item_id = ?target.item_id, reason = ?target.reason, "Play продолжает остановленную очередь");
        Some(self.play_playlist_row(target.item_id))
    }

    /// Блокирующий отказ после admission: автоматический план останавливает очередь.
    pub(super) fn report_playlist_navigation_blocked(
        &mut self,
        request_id: MediaOpenRequestId,
        item_id: playlist_core::PlaylistItemId,
        reason: QueueBlockReason,
    ) {
        let Some(controller) = self.controller.as_mut() else {
            return;
        };
        match controller.report_automatic_target_blocked(request_id, reason) {
            AutomaticTargetBlockedOutcome::Stopped => {}
            AutomaticTargetBlockedOutcome::StaleRequest => {
                // Не автоматический переход, а ручной выбор: прежняя D55-семантика
                // «ждём пользователя», но без бейджа, ведь файл не виноват.
                let mut outcome = controller.report_manual_navigation_target_failure(request_id);
                if matches!(outcome, ManualNavigationFailureOutcome::NotManualNavigation) {
                    outcome = controller.report_unstaged_manual_navigation_target_failure(item_id);
                }
                // B9: ручной отказ из-за сети тоже виден в центре (с «Повторить»).
                if let ManualNavigationFailureOutcome::AwaitingUserAfterFailure {
                    item_id: failed_item_id,
                } = outcome
                {
                    self.manual_open_failure.note(failed_item_id);
                }
            }
        }
        self.discovery.synchronize_navigation_interest(controller);
    }

    /// Блокирующий отказ до admission: план ещё у вызывающего кода и попадает владельцу.
    pub(super) fn report_playlist_target_blocked_before_admission(
        &mut self,
        request_id: MediaOpenRequestId,
        install: PlannedPlaylistInstall,
        reason: QueueBlockReason,
    ) {
        if !install.is_automatic_traversal() {
            self.report_playlist_navigation_blocked(request_id, install.item_id, reason);
            return;
        }
        let Some(controller) = self.controller.as_mut() else {
            return;
        };
        controller.report_unstaged_planned_target_blocked(install, reason);
        self.discovery.synchronize_navigation_interest(controller);
    }
}

#[cfg(test)]
mod tests;
