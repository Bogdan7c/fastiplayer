//! Ссылки, брошенные в окно: исполнение на стороне `AppState`.
//!
//! Оба места броска идут по уже существующим путям владельцев:
//! - панель плейлиста — `PlaylistRuntime::append_dropped_web_url` (тот же вход, что у
//!   кнопки «Добавить URL»: строка прогресса, типизированные причины отказа,
//!   подтверждение сохранения «чувствительной» ссылки);
//! - видео — общая admission замены очереди (`request_queue_replacement_with_intent`:
//!   подтверждение, если очередь не пуста, и/или подтверждение сохранения ссылки), а после
//!   допуска — `replace_queue_with_service_url` и обычный Row Play первой строки.
//!
//! Исходная ссылка в логи и тексты не попадает.

use tracing::warn;

use super::AppState;
use crate::external_open::DropNotice;
use crate::external_open::request::DroppedWebUrl;
use crate::playlist_runtime::{
    DroppedWebUrlAppendOutcome, DroppedWebUrlReplacementStart, PlaylistRuntime,
    ServiceUrlReplacementOutcome,
};
use crate::url_service_adapter::StartupUrlLocator;

impl AppState {
    /// Ссылка на панель плейлиста: добавить ровно так же, как кнопка «Добавить URL».
    pub(super) fn add_dropped_web_url_to_playlist(
        &mut self,
        url: &DroppedWebUrl,
        playlist_runtime: &mut PlaylistRuntime,
    ) {
        let yt_dlp_config = self.yt_dlp_metadata_config();
        match playlist_runtime.append_dropped_web_url(url.as_str(), &yt_dlp_config) {
            DroppedWebUrlAppendOutcome::Accepted => self.mark_pending_worker_redraw(),
            DroppedWebUrlAppendOutcome::Refused { user_message } => {
                self.notify_info(user_message.to_string());
            }
        }
    }

    /// Ссылка на видео: сначала получаем её структуру тем же job-ом, что у «Добавить URL»
    /// (строка прогресса, отмена, причины отказа), затем — admission замены очереди.
    ///
    /// Прямая ссылка на media (без topology) идёт прежним путём: замена очереди одной строкой.
    /// Результат коллекции применяет `apply_resolved_url_collection` (см. `resolved_url_collection`).
    pub(super) fn request_queue_replacement_with_dropped_web_url(
        &mut self,
        url: &DroppedWebUrl,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) {
        // До load decision очереди ещё нет: заменять нечего (как у остальных бросков на видео).
        if playlist_runtime.queue_load_decision_is_pending() {
            self.notify_info(DropNotice::PlaylistStillLoading.text());
            return;
        }
        let yt_dlp_config = self.yt_dlp_metadata_config();
        match playlist_runtime.start_dropped_web_url_replacement(url.as_str(), &yt_dlp_config) {
            DroppedWebUrlReplacementStart::ResolvingTopology => self.mark_pending_worker_redraw(),
            DroppedWebUrlReplacementStart::SingleLink(intent) => {
                self.request_queue_replacement_with_intent(intent, playlist_runtime, renderer);
            }
            DroppedWebUrlReplacementStart::Refused { user_message } => {
                self.notify_info(user_message.to_string());
            }
        }
    }

    /// Допущенная (подтверждённая или при пустой очереди) ссылка: новая очередь + play.
    pub(crate) fn replace_queue_with_admitted_service_url(
        &mut self,
        locator: &StartupUrlLocator,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) {
        let outcome = playlist_runtime.replace_queue_with_service_url(locator);
        self.finish_dropped_url_queue_replacement(outcome, playlist_runtime, renderer);
    }

    /// Итог замены очереди брошенной ссылкой: играем первую строку или сообщаем причину.
    ///
    /// Общий для одной ссылки и для разобранной коллекции: очередь уже заменена владельцем
    /// (или не тронута при отказе), здесь только Row Play и тексты для пользователя.
    pub(super) fn finish_dropped_url_queue_replacement(
        &mut self,
        outcome: ServiceUrlReplacementOutcome,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) {
        match outcome {
            ServiceUrlReplacementOutcome::Replaced { item } => {
                let row_play = playlist_runtime.play_playlist_row(item);
                if !crate::transport_runtime::apply_playlist_row_play(
                    self,
                    playlist_runtime,
                    renderer,
                    row_play,
                ) {
                    self.set_startup_error("Не удалось начать воспроизведение ссылки".to_string());
                }
                self.mark_pending_worker_redraw();
            }
            ServiceUrlReplacementOutcome::LoadDecisionPending => {
                self.notify_info(DropNotice::PlaylistStillLoading.text());
            }
            ServiceUrlReplacementOutcome::InstallInProgress => self.notify_open_still_in_progress(),
            ServiceUrlReplacementOutcome::Rejected => {
                warn!("Замена очереди брошенной ссылкой отклонена владельцем очереди");
                self.notify_info("Не удалось открыть ссылку".to_string());
            }
        }
    }
}
