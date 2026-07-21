//! Runtime adapters для row selection, explicit Play и canonical reorder.
//!
use std::sync::Arc;

use playlist_core::{MoveItemIntent, PlaylistEntryId, PlaylistItemId};

use super::controller::{ControllerMoveItemsOutcome, ControllerPlayItemOutcome};
use super::{
    CompoundHeaderPlayAction, CompoundHeaderPlayTarget, CompoundPartPlayAction,
    CompoundPartPlayTarget, CompoundRuntimeViewSnapshot, PlaylistRuntime,
    PlaylistStructuralRevision, ToggleCompoundDisclosure, ToggleCompoundDisclosureOutcome,
    TransportActionOrigin, UpdateSelection, UpdateSelectionOutcome,
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

/// Runtime disclosure outcome сохраняет startup load gate отдельно от stale action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeToggleCompoundDisclosureOutcome {
    Controller(ToggleCompoundDisclosureOutcome),
    LoadDecisionPending,
}

/// Header Play вызывает ровно один existing strong-open target.
pub(crate) enum RuntimeCompoundHeaderPlayOutcome {
    Play(RuntimeRowPlayOutcome),
    Rejected(CompoundHeaderPlayTarget),
    LoadDecisionPending,
}

/// Part click сохраняет typed group/part fencing и structural selection.
pub(crate) enum RuntimeCompoundPartPlayOutcome {
    Play(RuntimeRowPlayOutcome),
    Rejected(CompoundPartPlayTarget),
    LoadDecisionPending,
}

impl PlaylistRuntime {
    /// Возвращает immutable compound snapshot того же process-lifetime controller owner-а.
    pub(crate) fn compound_playlist_view_snapshot(
        &self,
    ) -> Option<Arc<CompoundRuntimeViewSnapshot>> {
        self.controller
            .as_ref()
            .map(|controller| controller.compound_view_snapshot())
    }

    /// Disclosure не влияет на persistence revision либо queue capacity.
    pub(crate) fn toggle_compound_disclosure(
        &mut self,
        action: ToggleCompoundDisclosure,
    ) -> RuntimeToggleCompoundDisclosureOutcome {
        let Some(controller) = self.controller.as_mut() else {
            return RuntimeToggleCompoundDisclosureOutcome::LoadDecisionPending;
        };
        RuntimeToggleCompoundDisclosureOutcome::Controller(
            controller.toggle_compound_disclosure(action),
        )
    }

    /// Header target резолвится один раз до существующего exact Play boundary.
    pub(crate) fn play_compound_header(
        &mut self,
        action: CompoundHeaderPlayAction,
    ) -> RuntimeCompoundHeaderPlayOutcome {
        let target = {
            let Some(controller) = self.controller.as_ref() else {
                return RuntimeCompoundHeaderPlayOutcome::LoadDecisionPending;
            };
            controller.compound_header_play_target(action)
        };
        match target {
            CompoundHeaderPlayTarget::ExactItem(item_id) => {
                RuntimeCompoundHeaderPlayOutcome::Play(self.play_playlist_row(item_id))
            }
            rejected => RuntimeCompoundHeaderPlayOutcome::Rejected(rejected),
        }
    }

    /// Part click запускает exact strong-open и не вызывает selection mutation.
    pub(crate) fn play_compound_part(
        &mut self,
        action: CompoundPartPlayAction,
    ) -> RuntimeCompoundPartPlayOutcome {
        let target = {
            let Some(controller) = self.controller.as_ref() else {
                return RuntimeCompoundPartPlayOutcome::LoadDecisionPending;
            };
            controller.compound_part_play_target(action)
        };
        match target {
            CompoundPartPlayTarget::ExactItem(item_id) => {
                RuntimeCompoundPartPlayOutcome::Play(self.play_playlist_row(item_id))
            }
            rejected => RuntimeCompoundPartPlayOutcome::Rejected(rejected),
        }
    }

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
        entry_ids: Arc<[PlaylistEntryId]>,
        intent: MoveItemIntent,
        structural_revision: PlaylistStructuralRevision,
    ) -> RuntimeMoveItemsOutcome {
        let Some(controller) = self.controller.as_mut() else {
            return RuntimeMoveItemsOutcome::LoadDecisionPending;
        };
        let dirty_before = controller.dirty_revision();
        let outcome = controller.move_items(entry_ids, intent, structural_revision);
        if matches!(outcome, ControllerMoveItemsOutcome::Moved { .. }) {
            self.removal_undo = None;
        }
        self.publish_controller_mutation_if_dirty(dirty_before);
        RuntimeMoveItemsOutcome::Controller(outcome)
    }
}
