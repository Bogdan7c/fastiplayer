//! Сохранение runtime-originated настроек перед suspend и terminal exit.
//!
//! Ширина sidebar, громкость пользователя (UX-11) и выбор «субтитры включены» пишутся в config с паузой.
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
                || self
                    .settings_runtime
                    .next_subtitle_enabled_persist_deadline()
                    .is_some()
            {
                tracing::error!(
                    "Deferred settings reached a lifecycle boundary without an active AppState"
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

        // Переключение субтитров между последним кадром и выходом тоже не должно теряться.
        let _subtitles_choice_changed = self.settings_runtime.record_subtitle_enabled_change(
            app_state.take_subtitle_enabled_preference_change(),
            Instant::now(),
        );

        let settings_runtime = &mut self.settings_runtime;
        let (sidebar_result, audio_result, subtitles_result) = with_lifecycle_settings_adapter(
            window,
            renderer,
            app_state,
            &mut self.playlist_runtime,
            &mut self.renderer_lifecycle,
            |runtime_adapter| {
                // Незавершённое применение настроек отменяется первым: оно не должно ни
                // записать config, ни мешать runtime-commit-ам ниже (C2).
                settings_runtime.cancel_pending_apply(runtime_adapter);
                // Ошибка sidebar не должна мешать сохранить громкость, и наоборот.
                let sidebar_result = settings_runtime.flush_pending_sidebar_resize(runtime_adapter);
                let audio_result = settings_runtime.flush_pending_user_audio_level(runtime_adapter);
                let subtitles_result =
                    settings_runtime.flush_pending_subtitle_enabled(runtime_adapter);
                (sidebar_result, audio_result, subtitles_result)
            },
        );

        if let Err(error) = sidebar_result {
            self.settings_runtime
                .report_runtime_error("Could not save sidebar width before suspend/exit", error);
        }
        if let Err(error) = audio_result {
            self.settings_runtime
                .report_runtime_error("Could not save volume before suspend/exit", error);
        }
        if let Err(error) = subtitles_result {
            self.settings_runtime
                .report_runtime_error("Could not save subtitles choice before suspend/exit", error);
        }
    }
}
