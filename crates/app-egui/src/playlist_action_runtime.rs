//! Post-render adapter typed Playlist actions к authoritative runtime owners.

use std::time::Instant;

use render_wgpu_shell::Renderer;
use winit::window::Window;

use crate::playlist_runtime::{
    ControllerMoveItemsOutcome, PlaylistRuntime, PlaylistUrlImportCancelOutcome,
    RemovalUndoOutcome, RuntimeCompoundHeaderPlayOutcome, RuntimeCompoundPartPlayOutcome,
    RuntimeMoveItemsOutcome, RuntimeRemovalOutcome, RuntimeToggleCompoundDisclosureOutcome,
    RuntimeUpdateSelectionOutcome, ToggleCompoundDisclosureOutcome, UpdateSelectionOutcome,
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
            PlaylistAction::UpdateSelection(update) => {
                let outcome = runtime.update_playlist_selection(update);
                changed |= matches!(
                    outcome,
                    RuntimeUpdateSelectionOutcome::Controller(UpdateSelectionOutcome::Updated)
                );
                if !matches!(
                    outcome,
                    RuntimeUpdateSelectionOutcome::Controller(
                        UpdateSelectionOutcome::Updated | UpdateSelectionOutcome::NoChange
                    )
                ) {
                    runtime.set_playlist_safe_feedback("Could not update Playlist selection");
                    changed = true;
                }
            }
            PlaylistAction::Play(item_id) => {
                let outcome = runtime.play_playlist_row(item_id);
                let applied = crate::transport_runtime::apply_playlist_row_play(
                    app_state, runtime, renderer, outcome,
                );
                if !applied {
                    runtime.set_playlist_safe_feedback("Could not play the selected item");
                }
                changed = true;
            }
            PlaylistAction::PlayCompoundHeader(action) => {
                let applied = match runtime.play_compound_header(action) {
                    RuntimeCompoundHeaderPlayOutcome::Play(outcome) => {
                        crate::transport_runtime::apply_playlist_row_play(
                            app_state, runtime, renderer, outcome,
                        )
                    }
                    RuntimeCompoundHeaderPlayOutcome::Rejected(reason) => {
                        tracing::debug!(?reason, "Compound header Play rejected by typed boundary");
                        false
                    }
                    RuntimeCompoundHeaderPlayOutcome::LoadDecisionPending => false,
                };
                if !applied {
                    runtime
                        .set_playlist_safe_feedback("Could not play the selected compound entry");
                }
                changed = true;
            }
            PlaylistAction::PlayCompoundPart(action) => {
                let applied = match runtime.play_compound_part(action) {
                    RuntimeCompoundPartPlayOutcome::Play(outcome) => {
                        crate::transport_runtime::apply_playlist_row_play(
                            app_state, runtime, renderer, outcome,
                        )
                    }
                    RuntimeCompoundPartPlayOutcome::Rejected(reason) => {
                        tracing::debug!(?reason, "Compound part Play rejected by typed boundary");
                        false
                    }
                    RuntimeCompoundPartPlayOutcome::LoadDecisionPending => false,
                };
                if !applied {
                    runtime.set_playlist_safe_feedback(
                        "Could not play the selected part of the compound entry",
                    );
                }
                changed = true;
            }
            PlaylistAction::ToggleCompoundDisclosure(action) => {
                let outcome = runtime.toggle_compound_disclosure(action);
                if !matches!(
                    outcome,
                    RuntimeToggleCompoundDisclosureOutcome::Controller(
                        ToggleCompoundDisclosureOutcome::Expanded
                            | ToggleCompoundDisclosureOutcome::Collapsed
                    )
                ) {
                    runtime.set_playlist_safe_feedback(
                        "Could not expand or collapse the compound entry",
                    );
                }
                changed = true;
            }
            PlaylistAction::RemoveSelected(action) => {
                let (entry_ids, structural_revision) = action.into_parts();
                let outcome = runtime.remove_selected_playlist_items(
                    entry_ids,
                    structural_revision,
                    Instant::now(),
                );
                changed |=
                    apply_removal_outcome(app_state, runtime, outcome, "remove selected elements");
            }
            PlaylistAction::RemoveUnselected(action) => {
                let (entry_ids, structural_revision) = action.into_parts();
                let outcome = runtime.remove_unselected_playlist_items(
                    entry_ids,
                    structural_revision,
                    Instant::now(),
                );
                changed |=
                    apply_removal_outcome(app_state, runtime, outcome, "remove unselected items");
            }
            PlaylistAction::MoveItems(action) => {
                let (entry_ids, intent, structural_revision) = action.into_parts();
                let outcome = runtime.move_playlist_items(entry_ids, intent, structural_revision);
                changed |= matches!(
                    outcome,
                    RuntimeMoveItemsOutcome::Controller(ControllerMoveItemsOutcome::Moved { .. })
                );
                if !matches!(
                    outcome,
                    RuntimeMoveItemsOutcome::Controller(
                        ControllerMoveItemsOutcome::Moved { .. }
                            | ControllerMoveItemsOutcome::AlreadyInPlace { .. }
                    )
                ) {
                    runtime.set_playlist_safe_feedback("Could not change Playlist order");
                    changed = true;
                }
            }
            PlaylistAction::AddFiles => changed |= runtime.start_playlist_file_dialog(window),
            PlaylistAction::StartImport(intent) => {
                changed |= runtime.start_playlist_import_dialog(window, intent);
            }
            PlaylistAction::StartExport(request) => {
                changed |= runtime.start_playlist_export(window, request);
            }
            PlaylistAction::ContinueImport(preview_id) => {
                let outcome = runtime.continue_playlist_import(preview_id);
                if let Some(feedback) = outcome.failure_feedback() {
                    runtime.set_playlist_safe_feedback(feedback);
                }
                changed = true;
            }
            PlaylistAction::CancelImport(preview_id) => {
                changed |= runtime.cancel_playlist_import(preview_id);
            }
            PlaylistAction::OpenUrlEditor => {
                runtime.open_playlist_url_editor();
                changed = true;
            }
            PlaylistAction::UpdateUrlDraft(text) => {
                runtime.update_playlist_url_draft(text.into_inner());
                changed = true;
            }
            PlaylistAction::SubmitUrl => {
                let yt_dlp_config = app_state.yt_dlp_metadata_config();
                changed |= runtime.submit_playlist_url_draft(&yt_dlp_config);
            }
            PlaylistAction::CancelUrlEditor => {
                changed |= runtime.cancel_playlist_url_editor();
            }
            PlaylistAction::CancelUrlImport => {
                // Перерисовка нужна, только если индикатор действительно исчез.
                changed |= matches!(
                    runtime.cancel_playlist_url_import_by_user(),
                    PlaylistUrlImportCancelOutcome::Cancelled
                );
            }
            PlaylistAction::Clear => {
                if runtime.playlist_interaction_model().item_count == 0 {
                    continue;
                }
                let outcome = runtime.clear_playlist_observing_playback(
                    Instant::now(),
                    app_state.playback_progress_for_clear(),
                );
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
                    runtime.set_playlist_safe_feedback("Could not clear Playlist");
                    changed = true;
                }
            }
            PlaylistAction::Sort(intent) => {
                if runtime.start_metadata_sort(intent).is_err() {
                    runtime.set_playlist_safe_feedback("Could not start sorting");
                }
                changed = true;
            }
            PlaylistAction::CancelNavigation => {
                changed |= runtime.cancel_playlist_navigation_from_ui();
            }
            PlaylistAction::RetrySave => {
                if runtime.retry_playlist_state_save().is_err() {
                    runtime.set_playlist_safe_feedback("Retry saving is currently unavailable");
                }
                changed = true;
            }
            PlaylistAction::GoCurrent(target) => {
                app_state.request_playlist_go_current(target);
                changed = true;
            }
            PlaylistAction::UndoRemoval => {
                // Только runtime owner проверяет deadline, invalidation и controller state.
                let undo = runtime.undo_last_removal_resuming_playback(Instant::now());
                let outcome = undo.outcome;
                // Undo Clear возвращает и воспроизведение: тем же путём, что клик по строке.
                if let Some(playback) = undo.playback {
                    let started = crate::transport_runtime::apply_playlist_row_play(
                        app_state,
                        runtime,
                        renderer,
                        crate::playlist_runtime::RuntimeRowPlayOutcome::Controller(playback),
                    );
                    if !started {
                        runtime.set_playlist_safe_feedback(
                            "Playlist restored, but playback could not be restored",
                        );
                    }
                }
                // UI side effect выбирается из typed outcome без потери причины отказа.
                match undo_removal_ui_effect(outcome) {
                    UndoRemovalUiEffect::Restored { selected_entry_id } => {
                        // Focus возвращается exact structural entry из восстановленного snapshot-а.
                        if let Some(selected_entry_id) = selected_entry_id {
                            app_state.request_playlist_row_focus(selected_entry_id);
                        }
                        tracing::debug!(
                            ?outcome,
                            "Playlist toolbar Undo successfully handled by runtime owner"
                        );
                    }
                    UndoRemovalUiEffect::Rejected { safe_message } => {
                        // Пользователь получает безопасное сообщение без controller internals.
                        runtime.set_playlist_safe_feedback(safe_message);
                        // Typed diagnostic сохраняет unavailable/expiry/invalidation/failure.
                        if matches!(outcome, RemovalUndoOutcome::Controller(_)) {
                            tracing::warn!(
                                ?outcome,
                                "Playlist toolbar Undo rejected by boundary controller"
                            );
                        } else {
                            tracing::debug!(
                                ?outcome,
                                "Playlist toolbar Undo is no longer authoritative"
                            );
                        }
                    }
                }
                changed = true;
            }
            PlaylistAction::UrlFocusRestored => {
                runtime.acknowledge_playlist_url_focus();
            }
        }
    }
    changed
}

