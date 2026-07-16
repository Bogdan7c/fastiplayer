//! Row-level egui events преобразуются только в typed actions и focus intents.

use egui::{Key, Modifiers, Response};
use playlist_core::PlaylistItemId;

use super::{PlaylistAction, PlaylistUiOutput, PlaylistUiState, virtualized_drag};
use crate::playlist_runtime::PlaylistViewModel;

pub(super) fn handle_row_response(
    ui: &mut egui::Ui,
    response: &Response,
    model: &PlaylistViewModel,
    row_index: usize,
    item_id: PlaylistItemId,
    state: &mut PlaylistUiState,
    output: &mut PlaylistUiOutput,
) {
    let pointer_actions = pointer_actions(item_id, response.clicked(), response.double_clicked());
    for action in pointer_actions {
        output.push_action(action);
    }
    if response.double_clicked() || response.clicked() {
        response.request_focus();
    }

    if response.has_focus() {
        handle_focused_keyboard(ui, model, row_index, item_id, state, output);
    }

    response.context_menu(|menu_ui| {
        if menu_ui.button("Воспроизвести").clicked() {
            output.push_action(context_action(item_id, ContextAction::Play));
            menu_ui.close();
        }
        if menu_ui.button("Удалить").clicked() {
            output.push_action(context_action(item_id, ContextAction::Remove));
            menu_ui.close();
        }
        if menu_ui.button("Удалить остальные").clicked() {
            output.push_action(context_action(item_id, ContextAction::RemoveOthers));
            menu_ui.close();
        }
    });

    virtualized_drag::begin_from_response(response, item_id, &mut state.drag);
}

fn handle_focused_keyboard(
    ui: &mut egui::Ui,
    model: &PlaylistViewModel,
    row_index: usize,
    item_id: PlaylistItemId,
    state: &mut PlaylistUiState,
    output: &mut PlaylistUiOutput,
) {
    let navigation_target = if consume_key(ui, Key::ArrowUp) {
        navigation_target(model, row_index, RowNavigationKey::Up)
    } else if consume_key(ui, Key::ArrowDown) {
        navigation_target(model, row_index, RowNavigationKey::Down)
    } else if consume_key(ui, Key::Home) {
        navigation_target(model, row_index, RowNavigationKey::Home)
    } else if consume_key(ui, Key::End) {
        navigation_target(model, row_index, RowNavigationKey::End)
    } else {
        None
    };

    if let Some(target_item_id) = navigation_target {
        output.push_action(PlaylistAction::Select(target_item_id));
        state.request_row_focus(target_item_id);
    }
    if consume_key(ui, Key::Enter) {
        output.push_action(PlaylistAction::Play(item_id));
    }
    if consume_key(ui, Key::Delete) {
        output.push_action(PlaylistAction::Remove(item_id));
    }
}

fn consume_key(ui: &mut egui::Ui, key: Key) -> bool {
    ui.input_mut(|input| input.consume_key(Modifiers::NONE, key))
}

fn pointer_actions(
    item_id: PlaylistItemId,
    clicked: bool,
    double_clicked: bool,
) -> Vec<PlaylistAction> {
    if double_clicked {
        vec![
            PlaylistAction::Select(item_id),
            PlaylistAction::Play(item_id),
        ]
    } else if clicked {
        vec![PlaylistAction::Select(item_id)]
    } else {
        Vec::new()
    }
}

#[derive(Debug, Clone, Copy)]
enum ContextAction {
    Play,
    Remove,
    RemoveOthers,
}

const fn context_action(item_id: PlaylistItemId, action: ContextAction) -> PlaylistAction {
    match action {
        ContextAction::Play => PlaylistAction::Play(item_id),
        ContextAction::Remove => PlaylistAction::Remove(item_id),
        ContextAction::RemoveOthers => PlaylistAction::RemoveOthers(item_id),
    }
}

#[derive(Debug, Clone, Copy)]
enum RowNavigationKey {
    Up,
    Down,
    Home,
    End,
}

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

    fn model(item_count: usize) -> PlaylistViewModel {
        let mut queue = PlaylistQueue::new();
        let drafts = (0..item_count).map(|index| {
            PlaylistItemDraft::local(
                // Одинаковый locator намеренно проверяет D09: действия адресуют exact Item ID.
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
    fn single_click_selects_without_play_and_double_click_plays_once() {
        let model = model(2);
        let first = model.item_id_at(0).unwrap();
        assert_eq!(
            pointer_actions(first, true, false),
            vec![PlaylistAction::Select(first)]
        );
        assert_eq!(
            pointer_actions(first, true, true),
            vec![PlaylistAction::Select(first), PlaylistAction::Play(first)]
        );
    }

    #[test]
    fn keyboard_targets_and_context_actions_keep_exact_duplicate_id() {
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
        assert_eq!(
            context_action(second, ContextAction::Play),
            PlaylistAction::Play(second)
        );
        assert_eq!(
            context_action(second, ContextAction::Remove),
            PlaylistAction::Remove(second)
        );
        assert_eq!(
            context_action(second, ContextAction::RemoveOthers),
            PlaylistAction::RemoveOthers(second)
        );
    }
}
