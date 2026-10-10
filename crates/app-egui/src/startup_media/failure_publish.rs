//! Публикация отказа стартовой подготовки: окно, строка очереди и «Повторить» (B9).
//!
//! Вынесено из `orchestration` без изменения поведения: файл подошёл к лимиту размера, а
//! здесь собрано всё, что происходит в момент отказа.

use std::sync::Arc;

use super::{StartupMediaController, StartupMediaTarget};
use crate::state::RetryPlan;

impl StartupMediaController {
    /// Публикует стартовую ошибку в UI и строку очереди, ничего не записывая в лог.
    ///
    /// Лог — забота вызывающего: для локальных файлов текст для пользователя содержит
    /// имя файла, которое в лог писать нельзя (решение владельца, UX сессия 02).
    /// `row_summary` — короткий текст бейджа строки restored-элемента.
    pub(super) fn publish_preparation_failure(
        &mut self,
        user_message: String,
        row_summary: Arc<str>,
        retry: RetryPlan,
        app_state: &mut crate::state::AppState,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
    ) {
        let target = self.orchestration.target.take();
        self.orchestration.preparation_failed();
        self.startup_error = Some(user_message.clone());
        app_state.report_open_failure(user_message, retry);

        if let Some(StartupMediaTarget::RestoredCurrent(failed)) = target {
            let next = playlist_runtime.report_startup_restore_failure(failed, row_summary);
            if let Some(next) = next {
                self.start_restored_target(next, app_state, playlist_runtime);
            }
        }
    }
}
