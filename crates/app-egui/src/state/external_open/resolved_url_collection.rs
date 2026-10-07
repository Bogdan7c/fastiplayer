//! Разобранная коллекция ссылки, брошенной на видео: исполнение на стороне `AppState`.
//!
//! Структуру ссылки получает `PlaylistRuntime` (job «Добавить URL» с назначением
//! `ReplaceAfterConfirmation`); здесь — решение «что показать и какой существующий путь вызвать»:
//!
//! - в ссылке не нашлось роликов → уведомление, очередь не тронута;
//! - пока шёл разбор, началось другое открытие/импорт → результат отбрасывается с
//!   «Файл ещё открывается» (как у обхода папки);
//! - иначе общий admission замены очереди: пустая очередь заменяется сразу, непустая требует
//!   подтверждения с подписью «N роликов с домена» (очередь не меняется до ответа);
//! - после допуска очередь заменяется записями коллекции в исходном порядке и играет первая.
//!
//! Отмена кнопкой «Отменить» и отказ сайта сюда не доходят: они завершаются во владельце
//! URL-импорта прежней обратной связью Add URL, очередь не трогается.

use tracing::{info, warn};

use super::super::AppState;
use crate::playlist_runtime::{
    AdmittedResolvedUrlCollection, InAppQueueReplacementIntent, PlaylistRuntime,
    ResolvedUrlCollection,
};

/// Текст, когда структура ссылки разобрана, но проигрываемых роликов в ней нет.
const NO_CLIPS_IN_LINK_MESSAGE: &str = "По ссылке не нашлось роликов";

impl AppState {
    /// Применяет разобранную коллекцию; вызывается UI-потоком по wake владельца.
    pub(crate) fn apply_resolved_url_collection(
        &mut self,
        collection: ResolvedUrlCollection,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) {
        // В логе только число: сама ссылка и адреса роликов приватны.
        info!(
            item_count = collection.item_count(),
            "Структура брошенной ссылки получена"
        );
        if collection.is_empty() {
            self.notify_info(NO_CLIPS_IN_LINK_MESSAGE.to_string());
            return;
        }
        // Устаревание: пока шёл разбор, пользователь мог начать другое открытие.
        if self.has_pending_local_file_open() || playlist_runtime.has_playlist_import_in_flight() {
            self.notify_open_still_in_progress();
            return;
        }
        let intent = InAppQueueReplacementIntent::resolved_url_collection(collection);
        self.request_queue_replacement_with_intent(intent, playlist_runtime, renderer);
    }

    /// Допущенная (подтверждённая или при пустой очереди) коллекция: новая очередь + play.
    pub(crate) fn replace_queue_with_admitted_resolved_url_collection(
        &mut self,
        admitted: AdmittedResolvedUrlCollection,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) {
        let outcome = playlist_runtime.replace_queue_with_resolved_url_collection(admitted);
        if matches!(
            outcome,
            crate::playlist_runtime::ServiceUrlReplacementOutcome::Rejected
        ) {
            warn!("Замена очереди коллекцией по ссылке отклонена владельцем очереди");
        }
        self.finish_dropped_url_queue_replacement(outcome, playlist_runtime, renderer);
    }
}
