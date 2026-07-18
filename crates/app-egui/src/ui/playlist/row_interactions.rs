//! Row-level egui events преобразуются только в typed actions и focus intents.

use std::sync::Arc;

use egui::{Key, Modifiers, PointerButton, Response};
use playlist_core::PlaylistItemId;

use super::actions::{RemoveSelected, RemoveUnselected};
use super::{PlaylistAction, PlaylistUiOutput, PlaylistUiState, virtualized_drag};
use crate::playlist_runtime::{ClearSelectionCursor, PlaylistViewModel, UpdateSelection};

/// Единый full-row response обслуживает pointer, keyboard, menu и drag.
pub(super) fn handle_row_response(
    ui: &mut egui::Ui,
    response: &Response,
    model: &PlaylistViewModel,
    row_index: usize,
    item_id: PlaylistItemId,
    state: &mut PlaylistUiState,
    output: &mut PlaylistUiOutput,
) {
    let row_is_selected = model.selection().is_selected(item_id);
    let modifiers = ui.input(|input| input.modifiers);
    for action in pointer_actions(
        model,
        item_id,
        row_is_selected,
        modifiers,
        response.clicked_by(PointerButton::Primary),
        response.double_clicked_by(PointerButton::Primary),
    ) {
        output.push_action(action);
    }
    if response.double_clicked_by(PointerButton::Primary)
        || response.clicked_by(PointerButton::Primary)
        || response.secondary_clicked()
    {
        response.request_focus();
    }

    // Right-click внутри selection сохраняет группу; снаружи сначала выбирает строку.
    if response.secondary_clicked() && !row_is_selected {
        output.push_action(PlaylistAction::UpdateSelection(replace_one(model, item_id)));
    }

    if response.has_focus() {
        handle_focused_keyboard(ui, model, row_index, item_id, state, output);
    }

    response.context_menu(|menu_ui| {
        if menu_ui.button("Воспроизвести").clicked() {
            output.push_action(PlaylistAction::Play(item_id));
            menu_ui.close();
        }

        // Счётчик берётся из Arc-backed snapshot за O(1), не сканируя queue каждый кадр.
        let selected_count = if row_is_selected {
            model.selection().selected_count()
        } else {
            1
        };
        if menu_ui
            .add_enabled(
                selected_count > 0,
                egui::Button::new(format!("Удалить выбранные ({selected_count})")),
            )
            .clicked()
        {
            // Exact IDs материализуются только в момент destructive action.
            let selected_item_ids = if row_is_selected {
                model.selected_item_ids()
            } else {
                Arc::from([item_id])
            };
            output.push_action(PlaylistAction::RemoveSelected(RemoveSelected::new(
                selected_item_ids,
                model.structural_revision(),
            )));
            menu_ui.close();
        }

        if menu_ui
            .add_enabled(
                selected_count < model.item_count(),
                egui::Button::new(format!("Удалить всё, кроме выбранных ({selected_count})")),
            )
            .clicked()
        {
            // Complement тоже строится только после явного подтверждения кликом по menu item.
            let unselected_item_ids =
                unselected_for_effective_selection(model, row_is_selected, item_id);
            output.push_action(PlaylistAction::RemoveUnselected(RemoveUnselected::new(
                unselected_item_ids,
                model.structural_revision(),
            )));
            menu_ui.close();
        }
    });

    virtualized_drag::begin_from_response(
        response,
        model,
        item_id,
        row_is_selected,
        &mut state.drag,
        output,
    );
}

/// Focus-scoped hotkeys не конфликтуют с global transport hotkeys.
fn handle_focused_keyboard(
    ui: &mut egui::Ui,
    model: &PlaylistViewModel,
    row_index: usize,
    item_id: PlaylistItemId,
    state: &mut PlaylistUiState,
    output: &mut PlaylistUiOutput,
) {
    if take_key(ui, Modifiers::COMMAND, Key::A) {
        let item_ids = all_item_ids(model);
        let range_anchor = model
            .selection()
            .range_anchor()
            .or_else(|| item_ids.first().copied());
        let interaction_cursor = model.selection().interaction_cursor().or(range_anchor);
        output.push_action(PlaylistAction::UpdateSelection(
            UpdateSelection::SelectAll {
                item_ids,
                range_anchor,
                interaction_cursor,
                structural_revision: model.structural_revision(),
            },
        ));
    }

    if take_key(ui, Modifiers::NONE, Key::Escape) {
        if virtualized_drag::is_active(&state.drag) {
            virtualized_drag::cancel(ui.ctx(), &mut state.drag);
        } else {
            output.push_action(PlaylistAction::UpdateSelection(UpdateSelection::Clear {
                cursor: ClearSelectionCursor::Preserve,
            }));
        }
    }

    let navigation = take_navigation_key(ui);
    if let Some((navigation_key, modifiers)) = navigation
        && let Some(target_item_id) = navigation_target(model, row_index, navigation_key)
    {
        output.push_action(PlaylistAction::UpdateSelection(keyboard_selection_update(
            model,
            item_id,
            target_item_id,
            modifiers,
        )));
        state.request_row_focus(target_item_id);
    }

    if take_key(ui, Modifiers::NONE, Key::Enter) {
        output.push_action(PlaylistAction::Play(item_id));
    }
    if take_key(ui, Modifiers::NONE, Key::Delete) {
        let selected_item_ids = model.selected_item_ids();
        if !selected_item_ids.is_empty() {
            output.push_action(PlaylistAction::RemoveSelected(RemoveSelected::new(
                selected_item_ids,
                model.structural_revision(),
            )));
        }
    }
}

