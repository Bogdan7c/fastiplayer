//! Runtime adapters для row selection, explicit Play и canonical reorder.

use std::sync::Arc;

use playlist_core::{MoveItemIntent, PlaylistItemId};

use super::controller::{ControllerMoveItemsOutcome, ControllerPlayItemOutcome};
use super::{
    PlaylistRuntime, PlaylistStructuralRevision, TransportActionOrigin, UpdateSelection,
    UpdateSelectionOutcome,
};

/// Load gate остаётся отдельным состоянием, а не маскируется как stale Item ID.
pub(crate) enum RuntimeRowPlayOutcome {
    Controller(ControllerPlayItemOutcome),
    LoadDecisionPending,
}

/// Runtime selection outcome сохраняет startup load-gate отдельно от controller no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeUpdateSelectionOutcome {
    Controller(UpdateSelectionOutcome),
    LoadDecisionPending,
}

/// Runtime group move сохраняет load-gate отдельно от controller/domain outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeMoveItemsOutcome {
    Controller(ControllerMoveItemsOutcome),
    LoadDecisionPending,
}

impl PlaylistRuntime {
    /// Exact selection action меняет только process-lifetime presentation state.
    pub(crate) fn update_playlist_selection(
        &mut self,
        update: UpdateSelection,
    ) -> RuntimeUpdateSelectionOutcome {
        let Some(controller) = self.controller.as_mut() else {
            return RuntimeUpdateSelectionOutcome::LoadDecisionPending;
        };
        RuntimeUpdateSelectionOutcome::Controller(controller.update_selection(update))
    }

    /// Explicit row Play supersede-ит только replacement prompt и использует D59/D60 controller.
    pub(crate) fn play_playlist_row(&mut self, item_id: PlaylistItemId) -> RuntimeRowPlayOutcome {
        self.supersede_queue_replacement_confirmation_for_row_play();
        self.discovery.cancel_initial_queue_playback();
        let Some(controller) = self.controller.as_mut() else {
            return RuntimeRowPlayOutcome::LoadDecisionPending;
        };
        RuntimeRowPlayOutcome::Controller(controller.play_item(item_id, TransportActionOrigin::Ui))
    }

    /// Одна group drop-команда становится не более чем одной persistent mutation.
    pub(crate) fn move_playlist_items(
        &mut self,
        item_ids: Arc<[PlaylistItemId]>,
        intent: MoveItemIntent,
        structural_revision: PlaylistStructuralRevision,
    ) -> RuntimeMoveItemsOutcome {
        let Some(controller) = self.controller.as_mut() else {
            return RuntimeMoveItemsOutcome::LoadDecisionPending;
        };
        let dirty_before = controller.dirty_revision();
        let outcome = controller.move_items(item_ids, intent, structural_revision);
        if matches!(outcome, ControllerMoveItemsOutcome::Moved { .. }) {
            self.removal_undo = None;
        }
        self.publish_controller_mutation_if_dirty(dirty_before);
        RuntimeMoveItemsOutcome::Controller(outcome)
    }
}
