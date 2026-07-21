//! Row-level egui events преобразуются только в typed actions и focus intents.

use std::sync::Arc;

use egui::{Key, Modifiers, PointerButton, Response};
use playlist_core::{PlaylistEntryId, PlaylistItemId};

use super::actions::{RemoveSelected, RemoveUnselected};
use super::{PlaylistAction, PlaylistUiOutput, PlaylistUiState, virtualized_drag};
use crate::playlist_runtime::{
    ClearSelectionCursor, CompoundHeaderPlayAction, CompoundRuntimeRowId,
    CompoundRuntimeViewSnapshot, PlaylistViewModel, ToggleCompoundDisclosure, UpdateSelection,
};

/// Structural row activation сохраняет отличие Single Play от generation-fenced header Play.
#[derive(Debug, Clone, Copy)]
pub(super) enum StructuralRowActivation {
    /// Обычная строка запускает exact Item ID через существующий boundary.
    Single { item_id: PlaylistItemId },
    /// Compound header делегирует target resolution runtime owner-у.
    CompoundHeader {
        action: CompoundHeaderPlayAction,
        header_play_item_id: PlaylistItemId,
    },
}

/// Named context держит общие row interaction dependencies и mutable frame output вместе.
pub(super) struct RowInteractionContext<'a> {
    pub(super) model: &'a PlaylistViewModel,
    pub(super) compound_snapshot: &'a CompoundRuntimeViewSnapshot,
    pub(super) visible_row_index: usize,
    pub(super) state: &'a mut PlaylistUiState,
    pub(super) output: &'a mut PlaylistUiOutput,
}

impl StructuralRowActivation {
    /// Typed post-render action не разрешает target внутри renderer-а.
    const fn playlist_action(self) -> PlaylistAction {
        match self {
            Self::Single { item_id } => PlaylistAction::Play(item_id),
            Self::CompoundHeader { action, .. } => PlaylistAction::PlayCompoundHeader(action),
        }
    }

    /// Drag payload сохраняет representative playable identity только для hint/capture.
    const fn item_id(self) -> PlaylistItemId {
        match self {
            Self::Single { item_id }
            | Self::CompoundHeader {
                header_play_item_id: item_id,
                ..
            } => item_id,
        }
    }

    /// Space/AccessKit меняют disclosure только у CompoundHeader.
    const fn disclosure_action(self) -> Option<ToggleCompoundDisclosure> {
        match self {
            Self::Single { .. } => None,
            Self::CompoundHeader { action, .. } => Some(ToggleCompoundDisclosure {
                compound_entry_id: action.compound_entry_id,
                structural_revision: action.structural_revision,
            }),
        }
    }
}

