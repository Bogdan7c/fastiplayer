//! Play, когда в плеере ничего не играет, а очередь может продолжиться (B8, A15-live).
//!
//! Вынесено из `transport_runtime`: там решается только «какое действие пришло», а здесь —
//! «есть ли элемент очереди, который Play обязан открыть вместо переключения паузы».

use player_core::PlaybackState;
use render_wgpu_shell::Renderer;
use tracing::warn;

use super::apply_playlist_row_play;
use crate::playlist_runtime::PlaylistRuntime;
use crate::state::AppState;

/// Продолжать остановленную очередь можно, только пока ничего не играет и не открывается.
///
/// Если пользователь успел перемотать доигравший файл или запустить другое, Play управляет
/// тем, что играет сейчас, а не прыгает на остановленный элемент.
const fn is_playback_idle_for_queue_resume(state: PlaybackState) -> bool {
    matches!(
        state,
        PlaybackState::Ended | PlaybackState::Idle | PlaybackState::Stopped
    )
}

/// Пробует вести Play через очередь; `true` — Play обработан, переключать паузу не нужно.
///
/// Порядок: (B8) очередь стоит на элементе, который не открылся из-за сети, — продолжаем
/// с него, а не перезапускаем доигравший; (A15-live) плеер пуст, очередь нет — открываем
/// `current` либо первый элемент. Во втором случае player не получает «Play без источника».
pub(super) fn resume_queue_from_idle(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    player_state: PlaybackState,
) -> bool {
    if is_playback_idle_for_queue_resume(player_state)
        && let Some(outcome) = playlist_runtime.play_queue_blocked_item()
    {
        if !apply_playlist_row_play(app_state, playlist_runtime, renderer, outcome) {
            warn!("Play failed to continue the queue with the stopped element");
        }
        return true;
    }
    if let Some(outcome) = playlist_runtime.play_idle_queue_target() {
        if !apply_playlist_row_play(app_state, playlist_runtime, renderer, outcome) {
            warn!("Play failed to open the queue element");
        }
        return true;
    }
    false
}
