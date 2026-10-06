//! Сохранение runtime-originated настроек перед suspend и terminal exit.
//!
//! Ширина sidebar и громкость пользователя (UX-11) пишутся в config с паузой.
//! Перед уничтожением renderer-bound `AppState` отложенные значения нужно записать
//! сразу, иначе изменения последних секунд потеряются.

use std::time::Instant;

use crate::frame_prepare::with_lifecycle_settings_adapter;
use crate::user_audio_level::PlayerAudioObservation;

use super::AppShell;

impl AppShell {
    /// Записывает отложенные ширину sidebar и громкость, пока `AppState` ещё жив.
    pub(super) fn flush_runtime_settings_for_lifecycle_boundary(&mut self) {
        let (Some(window), Some(renderer), Some(app_state)) = (
            self.window.as_ref(),
            self.renderer.as_mut(),
            self.app_state.as_mut(),
        ) else {
            if self
                .settings_runtime
                .next_sidebar_resize_deadline()
                .is_some()
                || self
                    .settings_runtime
                    .next_user_audio_level_persist_deadline()
                    .is_some()
            {
                tracing::error!(
                    "Отложенные настройки дошли до lifecycle boundary без активного AppState"
                );
            }
            return;
        };

        // Последнее действие (например, M прямо перед закрытием) могло ещё не попасть
        // в snapshot кадра: берём свежий snapshot прямо у worker-а.
        let final_audio_observation =
            PlayerAudioObservation::from_player_snapshot(&app_state.refresh_player_snapshot());
        let _level_changed = self
            .settings_runtime
            .record_player_audio_observation(final_audio_observation, Instant::now());

        let settings_runtime = &mut self.settings_runtime;
        let (sidebar_result, audio_result) = with_lifecycle_settings_adapter(
            window,
            renderer,
            app_state,
            &mut self.playlist_runtime,
            &mut self.renderer_lifecycle,
            |runtime_adapter| {
                // Ошибка sidebar не должна мешать сохранить громкость, и наоборот.
                let sidebar_result = settings_runtime.flush_pending_sidebar_resize(runtime_adapter);
                let audio_result = settings_runtime.flush_pending_user_audio_level(runtime_adapter);
                (sidebar_result, audio_result)
            },
        );

        if let Err(error) = sidebar_result {
            self.settings_runtime.report_runtime_error(
                "Не удалось сохранить ширину sidebar перед suspend/exit",
                error,
            );
        }
        if let Err(error) = audio_result {
            self.settings_runtime
                .report_runtime_error("Не удалось сохранить громкость перед suspend/exit", error);
        }
    }
}