/// Единый full-row response обслуживает pointer, keyboard, menu и drag.
pub(super) fn handle_row_response(
    ui: &mut egui::Ui,
    response: &Response,
    entry_id: PlaylistEntryId,
    activation: StructuralRowActivation,
    disclosure_hit_width: f32,
    context: &mut RowInteractionContext<'_>,
) {
    let row_is_selected = context.model.selection().is_selected(entry_id);
    let modifiers = ui.input(|input| input.modifiers);
    let primary_clicked = response.clicked_by(PointerButton::Primary);
    let row_was_focused = response.has_focus();
    let pointer_in_disclosure = activation.disclosure_action().is_some()
        && primary_clicked
        && response.interact_pointer_pos().is_some_and(|position| {
            position.x <= response.rect.left() + disclosure_hit_width.max(0.0)
        });
    if pointer_in_disclosure {
        if let Some(action) = activation.disclosure_action() {
            context
                .output
                .push_action(PlaylistAction::ToggleCompoundDisclosure(action));
        }
    } else {
        for action in pointer_actions(
            context.model,
            entry_id,
            activation,
            row_is_selected,
            modifiers,
            primary_clicked,
            response.double_clicked_by(PointerButton::Primary),
        ) {
            context.output.push_action(action);
        }
    }
    if response.double_clicked_by(PointerButton::Primary)
        || response.clicked_by(PointerButton::Primary)
        || response.secondary_clicked()
    {
        response.request_focus();
    }
    // AccessKit Default action может прийти до focus event; такой click нельзя терять.
    let unfocused_assistive_activation = response.clicked() && !primary_clicked && !row_was_focused;
    if unfocused_assistive_activation {
        if let Some(action) = activation.disclosure_action() {
            context
                .output
                .push_action(PlaylistAction::ToggleCompoundDisclosure(action));
        } else {
            context.output.push_action(activation.playlist_action());
        }
        response.request_focus();
    }

    // Right-click внутри selection сохраняет группу; снаружи сначала выбирает строку.
    if response.secondary_clicked() && !row_is_selected {
        context
            .output
            .push_action(PlaylistAction::UpdateSelection(replace_one(
                context.model,
                entry_id,
            )));
    }

    if row_was_focused {
        handle_focused_keyboard(ui, response, entry_id, activation, context);
    }

    response.context_menu(|menu_ui| {
        if menu_ui.button("Воспроизвести").clicked() {
            context.output.push_action(activation.playlist_action());
            menu_ui.close();
        }

        // Счётчик берётся из Arc-backed snapshot за O(1), не сканируя queue каждый кадр.
        let selected_count = if row_is_selected {
            context.model.selection().selected_count()
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
            let selected_entry_ids = if row_is_selected {
                context.model.selected_entry_ids()
            } else {
                Arc::from([entry_id])
            };
            context
                .output
                .push_action(PlaylistAction::RemoveSelected(RemoveSelected::new(
                    selected_entry_ids,
                    context.model.structural_revision(),
                )));
            menu_ui.close();
        }

        if menu_ui
            .add_enabled(
                selected_count < context.model.item_count(),
                egui::Button::new(format!("Удалить всё, кроме выбранных ({selected_count})")),
            )
            .clicked()
        {
            // Complement тоже строится только после явного подтверждения кликом по menu item.
            let unselected_entry_ids =
                unselected_for_effective_selection(context.model, row_is_selected, entry_id);
            context
                .output
                .push_action(PlaylistAction::RemoveUnselected(RemoveUnselected::new(
                    unselected_entry_ids,
                    context.model.structural_revision(),
                )));
            menu_ui.close();
        }
    });

    virtualized_drag::begin_from_response(
        response,
        context.model,
        entry_id,
        activation.item_id(),
        row_is_selected,
        &mut context.state.drag,
        context.output,
    );
}

/// Focus-scoped hotkeys не конфликтуют с global transport hotkeys.
fn handle_focused_keyboard(
    ui: &mut egui::Ui,
    response: &Response,
    entry_id: PlaylistEntryId,
    activation: StructuralRowActivation,
    context: &mut RowInteractionContext<'_>,
) {
    if take_key(ui, Modifiers::COMMAND, Key::A) {
        let entry_ids = all_entry_ids(context.model);
        let range_anchor = context
            .model
            .selection()
            .range_anchor()
            .or_else(|| entry_ids.first().copied());
        let interaction_cursor = context
            .model
            .selection()
            .interaction_cursor()
            .or(range_anchor);
        context.output.push_action(PlaylistAction::UpdateSelection(
            UpdateSelection::SelectAll {
                entry_ids,
                range_anchor,
                interaction_cursor,
                structural_revision: context.model.structural_revision(),
            },
        ));
    }

    if take_key(ui, Modifiers::NONE, Key::Escape) {
        if virtualized_drag::is_active(&context.state.drag) {
            virtualized_drag::cancel(ui.ctx(), &mut context.state.drag);
        } else {
            context
                .output
                .push_action(PlaylistAction::UpdateSelection(UpdateSelection::Clear {
                    cursor: ClearSelectionCursor::Preserve,
                }));
        }
    }

    handle_visible_navigation(ui, entry_id, context);

    let space_pressed = take_key(ui, Modifiers::NONE, Key::Space);
    let enter_pressed = take_key(ui, Modifiers::NONE, Key::Enter);
    if space_pressed && let Some(action) = activation.disclosure_action() {
        context
            .output
            .push_action(PlaylistAction::ToggleCompoundDisclosure(action));
    }
    if enter_pressed {
        context.output.push_action(activation.playlist_action());
    }
    let assistive_activation = response.clicked()
        && !response.clicked_by(PointerButton::Primary)
        && !space_pressed
        && !enter_pressed;
    if assistive_activation {
        if let Some(action) = activation.disclosure_action() {
            context
                .output
                .push_action(PlaylistAction::ToggleCompoundDisclosure(action));
        } else {
            context.output.push_action(activation.playlist_action());
        }
    }
    if take_key(ui, Modifiers::NONE, Key::Delete) {
        let selected_entry_ids = context.model.selected_entry_ids();
        if !selected_entry_ids.is_empty() {
            context
                .output
                .push_action(PlaylistAction::RemoveSelected(RemoveSelected::new(
                    selected_entry_ids,
                    context.model.structural_revision(),
                )));
        }
    }
}

