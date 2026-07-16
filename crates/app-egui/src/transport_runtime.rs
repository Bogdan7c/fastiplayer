//! Единый app adapter typed UI/hotkey transport intents.
//!
//! UI и shell не вычисляют traversal. Adapter задаёт origin `Ui`, вызывает controller boundary,
//! а подготовку/установку передаёт существующему strong media-open protocol.

use player_core::{PlaybackState, PlayerCommand, PlayerSnapshot};
use playlist_core::ManualNavigationDirection;
use render_wgpu_shell::Renderer;
use tracing::warn;

use crate::playlist_runtime::{
    AutomaticLifecycleOutcome, ControllerManualNavigationOutcome,
    PlaylistDiscoveryNavigationAction, PlaylistRuntime, TransportActionOrigin,
};
use crate::state::AppState;
use crate::ui::player_controls::TransportControlAction;

pub(crate) fn apply_transport_actions(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    player_snapshot: &PlayerSnapshot,
    actions: Vec<TransportControlAction>,
) {
    for action in actions {
        apply_transport_action(
            app_state,
            playlist_runtime,
            renderer,
            player_snapshot,
            action,
        );
    }
}

pub(crate) fn apply_transport_action(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    player_snapshot: &PlayerSnapshot,
    action: TransportControlAction,
) {
    match action {
        TransportControlAction::Previous => request_navigation(
            app_state,
            playlist_runtime,
            renderer,
            ManualNavigationDirection::Previous,
            player_snapshot,
        ),
        TransportControlAction::Next => request_navigation(
            app_state,
            playlist_runtime,
            renderer,
            ManualNavigationDirection::Next,
            player_snapshot,
        ),
        TransportControlAction::TogglePlayback => {
            if let Some(dispatch) = playlist_runtime.toggle_ui_stable_transport_intent() {
                if dispatch.exact_current.is_some() || dispatch.pending_update.is_some() {
                    app_state.apply_playlist_stable_intent_dispatch(playlist_runtime, dispatch);
                } else {
                    send_legacy_toggle(app_state);
                }
            } else {
                send_legacy_toggle(app_state);
            }
        }
        TransportControlAction::CancelNavigation => {
            let outcome = playlist_runtime.cancel_global_playlist_navigation_wait();
            tracing::debug!(
                ?outcome,
                "Global playlist wait Cancel обработан runtime owner-ом"
            );
        }
        TransportControlAction::UndoRemoval => {
            let outcome = playlist_runtime.undo_last_removal(std::time::Instant::now());
            tracing::debug!(?outcome, "Global playlist Undo обработан runtime owner-ом");
        }
    }
}

pub(crate) fn apply_discovery_navigation_action(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
) {
    let Some(action) = playlist_runtime.take_playlist_discovery_navigation_action() else {
        return;
    };
    match action {
        PlaylistDiscoveryNavigationAction::Manual(outcome) => {
            apply_manual_navigation_outcome(app_state, playlist_runtime, renderer, outcome);
        }
        PlaylistDiscoveryNavigationAction::Automatic(outcome) => match outcome {
            AutomaticLifecycleOutcome::ReplayCurrent { request } => {
                app_state.dispatch_exact_playlist_transport(request);
            }
            AutomaticLifecycleOutcome::OpenItem { install } => {
                app_state.begin_planned_playlist_install(playlist_runtime, renderer, install, None);
            }
            _ => {}
        },
        PlaylistDiscoveryNavigationAction::ScopeCancelled { .. } => {}
        PlaylistDiscoveryNavigationAction::ScopeFatal { .. } => {
            warn!("Playlist discovery navigation scope завершился fatal outcome");
        }
    }
}

pub(crate) const fn playback_toggle_will_pause(state: PlaybackState) -> bool {
    matches!(
        state,
        PlaybackState::Playing
            | PlaybackState::Buffering
            | PlaybackState::Seeking
            | PlaybackState::Draining
    )
}

fn request_navigation(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    direction: ManualNavigationDirection,
    player_snapshot: &PlayerSnapshot,
) {
    let Some(outcome) = playlist_runtime.request_playlist_navigation(
        direction,
        TransportActionOrigin::Ui,
        player_snapshot.current_position,
    ) else {
        return;
    };
    apply_manual_navigation_outcome(app_state, playlist_runtime, renderer, outcome);
}

fn apply_manual_navigation_outcome(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    outcome: ControllerManualNavigationOutcome,
) {
    match outcome {
        ControllerManualNavigationOutcome::RestartCurrent { request } => {
            app_state.dispatch_exact_playlist_transport(request);
        }
        ControllerManualNavigationOutcome::StartInstall { install } => {
            app_state.begin_planned_playlist_install(playlist_runtime, renderer, install, None);
        }
        ControllerManualNavigationOutcome::SupersedeInstall {
            expected_request_id,
            cause,
            install,
        } => app_state.supersede_planned_playlist_install(
            playlist_runtime,
            expected_request_id,
            cause,
            install,
        ),
        ControllerManualNavigationOutcome::AbortedBeforeDispatch {
            request_id,
            cause,
            next,
            ..
        } => app_state.replace_aborted_playlist_install(playlist_runtime, request_id, cause, next),
        ControllerManualNavigationOutcome::PreviewInvalidated(_)
        | ControllerManualNavigationOutcome::Waiting { .. }
        | ControllerManualNavigationOutcome::NoItem(_)
        | ControllerManualNavigationOutcome::StaleWait { .. }
        | ControllerManualNavigationOutcome::Guarded(_)
        | ControllerManualNavigationOutcome::IntentRevisionExhausted => {}
    }
}

fn send_legacy_toggle(app_state: &mut AppState) {
    if let Err(error) = app_state
        .player_worker
        .try_send_command(PlayerCommand::TogglePlayback)
    {
        warn!(error = %error, "Не удалось переключить playback");
    } else {
        app_state.mark_pending_worker_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_parity_matches_existing_active_and_inactive_player_states() {
        for state in [
            PlaybackState::Playing,
            PlaybackState::Buffering,
            PlaybackState::Seeking,
            PlaybackState::Draining,
        ] {
            assert!(playback_toggle_will_pause(state));
        }
        for state in [
            PlaybackState::Idle,
            PlaybackState::Opening,
            PlaybackState::Paused,
            PlaybackState::Scrubbing,
            PlaybackState::Ended,
            PlaybackState::Stopped,
            PlaybackState::Failed,
        ] {
            assert!(!playback_toggle_will_pause(state));
        }
    }
}