/// UI-эффект typed Undo outcome не раскрывает controller storage вызывающему коду.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UndoRemovalUiEffect {
    /// Snapshot восстановлен; exact selected entry может отсутствовать у пустого selection.
    Restored {
        selected_entry_id: Option<playlist_core::PlaylistEntryId>,
    },
    /// Undo отклонён; пользователю показывается безопасная локализованная причина.
    Rejected { safe_message: &'static str },
}

/// Сопоставляет каждую typed runtime-причину с безопасным UI-результатом.
fn undo_removal_ui_effect(outcome: RemovalUndoOutcome) -> UndoRemovalUiEffect {
    match outcome {
        RemovalUndoOutcome::Restored {
            selected_entry_id, ..
        } => UndoRemovalUiEffect::Restored { selected_entry_id },
        RemovalUndoOutcome::Unavailable => UndoRemovalUiEffect::Rejected {
            safe_message: "Undo removal is no longer available",
        },
        RemovalUndoOutcome::Expired => UndoRemovalUiEffect::Rejected {
            safe_message: "Undo removal time expired",
        },
        RemovalUndoOutcome::Invalidated => UndoRemovalUiEffect::Rejected {
            safe_message: "Undo removal is unavailable after Playlist changes",
        },
        RemovalUndoOutcome::Controller(_) => UndoRemovalUiEffect::Rejected {
            safe_message: "Could not undo removal from Playlist",
        },
    }
}