/// Pointer selection соблюдает обычные desktop Ctrl/Cmd/Shift combinations.
fn pointer_actions(
    model: &PlaylistViewModel,
    item_id: PlaylistItemId,
    row_is_selected: bool,
    modifiers: Modifiers,
    clicked: bool,
    double_clicked: bool,
) -> Vec<PlaylistAction> {
    if double_clicked {
        let mut actions = Vec::with_capacity(2);
        if !row_is_selected {
            actions.push(PlaylistAction::UpdateSelection(replace_one(model, item_id)));
        }
        actions.push(PlaylistAction::Play(item_id));
        return actions;
    }
    if clicked {
        return vec![PlaylistAction::UpdateSelection(pointer_selection_update(
            model, item_id, modifiers,
        ))];
    }
    Vec::new()
}

/// Обычный replace intent используется click, right-click и unselected drag.
fn replace_one(model: &PlaylistViewModel, item_id: PlaylistItemId) -> UpdateSelection {
    UpdateSelection::Replace {
        item_id,
        structural_revision: model.structural_revision(),
    }
}

/// Разрешает Shift-range только в момент explicit pointer event.
fn pointer_selection_update(
    model: &PlaylistViewModel,
    item_id: PlaylistItemId,
    modifiers: Modifiers,
) -> UpdateSelection {
    if modifiers.command && modifiers.shift {
        return range_update(model, item_id, item_id, RangeSelectionMode::Add);
    }
    if modifiers.shift {
        return range_update(model, item_id, item_id, RangeSelectionMode::Replace);
    }
    if modifiers.command {
        return UpdateSelection::Toggle {
            item_id,
            structural_revision: model.structural_revision(),
        };
    }
    replace_one(model, item_id)
}

/// Keyboard Ctrl/Cmd переносит cursor, а Shift расширяет exact range.
fn keyboard_selection_update(
    model: &PlaylistViewModel,
    current_item_id: PlaylistItemId,
    target_item_id: PlaylistItemId,
    modifiers: Modifiers,
) -> UpdateSelection {
    if modifiers.command && modifiers.shift {
        return range_update(
            model,
            current_item_id,
            target_item_id,
            RangeSelectionMode::Add,
        );
    }
    if modifiers.shift {
        return range_update(
            model,
            current_item_id,
            target_item_id,
            RangeSelectionMode::Replace,
        );
    }
    if modifiers.command {
        return UpdateSelection::MoveCursor {
            item_id: target_item_id,
            structural_revision: model.structural_revision(),
        };
    }
    replace_one(model, target_item_id)
}

/// Typed range mode устраняет неочевидный positional bool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeSelectionMode {
    Replace,
    Add,
}

/// Строит exact canonical range из stable anchor и target.
fn range_update(
    model: &PlaylistViewModel,
    fallback_anchor: PlaylistItemId,
    target_item_id: PlaylistItemId,
    mode: RangeSelectionMode,
) -> UpdateSelection {
    let range_anchor = model
        .selection()
        .range_anchor()
        .or_else(|| model.selection().interaction_cursor())
        .unwrap_or(fallback_anchor);
    let item_ids = model
        .range_item_ids(range_anchor, target_item_id)
        .unwrap_or_else(|| Arc::from([target_item_id]));
    match mode {
        RangeSelectionMode::Replace => UpdateSelection::ReplaceRange {
            item_ids,
            range_anchor,
            interaction_cursor: target_item_id,
            structural_revision: model.structural_revision(),
        },
        RangeSelectionMode::Add => UpdateSelection::AddRange {
            item_ids,
            range_anchor,
            interaction_cursor: target_item_id,
            structural_revision: model.structural_revision(),
        },
    }
}

/// Собирает exact full queue только на Ctrl/Cmd+A.
fn all_item_ids(model: &PlaylistViewModel) -> Arc<[PlaylistItemId]> {
    (0..model.item_count())
        .filter_map(|row_index| model.item_id_at(row_index))
        .collect::<Vec<_>>()
        .into()
}