/// Up/Down/Home/End проходят visible projections, но selection меняют только на structural row.
pub(super) fn handle_visible_navigation(
    ui: &mut egui::Ui,
    current_entry_id: PlaylistEntryId,
    context: &mut RowInteractionContext<'_>,
) {
    let Some((navigation_key, modifiers)) = take_navigation_key(ui) else {
        return;
    };
    let Some(target_row_id) = visible_navigation_target(
        context.compound_snapshot,
        context.visible_row_index,
        navigation_key,
    ) else {
        return;
    };
    if let CompoundRuntimeRowId::Entry(target_entry_id) = target_row_id {
        context
            .output
            .push_action(PlaylistAction::UpdateSelection(keyboard_selection_update(
                context.model,
                current_entry_id,
                target_entry_id,
                modifiers,
            )));
    }
    context.state.request_visible_row_focus(target_row_id);
}

/// Pointer selection соблюдает обычные desktop Ctrl/Cmd/Shift combinations.
fn pointer_actions(
    model: &PlaylistViewModel,
    entry_id: PlaylistEntryId,
    activation: StructuralRowActivation,
    row_is_selected: bool,
    modifiers: Modifiers,
    clicked: bool,
    double_clicked: bool,
) -> Vec<PlaylistAction> {
    if double_clicked {
        let mut actions = Vec::with_capacity(2);
        if !row_is_selected {
            actions.push(PlaylistAction::UpdateSelection(replace_one(
                model, entry_id,
            )));
        }
        actions.push(activation.playlist_action());
        return actions;
    }
    if clicked {
        return vec![PlaylistAction::UpdateSelection(pointer_selection_update(
            model, entry_id, modifiers,
        ))];
    }
    Vec::new()
}

/// Обычный replace intent используется click, right-click и unselected drag.
fn replace_one(model: &PlaylistViewModel, entry_id: PlaylistEntryId) -> UpdateSelection {
    UpdateSelection::Replace {
        entry_id,
        structural_revision: model.structural_revision(),
    }
}

/// Разрешает Shift-range только в момент explicit pointer event.
fn pointer_selection_update(
    model: &PlaylistViewModel,
    entry_id: PlaylistEntryId,
    modifiers: Modifiers,
) -> UpdateSelection {
    if modifiers.command && modifiers.shift {
        return range_update(model, entry_id, entry_id, RangeSelectionMode::Add);
    }
    if modifiers.shift {
        return range_update(model, entry_id, entry_id, RangeSelectionMode::Replace);
    }
    if modifiers.command {
        return UpdateSelection::Toggle {
            entry_id,
            structural_revision: model.structural_revision(),
        };
    }
    replace_one(model, entry_id)
}

/// Keyboard Ctrl/Cmd переносит cursor, а Shift расширяет exact range.
fn keyboard_selection_update(
    model: &PlaylistViewModel,
    current_entry_id: PlaylistEntryId,
    target_entry_id: PlaylistEntryId,
    modifiers: Modifiers,
) -> UpdateSelection {
    if modifiers.command && modifiers.shift {
        return range_update(
            model,
            current_entry_id,
            target_entry_id,
            RangeSelectionMode::Add,
        );
    }
    if modifiers.shift {
        return range_update(
            model,
            current_entry_id,
            target_entry_id,
            RangeSelectionMode::Replace,
        );
    }
    if modifiers.command {
        return UpdateSelection::MoveCursor {
            entry_id: target_entry_id,
            structural_revision: model.structural_revision(),
        };
    }
    replace_one(model, target_entry_id)
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
    fallback_anchor: PlaylistEntryId,
    target_entry_id: PlaylistEntryId,
    mode: RangeSelectionMode,
) -> UpdateSelection {
    let range_anchor = model
        .selection()
        .range_anchor()
        .or_else(|| model.selection().interaction_cursor())
        .unwrap_or(fallback_anchor);
    let entry_ids = model
        .range_entry_ids(range_anchor, target_entry_id)
        .unwrap_or_else(|| Arc::from([target_entry_id]));
    match mode {
        RangeSelectionMode::Replace => UpdateSelection::ReplaceRange {
            entry_ids,
            range_anchor,
            interaction_cursor: target_entry_id,
            structural_revision: model.structural_revision(),
        },
        RangeSelectionMode::Add => UpdateSelection::AddRange {
            entry_ids,
            range_anchor,
            interaction_cursor: target_entry_id,
            structural_revision: model.structural_revision(),
        },
    }
}

