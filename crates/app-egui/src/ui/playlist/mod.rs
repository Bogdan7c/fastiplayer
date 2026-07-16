//! Read-only virtualized Playlist sidebar.

mod actions;
mod renderer;
mod status;
mod toolbar;

pub(crate) use actions::PlaylistAction;

use std::sync::Arc;

use playlist_core::PlaylistItemId;

use crate::playlist_runtime::{
    PlaylistGoCurrentTarget, PlaylistInteractionModel, PlaylistRuntimeBinding,
    PlaylistStructuralRevision, PlaylistViewModel,
};

const MAX_VISIBLE_HINT_ITEMS: usize = 256;

/// UI-owned положение viewport; controller cursor/active identity сюда не попадают.
#[derive(Debug, Default)]
pub(crate) struct PlaylistUiState {
    viewport_anchor: Option<ViewportAnchor>,
    observed_structural_revision: Option<PlaylistStructuralRevision>,
    go_current: Option<PlaylistGoCurrentTarget>,
}

#[derive(Debug, Clone, Copy)]
struct ViewportAnchor {
    item_id: PlaylistItemId,
    intra_row_offset: f32,
}

/// Typed output renderer-а остаётся bounded даже при огромном viewport-е.
#[derive(Debug, Default)]
pub(crate) struct PlaylistUiOutput {
    visible_item_ids: Vec<PlaylistItemId>,
    actions: Vec<PlaylistAction>,
}

impl PlaylistUiOutput {
    pub(super) fn push_action(&mut self, action: PlaylistAction) {
        self.actions.push(action);
    }

    pub(crate) fn take_actions(&mut self) -> Vec<PlaylistAction> {
        std::mem::take(&mut self.actions)
    }

    fn record_visible(&mut self, item_id: PlaylistItemId) {
        if self.visible_item_ids.len() >= MAX_VISIBLE_HINT_ITEMS
            || self.visible_item_ids.contains(&item_id)
        {
            return;
        }
        self.visible_item_ids.push(item_id);
    }

    pub(crate) fn into_visible_hint(
        self,
        binding: PlaylistRuntimeBinding,
    ) -> Option<PlaylistVisibleItemsHint> {
        (!self.visible_item_ids.is_empty()).then(|| PlaylistVisibleItemsHint {
            binding,
            item_ids: self.visible_item_ids.into(),
        })
    }
}

impl PlaylistUiState {
    /// D80 intent применяется после render и исполняется ровно в следующем authoritative frame.
    pub(crate) fn request_go_current(&mut self, target: PlaylistGoCurrentTarget) {
        self.go_current = Some(target);
    }

    pub(super) fn take_go_current(&mut self) -> Option<PlaylistGoCurrentTarget> {
        self.go_current.take()
    }

    pub(super) fn take_tombstone_request(&mut self) -> bool {
        if matches!(self.go_current, Some(PlaylistGoCurrentTarget::Tombstone)) {
            self.go_current = None;
            true
        } else {
            false
        }
    }
}

/// Exact runtime binding не даёт применить hint от stale renderer attachment-а.
#[derive(Debug, Clone)]
pub(crate) struct PlaylistVisibleItemsHint {
    binding: PlaylistRuntimeBinding,
    item_ids: Arc<[PlaylistItemId]>,
}

impl PlaylistVisibleItemsHint {
    pub(crate) const fn binding(&self) -> PlaylistRuntimeBinding {
        self.binding
    }

    pub(crate) fn item_ids(&self) -> &[PlaylistItemId] {
        &self.item_ids
    }
}

pub(crate) fn show(
    ui: &mut egui::Ui,
    model: Option<&PlaylistViewModel>,
    interaction: &PlaylistInteractionModel,
    state: &mut PlaylistUiState,
    output: &mut PlaylistUiOutput,
) {
    let Some(model) = model else {
        status::show_unavailable(ui);
        return;
    };
    if !ui.is_enabled() {
        // Outgoing/incoming animation copies рисуются в другом egui ID scope.
        // Они не имеют права вернуть action, заменить viewport anchor или demand hint.
        let mut visual_state = PlaylistUiState::default();
        let mut discarded_output = PlaylistUiOutput::default();
        toolbar::show(ui, interaction, &mut discarded_output);
        status::show_summary(ui, model, &mut visual_state);
        renderer::show_rows(ui, model, &mut visual_state, &mut discarded_output);
        return;
    }
    toolbar::show(ui, interaction, output);
    status::show_summary(ui, model, state);
    renderer::show_rows(ui, model, state, output);
}

#[cfg(test)]
mod tests;
