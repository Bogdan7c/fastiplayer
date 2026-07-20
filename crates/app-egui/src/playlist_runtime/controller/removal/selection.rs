//! Selection-to-structural-target preflight for destructive bulk actions.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use playlist_core::{PlaylistEntry, PlaylistEntryId, PlaylistItemId};

use super::{ControllerDestructiveRemovalOutcome, PlaylistController};
use crate::playlist_runtime::PlaylistStructuralRevision;

/// Preflight связывает flat selection membership с explicit structural targets.
pub(super) struct PreflightExactRemoval {
    /// Exact playable identities нужны selection/tombstone policy.
    pub(super) item_ids: HashSet<PlaylistItemId>,
    /// Domain mutation получает только validated top-level identities.
    pub(super) entry_ids: Arc<[PlaylistEntryId]>,
}

impl PlaylistController {
    /// Проверяет revision, uniqueness, membership и полное покрытие compounds.
    pub(super) fn preflight_exact_removal_ids(
        &self,
        item_ids: &[PlaylistItemId],
        expected_structural_revision: PlaylistStructuralRevision,
    ) -> Result<PreflightExactRemoval, ControllerDestructiveRemovalOutcome> {
        if expected_structural_revision != self.structural_revision {
            return Err(ControllerDestructiveRemovalOutcome::StaleStructuralRevision);
        }
        if item_ids.is_empty() {
            return Err(ControllerDestructiveRemovalOutcome::NoChange);
        }
        let mut entry_id_by_item = HashMap::with_capacity(self.queue.retained_item_count());
        let mut part_ids_by_compound = HashMap::new();
        for entry in self.queue.iter_top_level_entries() {
            match entry {
                PlaylistEntry::Single(item) => {
                    entry_id_by_item.insert(item.item_id(), entry.entry_id());
                }
                PlaylistEntry::Compound(group) => {
                    let entry_id = entry.entry_id();
                    let part_ids = group
                        .parts()
                        .map(|part| part.item().item_id())
                        .collect::<Vec<_>>();
                    entry_id_by_item
                        .extend(part_ids.iter().copied().map(|item_id| (item_id, entry_id)));
                    part_ids_by_compound.insert(entry_id, part_ids);
                }
            }
        }
        let mut exact_item_ids = HashSet::with_capacity(item_ids.len());
        for item_id in item_ids {
            if !exact_item_ids.insert(*item_id) {
                return Err(ControllerDestructiveRemovalOutcome::DuplicateItemId {
                    item_id: *item_id,
                });
            }
            if !entry_id_by_item.contains_key(item_id) {
                return Err(ControllerDestructiveRemovalOutcome::NotFound { item_id: *item_id });
            }
        }
        let mut entry_ids = Vec::new();
        for item_id in item_ids {
            let entry_id = entry_id_by_item[item_id];
            if let Some(part_ids) = part_ids_by_compound.get(&entry_id)
                && part_ids
                    .iter()
                    .any(|part_item_id| !exact_item_ids.contains(part_item_id))
            {
                return Err(
                    ControllerDestructiveRemovalOutcome::PartialCompoundSelection {
                        compound_entry_id: entry_id,
                    },
                );
            }
            if !entry_ids.contains(&entry_id) {
                entry_ids.push(entry_id);
            }
        }
        Ok(PreflightExactRemoval {
            item_ids: exact_item_ids,
            entry_ids: entry_ids.into(),
        })
    }
}
