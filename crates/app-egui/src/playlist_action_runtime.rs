//! Post-render adapter typed Playlist actions к authoritative runtime owners.

use std::time::Instant;

use render_wgpu_shell::Renderer;
use winit::window::Window;

use crate::playlist_runtime::{
    ControllerMoveItemOutcome, MetadataSortCancelOutcome, PlaylistProgressCancelScope,
    PlaylistRuntime, RuntimeMoveItemOutcome, RuntimeRemovalOutcome, StopAfterCurrentOutcome,
    TransportActionOrigin,
};
use crate::state::AppState;
use crate::ui::playlist::PlaylistAction;

/// Возвращает `true`, если действие изменило видимый runtime/UI state.
pub(crate) fn apply_playlist_actions(
    window: &Window,
    app_state: &mut AppState,
    runtime: &mut PlaylistRuntime,
    renderer: &Renderer,
    actions: Vec<PlaylistAction>,
) -> bool {
    let mut changed = false;
    for action in actions {
        match action {
            PlaylistAction::Select(item_id) => {
                changed |= runtime.select_playlist_row(Some(item_id));
            }
            PlaylistAction::Play(item_id) => {
                let outcome = runtime.play_playlist_row(item_id);
                let applied = crate::transport_runtime::apply_playlist_row_play(
                    app_state, runtime, renderer, outcome,
                );
                if !applied {
                    runtime.set_playlist_safe_feedback("Не удалось запустить выбранный элемент");
                }
                changed = true;
            }
            PlaylistAction::Remove(item_id) => {
                let outcome = runtime.remove_playlist_item(item_id, Instant::now());
                changed |= apply_removal_outcome(app_state, runtime, outcome, "удалить элемент");
            }
            PlaylistAction::RemoveOthers(item_id) => {
                let outcome = runtime.remove_other_playlist_items(item_id, Instant::now());
                changed |= apply_removal_outcome(
                    app_state,
                    runtime,
                    outcome,
                    "удалить остальные элементы",
                );
            }
            PlaylistAction::Move { item_id, intent } => {
                let outcome = runtime.move_playlist_item(item_id, intent);
                changed |= matches!(
                    outcome,
                    RuntimeMoveItemOutcome::Controller(ControllerMoveItemOutcome::Moved { .. })
                );
                if !matches!(
                    outcome,
                    RuntimeMoveItemOutcome::Controller(
                        ControllerMoveItemOutcome::Moved { .. }
                            | ControllerMoveItemOutcome::AlreadyInPlace
                    )
                ) {
                    runtime.set_playlist_safe_feedback("Не удалось изменить порядок плейлиста");
                    changed = true;
                }
            }
            PlaylistAction::AddFiles => changed |= runtime.start_playlist_file_dialog(window),
            PlaylistAction::OpenUrlEditor => {
                runtime.open_playlist_url_editor();
                changed = true;
            }
            PlaylistAction::UpdateUrlDraft(text) => {
                runtime.update_playlist_url_draft(text.into_inner());
                changed = true;
            }
            PlaylistAction::SubmitUrl => changed |= runtime.submit_playlist_url_draft(),
            PlaylistAction::CancelUrlEditor => {
                changed |= runtime.cancel_playlist_url_editor();
            }
            PlaylistAction::Clear => {
                if runtime.playlist_interaction_model().item_count == 0 {
                    continue;
                }
                let outcome = runtime.clear_playlist(Instant::now());
                changed |= !matches!(outcome, RuntimeRemovalOutcome::NoChange);
                if matches!(
                    outcome,
                    RuntimeRemovalOutcome::FatalInvariant
                        | RuntimeRemovalOutcome::DirtyRevisionExhausted
                        | RuntimeRemovalOutcome::StructuralRevisionExhausted
                        | RuntimeRemovalOutcome::DomainRevisionExhausted
                        | RuntimeRemovalOutcome::DeadlineOverflow
                        | RuntimeRemovalOutcome::LoadDecisionPending
                ) {
                    runtime.set_playlist_safe_feedback("Не удалось очистить плейлист");
                    changed = true;
                }
            }
            PlaylistAction::SetRepeatMode(mode) => match runtime.record_startup_repeat_mode(mode) {
                Ok(mode_changed) => changed |= mode_changed,
                Err(_) => {
                    runtime.set_playlist_safe_feedback("Не удалось изменить режим повтора");
                    changed = true;
                }
            },
            PlaylistAction::SetShuffle(enabled) => {
                match runtime.record_startup_shuffle_enabled(enabled) {
                    Ok(mode_changed) => changed |= mode_changed,
                    Err(_) => {
                        runtime.set_playlist_safe_feedback("Не удалось изменить перемешивание");
                        changed = true;
                    }
                }
            }
            PlaylistAction::SetStopAfterCurrent(enabled) => {
                let transition_was_pending = runtime
                    .playlist_interaction_model()
                    .navigation_cancel_available;
                let outcome =
                    runtime.toggle_playlist_stop_after_current(enabled, TransportActionOrigin::Ui);
                if enabled
                    && transition_was_pending
                    && !matches!(outcome, None | Some(StopAfterCurrentOutcome::NoActiveMedia))
                {
                    runtime.set_playlist_safe_feedback(
                        "Ожидающий переход отменён; выключение режима его не возобновит",
                    );
                }
                changed |= !matches!(outcome, None | Some(StopAfterCurrentOutcome::NoActiveMedia));
            }
            PlaylistAction::Sort(intent) => {
                if runtime.start_metadata_sort(intent).is_err() {
                    runtime.set_playlist_safe_feedback("Не удалось начать сортировку");
                }
                changed = true;
            }
            PlaylistAction::CancelProgress(scope) => {
                changed |= match scope {
                    PlaylistProgressCancelScope::ManualAdd => runtime.cancel_all_manual_file_adds(),
                    PlaylistProgressCancelScope::SiblingDiscovery => {
                        runtime.cancel_sibling_discovery_from_ui()
                    }
                    PlaylistProgressCancelScope::MetadataSort(job_id) => {
                        matches!(
                            runtime.cancel_metadata_sort(job_id),
                            MetadataSortCancelOutcome::Requested
                        )
                    }
                };
            }
            PlaylistAction::CancelNavigation => {
                changed |= runtime.cancel_playlist_navigation_from_ui();
            }
            PlaylistAction::RetrySave => {
                if runtime.retry_playlist_state_save().is_err() {
                    runtime.set_playlist_safe_feedback("Повторное сохранение сейчас недоступно");
                }
                changed = true;
            }
            PlaylistAction::GoCurrent(target) => {
                app_state.request_playlist_go_current(target);
                changed = true;
            }
            PlaylistAction::UrlFocusRestored => {
                runtime.acknowledge_playlist_url_focus();
            }
        }
    }
    changed
}

fn apply_removal_outcome(
    app_state: &mut AppState,
    runtime: &mut PlaylistRuntime,
    outcome: RuntimeRemovalOutcome,
    action_label: &'static str,
) -> bool {
    if let RuntimeRemovalOutcome::Removed {
        selected_item_id, ..
    } = outcome
    {
        if let Some(selected_item_id) = selected_item_id {
            app_state.request_playlist_row_focus(selected_item_id);
        }
        return true;
    }
    if !matches!(
        outcome,
        RuntimeRemovalOutcome::NotFound { .. } | RuntimeRemovalOutcome::NoChange
    ) {
        runtime.set_playlist_safe_feedback(format!("Не удалось {action_label}"));
        return true;
    }
    false
}
