//! Selection-to-structural-target preflight for destructive bulk actions.

use std::collections::HashSet;
use std::sync::Arc;

use playlist_core::PlaylistEntryId;

use super::{ControllerDestructiveRemovalOutcome, PlaylistController};
use crate::playlist_runtime::PlaylistStructuralRevision;

impl PlaylistController {
    /// Проверяет revision, uniqueness и top-level membership без part-to-group inference.
    pub(super) fn preflight_exact_removal_entries(
        &self,
        entry_ids: &[PlaylistEntryId],
        expected_structural_revision: PlaylistStructuralRevision,
    ) -> Result<Arc<[PlaylistEntryId]>, ControllerDestructiveRemovalOutcome> {
        if expected_structural_revision != self.structural_revision {
            return Err(ControllerDestructiveRemovalOutcome::StaleStructuralRevision);
        }
        if entry_ids.is_empty() {
            return Err(ControllerDestructiveRemovalOutcome::NoChange);
        }
        let mut exact_entry_ids = HashSet::with_capacity(entry_ids.len());
        for entry_id in entry_ids {
            if !exact_entry_ids.insert(*entry_id) {
                return Err(ControllerDestructiveRemovalOutcome::DuplicateEntryId {
                    entry_id: *entry_id,
                });
            }
            if self.queue.top_level_entry(*entry_id).is_none() {
                return Err(ControllerDestructiveRemovalOutcome::EntryNotFound {
                    entry_id: *entry_id,
                });
            }
        }
        Ok(Arc::from(entry_ids))
    }
}