/// Собирает exact full queue только на Ctrl/Cmd+A.
fn all_entry_ids(model: &PlaylistViewModel) -> Arc<[PlaylistEntryId]> {
    (0..model.item_count())
        .filter_map(|row_index| model.entry_id_at(row_index))
        .collect::<Vec<_>>()
        .into()
}

/// Строит exact complement effective context selection только пока открыто menu.
fn unselected_for_effective_selection(
    model: &PlaylistViewModel,
    row_is_selected: bool,
    context_entry_id: PlaylistEntryId,
) -> Arc<[PlaylistEntryId]> {
    (0..model.item_count())
        .filter_map(|row_index| model.entry_id_at(row_index))
        .filter(|entry_id| {
            if row_is_selected {
                !model.selection().is_selected(*entry_id)
            } else {
                *entry_id != context_entry_id
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
fn visible_navigation_target(
    compound_snapshot: &CompoundRuntimeViewSnapshot,
    row_index: usize,
    key: RowNavigationKey,
) -> Option<CompoundRuntimeRowId> {
    let last_index = compound_snapshot.visible_row_count().saturating_sub(1);
    let target_index = match key {
        RowNavigationKey::Up => row_index.saturating_sub(1),
        RowNavigationKey::Down => row_index.saturating_add(1).min(last_index),
        RowNavigationKey::Home => 0,
        RowNavigationKey::End => last_index,
    };
    compound_snapshot.row_id_at(target_index)
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
        PlaylistViewModel::for_queue_with_revision(&queue, 1)
    }

    #[test]
    fn click_double_click_and_modifiers_emit_exact_typed_intents() {
        let model = model(3);
        let first = model.item_id_at(0).unwrap();
        let first_entry = model.entry_id_at(0).unwrap();
        let activation = StructuralRowActivation::Single { item_id: first };
        assert!(matches!(
            pointer_actions(&model, first_entry, activation, false, Modifiers::NONE, true, false).as_slice(),
            [PlaylistAction::UpdateSelection(UpdateSelection::Replace {
                entry_id,
                ..
            })] if *entry_id == first_entry
        ));
        assert_eq!(
            pointer_actions(
                &model,
                first_entry,
                activation,
                true,
                Modifiers::NONE,
                true,
                true
            ),
            vec![PlaylistAction::Play(first)]
        );
        assert!(matches!(
            pointer_actions(&model, first_entry, activation, false, Modifiers::COMMAND, true, false).as_slice(),
            [PlaylistAction::UpdateSelection(UpdateSelection::Toggle {
                entry_id,
                ..
            })] if *entry_id == first_entry
        ));
    }

    #[test]
    fn keyboard_navigation_keeps_exact_duplicate_ids() {
        let model = model(4);
        let snapshot = model.compound_snapshot();
        let first = model.entry_id_at(0).unwrap();
        let second = model.entry_id_at(1).unwrap();
        let last = model.entry_id_at(3).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            visible_navigation_target(snapshot, 1, RowNavigationKey::Up),
            Some(CompoundRuntimeRowId::Entry(first))
        );
        assert_eq!(
            visible_navigation_target(snapshot, 1, RowNavigationKey::Down),
            model.entry_id_at(2).map(CompoundRuntimeRowId::Entry)
        );
        assert_eq!(
            visible_navigation_target(snapshot, 2, RowNavigationKey::Home),
            Some(CompoundRuntimeRowId::Entry(first))
        );
        assert_eq!(
            visible_navigation_target(snapshot, 0, RowNavigationKey::End),
            Some(CompoundRuntimeRowId::Entry(last))
        );
    }

    #[test]
    fn context_complement_uses_effective_right_clicked_selection() {
        let model = model(4);
        let second = model.item_id_at(1).unwrap();
        assert_eq!(
            unselected_for_effective_selection(&model, false, PlaylistEntryId::Single(second),)
                .as_ref(),
            [
                model.entry_id_at(0).unwrap(),
                model.entry_id_at(2).unwrap(),
                model.entry_id_at(3).unwrap(),
            ]
        );
    }
}
