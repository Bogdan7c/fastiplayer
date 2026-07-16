//! Runtime adapters для row selection, explicit Play и canonical reorder.

use playlist_core::{MoveItemIntent, PlaylistItemId};

use super::controller::{ControllerMoveItemOutcome, ControllerPlayItemOutcome};
use super::{PlaylistRuntime, TransportActionOrigin};

/// Load gate остаётся отдельным состоянием, а не маскируется как stale Item ID.
pub(crate) enum RuntimeRowPlayOutcome {
    Controller(ControllerPlayItemOutcome),
    LoadDecisionPending,
}

/// Runtime move outcome сохраняет все controller/domain distinctions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeMoveItemOutcome {
    Controller(ControllerMoveItemOutcome),
    LoadDecisionPending,
}

impl PlaylistRuntime {
    /// Selection меняет только controller presentation snapshot.
    pub(crate) fn select_playlist_row(&mut self, item_id: Option<PlaylistItemId>) -> bool {
        self.controller
            .as_mut()
            .is_some_and(|controller| controller.select_row(item_id))
    }

    /// Explicit row Play supersede-ит только replacement prompt и использует D59/D60 controller.
    pub(crate) fn play_playlist_row(&mut self, item_id: PlaylistItemId) -> RuntimeRowPlayOutcome {
        self.supersede_queue_replacement_confirmation_for_row_play();
        let Some(controller) = self.controller.as_mut() else {
            return RuntimeRowPlayOutcome::LoadDecisionPending;
        };
        RuntimeRowPlayOutcome::Controller(controller.play_item(item_id, TransportActionOrigin::Ui))
    }

    /// Одна UI drop-команда становится не более чем одной persistent mutation.
    pub(crate) fn move_playlist_item(
        &mut self,
        item_id: PlaylistItemId,
        intent: MoveItemIntent,
    ) -> RuntimeMoveItemOutcome {
        let Some(controller) = self.controller.as_mut() else {
            return RuntimeMoveItemOutcome::LoadDecisionPending;
        };
        let dirty_before = controller.dirty_revision();
        let outcome = controller.move_item(item_id, intent);
        if matches!(outcome, ControllerMoveItemOutcome::Moved { .. }) {
            self.removal_undo = None;
        }
        self.publish_controller_mutation_if_dirty(dirty_before);
        RuntimeMoveItemOutcome::Controller(outcome)
    }
}
