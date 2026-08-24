//! Единый app adapter typed UI/hotkey transport intents.
//!
//! UI и shell не вычисляют traversal. Adapter задаёт origin `Ui`, вызывает controller boundary,
//! а подготовку/установку передаёт существующему strong media-open protocol.

use desktop_integration::{
    DesktopCommand, DesktopLoopStatus, DesktopTimelineSeekOutcome, DesktopTransportAction,
    TimelineSeekRequestId as DesktopTimelineSeekRequestId,
};
use media_core::{MediaDuration, MediaTime};
use player_core::{
    ExactTimelineSeekRequest, PlaybackState, PlayerCommand, PlayerSnapshot, TimelineSeekKind,
    TimelineSeekRequestId,
};
use playlist_core::ManualNavigationDirection;
use render_wgpu_shell::Renderer;
use tracing::warn;

use crate::playlist_runtime::{
    AutomaticLifecycleOutcome, ControllerInitialQueuePlaybackAction,
    ControllerManualNavigationOutcome, ControllerPlayItemOutcome,
    PlaylistDiscoveryNavigationAction, PlaylistRuntime, RuntimeRowPlayOutcome,
    TransportActionOrigin,
};
use crate::state::AppState;
use crate::ui::player_controls::TransportControlAction;

/// Row Play использует тот же strong install/exact transport adapter, что и main controls.
pub(crate) fn apply_playlist_row_play(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    outcome: RuntimeRowPlayOutcome,
) -> bool {
    let RuntimeRowPlayOutcome::Controller(outcome) = outcome else {
        return false;
    };
    match outcome {
        ControllerPlayItemOutcome::RestartActive { request, .. } => {
            app_state.dispatch_exact_playlist_transport(request);
            true
        }
        ControllerPlayItemOutcome::CoalescePending {
            intent_dispatch, ..
        } => {
            app_state.apply_playlist_stable_intent_dispatch(playlist_runtime, intent_dispatch);
            true
        }
        ControllerPlayItemOutcome::StartInstall {
            install,
            intent_dispatch,
        } => {
            app_state.apply_playlist_stable_intent_dispatch(playlist_runtime, intent_dispatch);
            app_state.begin_planned_playlist_install(playlist_runtime, renderer, install, None);
            true
        }
        ControllerPlayItemOutcome::Guarded {
            intent_dispatch, ..
        } => {
            app_state.apply_playlist_stable_intent_dispatch(playlist_runtime, intent_dispatch);
            true
        }
        ControllerPlayItemOutcome::ItemNotCommitted { .. }
        | ControllerPlayItemOutcome::IntentRevisionExhausted => false,
    }
}

/// Применяет deferred start новой directory queue через те же exact/strong adapters, что Row Play.
pub(crate) fn apply_initial_queue_playback_action(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
) -> bool {
    let Some(action) = playlist_runtime.take_initial_queue_playback_action() else {
        return false;
    };
    match action {
        ControllerInitialQueuePlaybackAction::RestartCurrent {
            request,
            intent_dispatch,
        } => {
            app_state.apply_playlist_stable_intent_dispatch(playlist_runtime, intent_dispatch);
            app_state.dispatch_exact_playlist_transport(request);
        }
        ControllerInitialQueuePlaybackAction::InstallFirst {
            install,
            intent_dispatch,
        } => {
            app_state.apply_playlist_stable_intent_dispatch(playlist_runtime, intent_dispatch);
            app_state.begin_planned_playlist_install(playlist_runtime, renderer, install, None);
        }
    }
    true
}

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
            TransportActionOrigin::Ui,
        ),
        TransportControlAction::Next => request_navigation(
            app_state,
            playlist_runtime,
            renderer,
            ManualNavigationDirection::Next,
            player_snapshot,
            TransportActionOrigin::Ui,
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
        TransportControlAction::SetShuffleEnabled { enabled } => {
            if playlist_runtime
                .record_startup_shuffle_enabled(enabled)
                .is_err()
            {
                playlist_runtime.set_playlist_safe_feedback("Не удалось изменить перемешивание");
            }
        }
        TransportControlAction::SetRepeatMode { mode } => {
            if playlist_runtime.record_startup_repeat_mode(mode).is_err() {
                playlist_runtime.set_playlist_safe_feedback("Не удалось изменить режим повтора");
            }
        }
        TransportControlAction::CancelNavigation => {
            let outcome = playlist_runtime.cancel_global_playlist_navigation_wait();
            tracing::debug!(
                ?outcome,
                "Global playlist wait Cancel обработан runtime owner-ом"
            );
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
        PlaylistDiscoveryNavigationAction::Automatic(outcome) => {
            apply_automatic_lifecycle_outcome(app_state, playlist_runtime, renderer, outcome);
        }
        PlaylistDiscoveryNavigationAction::ScopeCancelled { .. } => {}
        PlaylistDiscoveryNavigationAction::ScopeFatal { .. } => {
            warn!("Playlist discovery navigation scope завершился fatal outcome");
        }
    }
}