fn apply_removal_outcome(
    app_state: &mut AppState,
    runtime: &mut PlaylistRuntime,
    outcome: RuntimeRemovalOutcome,
    action_label: &'static str,
) -> bool {
    if let RuntimeRemovalOutcome::Removed {
        selected_entry_id, ..
    } = outcome
    {
        if let Some(selected_entry_id) = selected_entry_id {
            app_state.request_playlist_row_focus(selected_entry_id);
        }
        return true;
    }
    if !matches!(
        outcome,
        RuntimeRemovalOutcome::NotFound { .. } | RuntimeRemovalOutcome::NoChange
    ) {
        runtime.set_playlist_safe_feedback(format!("Could not {action_label}"));
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use playlist_core::{PlaylistEntryId, PlaylistItemId};

    use super::{UndoRemovalUiEffect, undo_removal_ui_effect};
    use crate::playlist_runtime::{ControllerRemovalUndoOutcome, RemovalUndoOutcome};

    #[test]
    fn successful_undo_preserves_exact_restored_focus_target() {
        let selected_item_id =
            PlaylistItemId::from_persistence_value(41).expect("Test ItemId must be non-zero.");
        let selected_entry_id = PlaylistEntryId::Single(selected_item_id);
        let outcome = RemovalUndoOutcome::Restored {
            selected_entry_id: Some(selected_entry_id),
            reattached_active: true,
        };

        assert_eq!(
            undo_removal_ui_effect(outcome),
            UndoRemovalUiEffect::Restored {
                selected_entry_id: Some(selected_entry_id)
            }
        );
    }

    #[test]
    fn unavailable_expired_and_invalidated_keep_distinct_safe_messages() {
        let cases = [
            (
                RemovalUndoOutcome::Unavailable,
                "Undo removal is no longer available",
            ),
            (RemovalUndoOutcome::Expired, "Undo removal time expired"),
            (
                RemovalUndoOutcome::Invalidated,
                "Undo removal is unavailable after Playlist changes",
            ),
        ];

        for (outcome, expected_message) in cases {
            assert_eq!(
                undo_removal_ui_effect(outcome),
                UndoRemovalUiEffect::Rejected {
                    safe_message: expected_message
                }
            );
        }
    }

    #[test]
    fn controller_failure_maps_to_safe_message_without_collapsing_typed_outcome() {
        let typed_outcome =
            RemovalUndoOutcome::Controller(ControllerRemovalUndoOutcome::DirtyRevisionExhausted);

        assert_eq!(
            undo_removal_ui_effect(typed_outcome),
            UndoRemovalUiEffect::Rejected {
                safe_message: "Could not undo removal from Playlist"
            }
        );
        assert!(matches!(
            typed_outcome,
            RemovalUndoOutcome::Controller(ControllerRemovalUndoOutcome::DirtyRevisionExhausted)
        ));
    }
}
