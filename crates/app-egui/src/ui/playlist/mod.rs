//! Read-only virtualized Playlist sidebar.

mod renderer;
mod status;

use std::sync::Arc;

use playlist_core::PlaylistItemId;

use crate::playlist_runtime::{
    PlaylistRuntimeBinding, PlaylistStructuralRevision, PlaylistViewModel,
};

const MAX_VISIBLE_HINT_ITEMS: usize = 256;

/// UI-owned положение viewport; controller cursor/active identity сюда не попадают.
#[derive(Debug, Default)]
pub(crate) struct PlaylistUiState {
    viewport_anchor: Option<ViewportAnchor>,
    observed_structural_revision: Option<PlaylistStructuralRevision>,
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
}

impl PlaylistUiOutput {
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
    state: &mut PlaylistUiState,
    output: &mut PlaylistUiOutput,
) {
    let Some(model) = model else {
        status::show_unavailable(ui);
        return;
    };
    status::show_summary(ui, model);
    if !ui.is_enabled() {
        // Outgoing/incoming animation copies рисуются в другом egui ID scope.
        // Они не имеют права заменить authoritative viewport anchor или demand hint.
        let mut visual_state = PlaylistUiState::default();
        let mut discarded_output = PlaylistUiOutput::default();
        renderer::show_rows(ui, model, &mut visual_state, &mut discarded_output);
        return;
    }
    renderer::show_rows(ui, model, state, output);
}

#[cfg(test)]
mod tests;
