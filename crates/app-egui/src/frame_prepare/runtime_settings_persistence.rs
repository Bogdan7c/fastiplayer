//! Покадровая отложенная запись runtime-originated настроек в config.
//!
//! Два независимых источника: ширина sidebar (drag мышью) и громкость пользователя
//! (UX-11: слайдер, M, MPRIS). Оба только запоминают изменение и пишут config после
//! паузы через `SettingsRuntime`; сам frame pipeline не знает про debounce.

use std::time::Instant;

use crate::settings_runtime::SettingsRuntime;
use crate::ui::sidebar::SidebarWidthChange;
use crate::user_audio_level::PlayerAudioObservation;

use super::FrameSettingsRuntimeAdapter;

/// Учитывает изменения кадра и пишет то, у чего пауза уже прошла.
///
/// Возвращает `true`, если попытка записи завершилась и UI нужно перерисовать.
/// Ошибки не глотаются: они попадают в status окна настроек.
pub(super) fn persist_due_runtime_originated_settings(
    settings_runtime: &mut SettingsRuntime,
    sidebar_width_change: Option<SidebarWidthChange>,
    player_audio: PlayerAudioObservation,
    runtime_adapter: &mut FrameSettingsRuntimeAdapter<'_>,
) -> bool {
    let now = Instant::now();
    if let Some(width_change) = sidebar_width_change {
        let _pending_changed = settings_runtime.record_sidebar_width_change(width_change, now);
    }
    let _audio_level_changed = settings_runtime.record_player_audio_observation(player_audio, now);

    let resize_requested_repaint =
        match settings_runtime.flush_due_sidebar_resize(now, runtime_adapter) {
            Ok(outcome) => outcome.needs_redraw(),
            Err(error) => {
                settings_runtime.report_runtime_error("Не удалось сохранить ширину sidebar", error);
                true
            }
        };
    let audio_requested_repaint =
        match settings_runtime.flush_due_user_audio_level(now, runtime_adapter) {
            Ok(outcome) => outcome.needs_redraw(),
            Err(error) => {
                settings_runtime.report_runtime_error("Не удалось сохранить громкость", error);
                true
            }
        };
    resize_requested_repaint || audio_requested_repaint
}
