//! Read-only virtualized Playlist sidebar.

mod actions;
mod active_accent;
mod header_undo;
pub(crate) mod import_preview;
mod renderer;
mod row_interactions;
mod status;
mod toolbar;
mod virtualized_drag;

pub(crate) use actions::PlaylistAction;
pub(crate) use header_undo::show as show_header_undo;

use std::sync::Arc;

use playlist_core::{PlaylistEntryId, PlaylistItemId};

use crate::playlist_runtime::{
    PlaylistGoCurrentTarget, PlaylistInteractionModel, PlaylistRuntimeBinding,
    PlaylistStructuralRevision, PlaylistViewModel,
};

const MAX_VISIBLE_HINT_ITEMS: usize = 256;

/// UI-owned viewport и decorative accent без controller/playback ownership.
#[derive(Debug, Default)]
pub(crate) struct PlaylistUiState {
    /// Эфемерная геометрия активного акцента принадлежит только UI.
    active_accent: active_accent::ActiveAccentAnimationState,
    /// Единый status owner хранит typed lifetime, deadlines и residual transition.
    status: status::PlaylistStatusLifetimeState,
    viewport_anchor: Option<ViewportAnchor>,
    observed_structural_revision: Option<PlaylistStructuralRevision>,
    go_current: Option<PlaylistGoCurrentTarget>,
    focus_row: Option<PlaylistEntryId>,
    drag: virtualized_drag::VirtualizedDragState,
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
    pub(crate) fn push_action(&mut self, action: PlaylistAction) {
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

    /// D47 focus intent приходит только из controller-provided selected Entry ID.
    pub(crate) fn request_row_focus(&mut self, entry_id: PlaylistEntryId) {
        self.focus_row = Some(entry_id);
    }

    pub(super) fn take_row_focus(&mut self) -> Option<PlaylistEntryId> {
        self.focus_row.take()
    }
}

/// Exact runtime binding не даёт применить hint от stale renderer attachment-а.
#[derive(Debug, Clone)]
pub(crate) struct PlaylistVisibleItemsHint {
    binding: PlaylistRuntimeBinding,
    item_ids: Arc<[PlaylistItemId]>,
}

/// Именованный immutable input одного Playlist render pass.
pub(crate) struct PlaylistShowInput<'a> {
    /// Revision-stable строки могут отсутствовать до runtime binding.
    pub(crate) model: Option<&'a PlaylistViewModel>,
    /// Toolbar/forms/status читают только authoritative interaction snapshot.
    pub(crate) interaction: &'a PlaylistInteractionModel,
    /// Skin-owned визуальные токены строк.
    pub(crate) row_style: crate::ui::skin::PlaylistRowStyle,
    /// Skin-owned геометрия и цвета toolbar.
    pub(crate) toolbar_style: crate::ui::skin::PlaylistToolbarStyle,
    /// Общая typed policy обычного и reduced motion.
    pub(crate) motion: crate::ui::animation::UiMotion,
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
    input: PlaylistShowInput<'_>,
    state: &mut PlaylistUiState,
    output: &mut PlaylistUiOutput,
) {
    // Destructure один раз, чтобы render branches читались на уровне намерений.
    let PlaylistShowInput {
        model,
        interaction,
        row_style,
        toolbar_style,
        motion,
    } = input;
    let Some(model) = model else {
        status::show_unavailable(ui);
        return;
    };
    if !ui.is_enabled() {
        // Outgoing/incoming animation copies рисуются в другом egui ID scope.
        // Они не имеют права вернуть action, заменить viewport anchor или demand hint.
        let mut visual_state = PlaylistUiState::default();
        let mut discarded_output = PlaylistUiOutput::default();
        toolbar::show(ui, interaction, toolbar_style, &mut discarded_output);
        status::show_disabled_copy(ui, state);
        renderer::show_rows(
            ui,
            model,
            row_style,
            motion,
            &mut visual_state,
            &mut discarded_output,
        );
        return;
    }
    toolbar::show(ui, interaction, toolbar_style, output);
    status::show_status(ui, model, interaction, motion, state, output);
    renderer::show_rows(ui, model, row_style, motion, state, output);
}

#[cfg(test)]
mod tests;
