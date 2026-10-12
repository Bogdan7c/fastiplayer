//! Шаговое изменение громкости: колесо над видео и клавиши `↑`/`↓` (UX edge cases 2, B3).
//!
//! Владелец громкости остаётся прежним: это player (`PlayerCommand::SetVolume` /
//! `ToggleMute`). Шаги не пишут ни в config, ни в какое-то своё состояние: команда
//! уходит тем же путём, что слайдер и клавиша `M`, player меняет snapshot, а
//! наблюдатель `SettingsRuntime::record_player_audio_observation` (UX 11) сам
//! откладывает запись громкости/mute в `config.toml`. Поэтому сохранение и поведение
//! mute совпадают со слайдером без отдельного кода.

use player_core::{PlayerCommand, PlayerSnapshot};
use tracing::warn;

use super::AppState;

/// Шаг громкости одного «щелчка» колеса или нажатия стрелки, в тысячных (5 %).
pub(crate) const VOLUME_STEP_PERMILLE: i32 = 50;

/// Полная громкость в тех же тысячных.
const FULL_VOLUME_PERMILLE: i32 = 1_000;

/// Превращает N шагов в команду player-у или `None`, если делать нечего.
///
/// Правила:
/// - звук слышен: новая громкость = текущая ± шаги, зажатая в `0..=100 %`; если она
///   не изменилась (упёрлись в границу) — команды нет, очередь worker-а не засоряется;
///   спуск до нуля даёт `SetVolume(0.0)`, то есть mute — как у слайдера;
/// - звук выключен и шаг вверх: `ToggleMute` — player сам вернёт громкость «до mute»
///   (запасное значение — сохранённая слышимая громкость), как по клику на значок;
/// - звук выключен и шаг вниз: ничего, тише тишины не бывает.
#[must_use]
pub(super) fn volume_command_from_steps(
    player_snapshot: &PlayerSnapshot,
    fallback_audible_volume: f32,
    step_count: i32,
) -> Option<PlayerCommand> {
    if step_count == 0 {
        return None;
    }
    let is_silent = player_snapshot.muted || player_snapshot.volume <= 0.0;
    if is_silent {
        return (step_count > 0).then_some(PlayerCommand::ToggleMute {
            fallback_volume: fallback_audible_volume,
        });
    }

    let current_permille = (player_snapshot.volume * FULL_VOLUME_PERMILLE as f32).round() as i32;
    let target_permille = current_permille
        .saturating_add(step_count.saturating_mul(VOLUME_STEP_PERMILLE))
        .clamp(0, FULL_VOLUME_PERMILLE);
    if target_permille == current_permille {
        return None;
    }
    Some(PlayerCommand::SetVolume(
        target_permille as f32 / FULL_VOLUME_PERMILLE as f32,
    ))
}

impl AppState {
    /// Применяет шаги громкости из UI (колесо над видео, стрелки).
    pub(super) fn apply_volume_steps(&mut self, player_snapshot: &PlayerSnapshot, step_count: i32) {
        let Some(command) = volume_command_from_steps(
            player_snapshot,
            self.committed_config_snapshot.remembered_audible_volume(),
            step_count,
        ) else {
            return;
        };
        if let Err(error) = self.player_worker.try_send_command(command) {
            warn!(error = %error, "Failed to change the volume with a step from the UI");
        } else {
            self.mark_pending_worker_redraw();
        }
    }
}

#[cfg(test)]
mod tests;