/// Передаёт один exact player snapshot controller-у и исполняет возникший EOF action.
pub(crate) fn apply_playlist_automatic_snapshot(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    player_snapshot: &PlayerSnapshot,
) {
    let Some(binding) = app_state.playlist_runtime_binding() else {
        return;
    };
    playlist_runtime.observe_resume_checkpoint_snapshot(binding, player_snapshot);
    app_state.drive_next_item_preload(playlist_runtime, binding, player_snapshot);
    let Some(outcome) =
        playlist_runtime.observe_playlist_automatic_snapshot(binding, player_snapshot)
    else {
        return;
    };
    apply_automatic_lifecycle_outcome(app_state, playlist_runtime, renderer, outcome);
}

/// Оба источника automatic action — непосредственный EOF и deferred discovery readiness —
/// используют один strong-install/exact-replay executor.
fn apply_automatic_lifecycle_outcome(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    outcome: AutomaticLifecycleOutcome,
) {
    match outcome {
        AutomaticLifecycleOutcome::ReplayCurrent { request } => {
            app_state.dispatch_exact_playlist_transport(request);
        }
        AutomaticLifecycleOutcome::OpenItem { install } => {
            app_state.begin_planned_playlist_install(playlist_runtime, renderer, install, None);
        }
        AutomaticLifecycleOutcome::NoAction
        | AutomaticLifecycleOutcome::StaleObservation
        | AutomaticLifecycleOutcome::HeldForExplicitIntent { .. }
        | AutomaticLifecycleOutcome::Deferred { .. }
        | AutomaticLifecycleOutcome::Stop { .. } => {}
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

pub(crate) fn request_navigation(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    direction: ManualNavigationDirection,
    player_snapshot: &PlayerSnapshot,
    origin: TransportActionOrigin,
) {
    let Some(outcome) = playlist_runtime.request_playlist_navigation(
        direction,
        origin,
        player_snapshot.current_position,
    ) else {
        return;
    };
    apply_manual_navigation_outcome(app_state, playlist_runtime, renderer, outcome);
}

/// Drain-ит neutral MPRIS mailbox только на app UI thread.
pub(crate) fn apply_desktop_commands(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    player_snapshot: &PlayerSnapshot,
    commands: Vec<DesktopCommand>,
) -> bool {
    let mut visible_change = false;
    for command in commands {
        let player_dependent = matches!(
            &command.action,
            DesktopTransportAction::Next
                | DesktopTransportAction::Previous
                | DesktopTransportAction::Play
                | DesktopTransportAction::Pause
                | DesktopTransportAction::PlayPause
                | DesktopTransportAction::Seek { .. }
                | DesktopTransportAction::SetPosition { .. }
                | DesktopTransportAction::SetRatePause
        );
        if player_dependent
            && !playlist_runtime.desktop_control_binding_matches(
                command.observed_control_revision,
                app_state.playlist_runtime_binding(),
                player_snapshot,
            )
        {
            match &command.action {
                DesktopTransportAction::Seek { request_id, .. }
                | DesktopTransportAction::SetPosition { request_id, .. } => {
                    playlist_runtime.record_desktop_seek_outcome(
                        DesktopTimelineSeekOutcome::StaleInstance {
                            request_id: *request_id,
                        },
                    );
                }
                _ => {}
            }
            continue;
        }
        let action = command.action;
        match action {
            DesktopTransportAction::Next => request_navigation(
                app_state,
                playlist_runtime,
                renderer,
                ManualNavigationDirection::Next,
                player_snapshot,
                TransportActionOrigin::Mpris,
            ),
            DesktopTransportAction::Previous => request_navigation(
                app_state,
                playlist_runtime,
                renderer,
                ManualNavigationDirection::Previous,
                player_snapshot,
                TransportActionOrigin::Mpris,
            ),
            DesktopTransportAction::Play => apply_desktop_intent(
                app_state,
                playlist_runtime,
                crate::playlist_runtime::StablePlaybackIntent::Playing,
            ),
            DesktopTransportAction::Pause | DesktopTransportAction::SetRatePause => {
                apply_desktop_intent(
                    app_state,
                    playlist_runtime,
                    crate::playlist_runtime::StablePlaybackIntent::Paused,
                );
            }
            DesktopTransportAction::PlayPause => {
                if let Some(dispatch) = playlist_runtime.toggle_desktop_playback_intent() {
                    app_state.apply_playlist_stable_intent_dispatch(playlist_runtime, dispatch);
                }
            }
            DesktopTransportAction::Stop => match playlist_runtime.request_desktop_stop() {
                Some(Ok(request)) => app_state.dispatch_exact_playlist_transport(request),
                Some(Err(outcome)) => {
                    tracing::debug!(?outcome, "MPRIS Stop сохранён controller guard-ом")
                }
                None => {}
            },
            DesktopTransportAction::SetLoopStatus(status) => {
                let repeat_mode = match status {
                    DesktopLoopStatus::None => playlist_core::RepeatMode::StopAtEnd,
                    DesktopLoopStatus::Track => playlist_core::RepeatMode::RepeatOne,
                    DesktopLoopStatus::Playlist => playlist_core::RepeatMode::RepeatQueue,
                };
                match playlist_runtime.record_startup_repeat_mode(repeat_mode) {
                    Ok(changed) => visible_change |= changed,
                    Err(error) => warn!(?error, "MPRIS LoopStatus mutation отклонена"),
                }
            }
            DesktopTransportAction::SetShuffle(enabled) => {
                match playlist_runtime.record_startup_shuffle_enabled(enabled) {
                    Ok(changed) => visible_change |= changed,
                    Err(error) => warn!(?error, "MPRIS Shuffle mutation отклонена"),
                }
            }
            DesktopTransportAction::SetVolume(volume) => {
                if playlist_runtime.set_desktop_effective_volume(volume) {
                    visible_change = true;
                    if let Err(error) = app_state
                        .player_command_sender()
                        .try_send(PlayerCommand::SetVolume(volume.as_player()))
                    {
                        warn!(error = %error, "MPRIS Volume не принят active player binding");
                    }
                }
            }
            DesktopTransportAction::Seek {
                request_id,
                offset_microseconds,
            } => {
                apply_relative_desktop_seek(
                    app_state,
                    playlist_runtime,
                    renderer,
                    player_snapshot,
                    request_id,
                    offset_microseconds,
                );
            }
            DesktopTransportAction::SetPosition {
                request_id,
                track_key,
                position_microseconds,
            } => {
                if !playlist_runtime.desktop_track_matches(track_key) {
                    playlist_runtime.record_desktop_seek_outcome(
                        DesktopTimelineSeekOutcome::StaleTrack { request_id },
                    );
                    continue;
                }
                let Some(media_instance_id) = player_snapshot.media_instance_id else {
                    playlist_runtime.record_desktop_seek_outcome(
                        DesktopTimelineSeekOutcome::StaleInstance { request_id },
                    );
                    continue;
                };
                let Ok(microseconds) = u64::try_from(position_microseconds) else {
                    playlist_runtime.record_desktop_seek_outcome(
                        DesktopTimelineSeekOutcome::InvalidRange { request_id },
                    );
                    continue;
                };
                let target =
                    MediaTime::from_duration(std::time::Duration::from_micros(microseconds));
                app_state.dispatch_exact_timeline_seek(ExactTimelineSeekRequest {
                    request_id: player_seek_request_id(request_id),
                    media_instance_id,
                    target,
                    kind: TimelineSeekKind::SetPosition,
                });
            }
        }
    }
    visible_change
}

fn apply_desktop_intent(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    intent: crate::playlist_runtime::StablePlaybackIntent,
) {
    if let Some(dispatch) = playlist_runtime.record_desktop_playback_intent(intent) {
        app_state.apply_playlist_stable_intent_dispatch(playlist_runtime, dispatch);
    }
}

fn apply_relative_desktop_seek(
    app_state: &mut AppState,
    playlist_runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    snapshot: &PlayerSnapshot,
    request_id: DesktopTimelineSeekRequestId,
    offset_microseconds: i64,
) {
    match resolve_relative_desktop_seek(
        snapshot.timeline.current_position,
        snapshot.timeline.duration,
        offset_microseconds,
    ) {
        RelativeSeekResolution::BeyondEnd => {
            playlist_runtime
                .record_desktop_seek_outcome(DesktopTimelineSeekOutcome::BeyondEnd { request_id });
            request_navigation(
                app_state,
                playlist_runtime,
                renderer,
                ManualNavigationDirection::Next,
                snapshot,
                TransportActionOrigin::Mpris,
            );
        }
        RelativeSeekResolution::ArithmeticOverflow => {
            playlist_runtime.record_desktop_seek_outcome(
                DesktopTimelineSeekOutcome::ArithmeticOverflow { request_id },
            );
        }
        RelativeSeekResolution::InTrack(target) => {
            let Some(media_instance_id) = snapshot.media_instance_id else {
                playlist_runtime.record_desktop_seek_outcome(
                    DesktopTimelineSeekOutcome::StaleInstance { request_id },
                );
                return;
            };
            app_state.dispatch_exact_timeline_seek(ExactTimelineSeekRequest {
                request_id: player_seek_request_id(request_id),
                media_instance_id,
                target,
                kind: TimelineSeekKind::Relative,
            });
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RelativeSeekResolution {
    InTrack(MediaTime),
    BeyondEnd,
    ArithmeticOverflow,
}

/// Разрешает signed relative seek отдельно от SetPosition range policy.
fn resolve_relative_desktop_seek(
    current_position: MediaTime,
    known_duration: Option<MediaDuration>,
    offset_microseconds: i64,
) -> RelativeSeekResolution {
    let Ok(current_microseconds) = i128::try_from(current_position.as_duration().as_micros())
    else {
        return if known_duration.is_some() && offset_microseconds > 0 {
            RelativeSeekResolution::BeyondEnd
        } else {
            RelativeSeekResolution::ArithmeticOverflow
        };
    };
    let Some(target_microseconds) =
        current_microseconds.checked_add(i128::from(offset_microseconds))
    else {
        return if known_duration.is_some() && offset_microseconds > 0 {
            RelativeSeekResolution::BeyondEnd
        } else {
            RelativeSeekResolution::ArithmeticOverflow
        };
    };
    let target_microseconds = target_microseconds.max(0);
    if known_duration.is_some_and(|duration| {
        i128::try_from(duration.as_duration().as_micros())
            .is_ok_and(|duration_microseconds| target_microseconds > duration_microseconds)
    }) {
        return RelativeSeekResolution::BeyondEnd;
    }
    let Ok(target_microseconds) = u64::try_from(target_microseconds) else {
        return if known_duration.is_some() && offset_microseconds > 0 {
            RelativeSeekResolution::BeyondEnd
        } else {
            RelativeSeekResolution::ArithmeticOverflow
        };
    };
    RelativeSeekResolution::InTrack(MediaTime::from_duration(std::time::Duration::from_micros(
        target_microseconds,
    )))
}

fn player_seek_request_id(request_id: DesktopTimelineSeekRequestId) -> TimelineSeekRequestId {
    TimelineSeekRequestId::new(
        std::num::NonZeroU64::new(request_id.get())
            .expect("desktop timeline request IDs are non-zero"),
    )
}

pub(crate) fn apply_manual_navigation_outcome(
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

    #[test]
    fn relative_seek_clamps_underflow_and_accepts_equal_known_end() {
        assert_eq!(
            resolve_relative_desktop_seek(MediaTime::from_secs(2), None, -3_000_000),
            RelativeSeekResolution::InTrack(MediaTime::ZERO)
        );
        assert_eq!(
            resolve_relative_desktop_seek(
                MediaTime::from_secs(2),
                Some(MediaDuration::from_secs(3)),
                1_000_000,
            ),
            RelativeSeekResolution::InTrack(MediaTime::from_secs(3))
        );
    }

    #[test]
    fn relative_seek_separates_known_beyond_end_from_unknown_overflow() {
        assert_eq!(
            resolve_relative_desktop_seek(
                MediaTime::from_secs(2),
                Some(MediaDuration::from_secs(3)),
                1_000_001,
            ),
            RelativeSeekResolution::BeyondEnd
        );
        assert_eq!(
            resolve_relative_desktop_seek(
                MediaTime::from_duration(std::time::Duration::MAX),
                None,
                1,
            ),
            RelativeSeekResolution::ArithmeticOverflow
        );
    }
}