/// Строит exact complement effective context selection только пока открыто menu.
fn unselected_for_effective_selection(
    model: &PlaylistViewModel,
    row_is_selected: bool,
    context_item_id: PlaylistItemId,
) -> Arc<[PlaylistItemId]> {
    (0..model.item_count())
        .filter_map(|row_index| model.item_id_at(row_index))
        .filter(|item_id| {
            if row_is_selected {
                !model.selection().is_selected(*item_id)
            } else {
                *item_id != context_item_id
            }
        })
        .collect::<Vec<_>>()
        .into()
}

/// Consume использует logical modifiers egui: COMMAND означает Cmd на macOS и Ctrl иначе.
fn take_key(ui: &mut egui::Ui, modifiers: Modifiers, key: Key) -> bool {
    ui.input_mut(|input| input.consume_key(modifiers, key))
}

/// Проверяет наиболее специфичные modifier combinations до более общих.
fn take_navigation_key(ui: &mut egui::Ui) -> Option<(RowNavigationKey, Modifiers)> {
    for modifiers in [
        Modifiers::COMMAND | Modifiers::SHIFT,
        Modifiers::SHIFT,
        Modifiers::COMMAND,
        Modifiers::NONE,
    ] {
        for (key, navigation_key) in [
            (Key::ArrowUp, RowNavigationKey::Up),
            (Key::ArrowDown, RowNavigationKey::Down),
            (Key::Home, RowNavigationKey::Home),
            (Key::End, RowNavigationKey::End),
        ] {
            if take_key(ui, modifiers, key) {
                return Some((navigation_key, modifiers));
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowNavigationKey {
    Up,
    Down,
    Home,
    End,
}

/// Navigation target остаётся stable-ID based и bounded одним O(1) lookup.
fn navigation_target(
    model: &PlaylistViewModel,
    row_index: usize,
    key: RowNavigationKey,
) -> Option<PlaylistItemId> {
    let last_index = model.item_count().saturating_sub(1);
    let target_index = match key {
        RowNavigationKey::Up => row_index.saturating_sub(1),
        RowNavigationKey::Down => row_index.saturating_add(1).min(last_index),
        RowNavigationKey::Home => 0,
        RowNavigationKey::End => last_index,
    };
    model.item_id_at(target_index)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use playlist_core::{
        CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind, PlaylistQueue,
    };

    use super::*;

    /// Duplicate locators доказывают, что interaction адресует только stable IDs.
    fn model(item_count: usize) -> PlaylistViewModel {
        let mut queue = PlaylistQueue::new();
        let drafts = (0..item_count).map(|index| {
            PlaylistItemDraft::local(
                LocalLocator::Native(PathBuf::from("duplicate.mp3")),
                None,
                CachedPlaylistMetadata::new(format!("row-{index}"), PlaylistMediaKind::Audio),
            )
        });
        queue.append_batch(drafts.collect()).unwrap();
        PlaylistViewModel::for_queue_with_revision(
            &queue,
            1,
            crate::playlist_runtime::PlaylistLoadingView::Ready,
        )
    }

    #[test]
    fn click_double_click_and_modifiers_emit_exact_typed_intents() {
        let model = model(3);
        let first = model.item_id_at(0).unwrap();
        assert!(matches!(
            pointer_actions(&model, first, false, Modifiers::NONE, true, false).as_slice(),
            [PlaylistAction::UpdateSelection(UpdateSelection::Replace {
                item_id,
                ..
            })] if *item_id == first
        ));
        assert_eq!(
            pointer_actions(&model, first, true, Modifiers::NONE, true, true),
            vec![PlaylistAction::Play(first)]
        );
        assert!(matches!(
            pointer_actions(&model, first, false, Modifiers::COMMAND, true, false).as_slice(),
            [PlaylistAction::UpdateSelection(UpdateSelection::Toggle {
                item_id,
                ..
            })] if *item_id == first
        ));
    }

    #[test]
    fn keyboard_navigation_keeps_exact_duplicate_ids() {
        let model = model(4);
        let first = model.item_id_at(0).unwrap();
        let second = model.item_id_at(1).unwrap();
        let last = model.item_id_at(3).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            navigation_target(&model, 1, RowNavigationKey::Up),
            Some(first)
        );
        assert_eq!(
            navigation_target(&model, 1, RowNavigationKey::Down),
            model.item_id_at(2)
        );
        assert_eq!(
            navigation_target(&model, 2, RowNavigationKey::Home),
            Some(first)
        );
        assert_eq!(
            navigation_target(&model, 0, RowNavigationKey::End),
            Some(last)
        );
    }

    #[test]
    fn context_complement_uses_effective_right_clicked_selection() {
        let model = model(4);
        let second = model.item_id_at(1).unwrap();
        assert_eq!(
            unselected_for_effective_selection(&model, false, second).as_ref(),
            [
                model.item_id_at(0).unwrap(),
                model.item_id_at(2).unwrap(),
                model.item_id_at(3).unwrap(),
            ]
        );
    }
}
