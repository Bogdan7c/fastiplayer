//! Post-render adapter typed Playlist actions к authoritative runtime owners.

use std::time::Instant;

use render_wgpu_shell::Renderer;
use winit::window::Window;

use crate::playlist_runtime::{
    ControllerMoveItemsOutcome, MetadataSortCancelOutcome, PlaylistProgressCancelScope,
    PlaylistRuntime, RemovalUndoOutcome, RuntimeMoveItemsOutcome, RuntimeRemovalOutcome,
    RuntimeUpdateSelectionOutcome, UpdateSelectionOutcome,
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
                    runtime.set_playlist_safe_feedback("Не удалось обновить выделение плейлиста");
                    changed = true;
                }
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
            PlaylistAction::RemoveSelected(action) => {
                let (item_ids, structural_revision) = action.into_parts();
                let outcome = runtime.remove_selected_playlist_items(
                    item_ids,
                    structural_revision,
                    Instant::now(),
                );
                changed |= apply_removal_outcome(
                    app_state,
                    runtime,
                    outcome,
                    "удалить выбранные элементы",
                );
            }
            PlaylistAction::RemoveUnselected(action) => {
                let (item_ids, structural_revision) = action.into_parts();
                let outcome = runtime.remove_unselected_playlist_items(
                    item_ids,
                    structural_revision,
                    Instant::now(),
                );
                changed |= apply_removal_outcome(
                    app_state,
                    runtime,
                    outcome,
                    "удалить невыбранные элементы",
                );
            }
            PlaylistAction::MoveItems(action) => {
                let (item_ids, intent, structural_revision) = action.into_parts();
                let outcome = runtime.move_playlist_items(item_ids, intent, structural_revision);
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
            PlaylistAction::SubmitUrl => {
                let yt_dlp_config = app_state.yt_dlp_metadata_config();
                changed |= runtime.submit_playlist_url_draft(&yt_dlp_config);
            }
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
            PlaylistAction::UndoRemoval => {
                // Только runtime owner проверяет deadline, invalidation и controller state.
                let outcome = runtime.undo_last_removal(Instant::now());
                // UI side effect выбирается из typed outcome без потери причины отказа.
                match undo_removal_ui_effect(outcome) {
                    UndoRemovalUiEffect::Restored { selected_item_id } => {
                        // Focus возвращается exact selected item из восстановленного snapshot-а.
                        if let Some(selected_item_id) = selected_item_id {
                            app_state.request_playlist_row_focus(selected_item_id);
                        }
                        tracing::debug!(
                            ?outcome,
                            "Playlist toolbar Undo успешно обработан runtime owner-ом"
                        );
                    }
                    UndoRemovalUiEffect::Rejected { safe_message } => {
                        // Пользователь получает безопасное сообщение без controller internals.
                        runtime.set_playlist_safe_feedback(safe_message);
                        // Typed diagnostic сохраняет unavailable/expiry/invalidation/failure.
                        if matches!(outcome, RemovalUndoOutcome::Controller(_)) {
                            tracing::warn!(
                                ?outcome,
                                "Playlist toolbar Undo отклонён controller boundary"
                            );
                        } else {
                            tracing::debug!(
                                ?outcome,
                                "Playlist toolbar Undo больше не authoritative"
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
    /// Snapshot восстановлен; exact selected item может отсутствовать у пустого selection.
    Restored {
        selected_item_id: Option<playlist_core::PlaylistItemId>,
    },
    /// Undo отклонён; пользователю показывается безопасная локализованная причина.
    Rejected { safe_message: &'static str },
}

/// Сопоставляет каждую typed runtime-причину с безопасным UI-результатом.
fn undo_removal_ui_effect(outcome: RemovalUndoOutcome) -> UndoRemovalUiEffect {
    match outcome {
        RemovalUndoOutcome::Restored {
            selected_item_id, ..
        } => UndoRemovalUiEffect::Restored { selected_item_id },
        RemovalUndoOutcome::Unavailable => UndoRemovalUiEffect::Rejected {
            safe_message: "Отмена удаления уже недоступна",
        },
        RemovalUndoOutcome::Expired => UndoRemovalUiEffect::Rejected {
            safe_message: "Время для отмены удаления истекло",
        },
        RemovalUndoOutcome::Invalidated => UndoRemovalUiEffect::Rejected {
            safe_message: "Отмена удаления недоступна после изменения плейлиста",
        },
        RemovalUndoOutcome::Controller(_) => UndoRemovalUiEffect::Rejected {
            safe_message: "Не удалось отменить удаление из плейлиста",
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

#[cfg(test)]
mod tests {
    use playlist_core::PlaylistItemId;

    use super::{UndoRemovalUiEffect, undo_removal_ui_effect};
    use crate::playlist_runtime::{ControllerRemovalUndoOutcome, RemovalUndoOutcome};

    #[test]
    fn successful_undo_preserves_exact_restored_focus_target() {
        let selected_item_id =
            PlaylistItemId::from_persistence_value(41).expect("test ItemId должен быть ненулевым");
        let outcome = RemovalUndoOutcome::Restored {
            selected_item_id: Some(selected_item_id),
            reattached_active: true,
        };

        assert_eq!(
            undo_removal_ui_effect(outcome),
            UndoRemovalUiEffect::Restored {
                selected_item_id: Some(selected_item_id)
            }
        );
    }

    #[test]
    fn unavailable_expired_and_invalidated_keep_distinct_safe_messages() {
        let cases = [
            (
                RemovalUndoOutcome::Unavailable,
                "Отмена удаления уже недоступна",
            ),
            (
                RemovalUndoOutcome::Expired,
                "Время для отмены удаления истекло",
            ),
            (
                RemovalUndoOutcome::Invalidated,
                "Отмена удаления недоступна после изменения плейлиста",
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
                safe_message: "Не удалось отменить удаление из плейлиста"
            }
        );
        assert!(matches!(
            typed_outcome,
            RemovalUndoOutcome::Controller(ControllerRemovalUndoOutcome::DirtyRevisionExhausted)
        ));
    }
}
