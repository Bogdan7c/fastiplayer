//! Остановка автоматической очереди на элементе из-за общего обстоятельства (B8).
//!
//! В отличие от `finish_automatic_target_failure` здесь элемент **не** получает
//! runtime-ошибку (бейдж «битый»), не считается пропущенным и план обхода не
//! продолжается: очередь замирает на этом элементе. Controller запоминает элемент, чтобы
//! Play после устранения причины (вернулась сеть) продолжил именно с него, а не заново
//! проиграл доигравший предыдущий.

use playlist_core::PlaylistItemId;

use super::AutomaticStopCause;
use crate::media_open::MediaOpenRequestId;
use crate::playlist_runtime::controller::PlaylistController;
use crate::playlist_runtime::controller::transport::PlannedPlaylistInstall;
use crate::playlist_runtime::queue_block::QueueBlockReason;

/// Элемент, на котором остановилась очередь, и причина остановки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueueBlockedTarget {
    /// Элемент, который не удалось открыть из-за обстоятельств; битым он не помечен.
    pub(crate) item_id: PlaylistItemId,
    /// Что помешало открыть элемент.
    pub(crate) reason: QueueBlockReason,
}

/// Исход отчёта о блокирующем отказе по request ID уже принятого плана.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutomaticTargetBlockedOutcome {
    /// Это был не автоматический переход (ручной выбор или чужой request): состояние не тронуто.
    StaleRequest,
    /// Автоматический переход остановлен на элементе.
    Stopped,
}

impl PlaylistController {
    /// Блокирующий отказ открытия автоматически выбранного элемента (план уже принят).
    pub(crate) fn report_automatic_target_blocked(
        &mut self,
        request_id: MediaOpenRequestId,
        reason: QueueBlockReason,
    ) -> AutomaticTargetBlockedOutcome {
        let request = self
            .take_awaiting_automatic_failure(request_id)
            .or_else(|| self.take_released_automatic_plan(request_id));
        // План обхода дальше не нужен: очередь не продолжается, пока пользователь не решит.
        let Some((item_id, _discarded_plan)) = request else {
            return AutomaticTargetBlockedOutcome::StaleRequest;
        };
        self.stop_queue_at_blocked_target(item_id, reason);
        AutomaticTargetBlockedOutcome::Stopped
    }

    /// Блокирующий отказ до admission: план ещё у вызывающего кода и потребляется здесь.
    ///
    /// Не автоматический план (ручной выбор, резерв) блокировкой очереди не управляет:
    /// такой отказ вызывающий код ведёт прежним ручным маршрутом.
    pub(crate) fn report_unstaged_planned_target_blocked(
        &mut self,
        install: PlannedPlaylistInstall,
        reason: QueueBlockReason,
    ) {
        use crate::playlist_runtime::controller::install::PlaylistInstallMutation;
        if matches!(
            install.mutation,
            PlaylistInstallMutation::AutomaticTraversal(_)
        ) {
            self.stop_queue_at_blocked_target(install.item_id, reason);
        } else {
            self.report_unstaged_manual_navigation_target_failure(install.item_id);
        }
    }

    /// Общий хвост остановки: помечает край как обработанный, запоминает элемент, закрывает
    /// цепочку пропусков итогом «остановлено по обстоятельству».
    fn stop_queue_at_blocked_target(&mut self, item_id: PlaylistItemId, reason: QueueBlockReason) {
        self.mark_current_edge_handled();
        self.automatic_lifecycle.blocked_target = Some(QueueBlockedTarget { item_id, reason });
        tracing::debug!(
            item_id = ?item_id,
            cause = ?AutomaticStopCause::OpenBlocked(reason),
            "automatic playlist lifecycle остановлен обстоятельством, элемент не помечен битым"
        );
        self.finish_automatic_skip_chain_with_stop(AutomaticStopCause::OpenBlocked(reason));
        self.publish_view(false);
    }

    /// Элемент, на котором остановилась очередь и который ещё есть в ней.
    ///
    /// `None` — очередь не заблокирована либо элемент уже удалён из очереди.
    pub(crate) fn queue_blocked_target(&self) -> Option<QueueBlockedTarget> {
        let target = self.automatic_lifecycle.blocked_target?;
        self.queue.item(target.item_id)?;
        Some(target)
    }

    /// Снимает блокировку: пользователь или установка media сменили положение дел.
    pub(super) fn clear_queue_block(&mut self) {
        self.automatic_lifecycle.blocked_target = None;
    }
}
