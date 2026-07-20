//! Group-safe bulk removal with exact shuffle reference cleanup.

use std::collections::{HashMap, HashSet};

use crate::{PlaylistEntry, PlaylistEntryId};

use super::super::structural::StructuralEntryLookupError;
use super::super::{PlaylistQueue, TraversalCurrentEffect, TraversalCurrentItemId};
use super::{BulkRemoveError, BulkRemoveOutcome};

impl PlaylistQueue {
    /// Удаляет requested top-level entries одним retain и traversal rebuild.
    pub fn remove_batch(
        &mut self,
        requested_entry_ids: &[PlaylistEntryId],
    ) -> Result<BulkRemoveOutcome, BulkRemoveError> {
        if self.active_reservation.is_some() {
            return Err(BulkRemoveError::InstallCommitLinearizing);
        }
        if requested_entry_ids.is_empty() {
            return Ok(BulkRemoveOutcome::NoItemsRequested);
        }
        let requested: HashSet<_> = requested_entry_ids.iter().copied().collect();
        let committed_entry_ids = self
            .entries
            .iter()
            .map(PlaylistEntry::entry_id)
            .collect::<HashSet<_>>();
        let compound_entry_by_part = self
            .entries
            .iter()
            .filter_map(PlaylistEntry::as_compound)
            .flat_map(|group| {
                let compound_entry_id = PlaylistEntryId::Compound(group.group_id());
                group
                    .parts()
                    .map(move |part| (part.item().item_id(), compound_entry_id))
            })
            .collect::<HashMap<_, _>>();
        let mut committed_to_remove = HashSet::with_capacity(requested.len());
        for entry_id in requested {
            if committed_entry_ids.contains(&entry_id) {
                committed_to_remove.insert(entry_id);
                continue;
            }
            if let PlaylistEntryId::Single(part_item_id) = entry_id
                && let Some(compound_entry_id) = compound_entry_by_part.get(&part_item_id)
            {
                return Err(BulkRemoveError::CompoundPartTarget {
                    part_item_id,
                    compound_entry_id: *compound_entry_id,
                });
            }
        }
        if committed_to_remove.is_empty() {
            return Ok(BulkRemoveOutcome::NoMatchingItems);
        }
        self.commit_bulk_remove(&committed_to_remove)
    }

    /// `Remove Others` сохраняет exact top-level identity одним bulk commit.
    pub fn remove_others(
        &mut self,
        retained_entry_id: PlaylistEntryId,
    ) -> Result<BulkRemoveOutcome, BulkRemoveError> {
        if self.active_reservation.is_some() {
            return Err(BulkRemoveError::InstallCommitLinearizing);
        }
        match self.resolve_top_level_entry_index(retained_entry_id) {
            Ok(_) => {}
            Err(StructuralEntryLookupError::NotFound) => {
                return Ok(BulkRemoveOutcome::NoMatchingItems);
            }
            Err(StructuralEntryLookupError::CompoundPart {
                part_item_id,
                compound_entry_id,
            }) => {
                return Err(BulkRemoveError::CompoundPartTarget {
                    part_item_id,
                    compound_entry_id,
                });
            }
        }
        let committed_to_remove: HashSet<_> = self
            .entries
            .iter()
            .map(PlaylistEntry::entry_id)
            .filter(|entry_id| *entry_id != retained_entry_id)
            .collect();
        if committed_to_remove.is_empty() {
            return Ok(BulkRemoveOutcome::NoMatchingItems);
        }
        self.commit_bulk_remove(&committed_to_remove)
    }

    /// Общий preflight/commit публикует removal ровно одной revision.
    fn commit_bulk_remove(
        &mut self,
        committed_entries_to_remove: &HashSet<PlaylistEntryId>,
    ) -> Result<BulkRemoveOutcome, BulkRemoveError> {
        let mut committed_item_ids_to_remove = HashSet::new();
        for entry in self
            .entries
            .iter()
            .filter(|entry| committed_entries_to_remove.contains(&entry.entry_id()))
        {
            match entry {
                PlaylistEntry::Single(item) => {
                    committed_item_ids_to_remove.insert(item.item_id());
                }
                PlaylistEntry::Compound(group) => {
                    committed_item_ids_to_remove
                        .extend(group.parts().map(|part| part.item().item_id()));
                }
            }
        }
        let next_structural_revision = self
            .structural_revision
            .checked_next()
            .ok_or(BulkRemoveError::StructuralRevisionExhausted)?;
        let clears_current = self
            .traversal_current
            .is_some_and(|current| committed_item_ids_to_remove.contains(&current.item_id()));
        let removed_current_item_id = self
            .traversal_current
            .map(TraversalCurrentItemId::item_id)
            .filter(|_| clears_current);
        let next_traversal_revision = clears_current
            .then(|| {
                self.traversal_revision
                    .checked_next()
                    .ok_or(BulkRemoveError::TraversalRevisionExhausted)
            })
            .transpose()?;
        let remaining_canonical_item_ids: Vec<_> = self
            .iter_playable_ids()
            .filter(|item_id| !committed_item_ids_to_remove.contains(item_id))
            .collect();
        if let Some(shuffle_traversal) = &mut self.shuffle_traversal {
            shuffle_traversal.remove_items(
                &committed_item_ids_to_remove,
                &remaining_canonical_item_ids,
                clears_current,
            );
        }
        self.entries
            .retain(|entry| !committed_entries_to_remove.contains(&entry.entry_id()));
        self.structural_revision = next_structural_revision;
        let traversal_current_effect = if clears_current {
            self.traversal_current = None;
            self.traversal_revision =
                next_traversal_revision.expect("preflighted traversal revision");
            TraversalCurrentEffect::Cleared
        } else {
            TraversalCurrentEffect::Preserved
        };
        let current_outcome = match removed_current_item_id {
            Some(removed_item_id) => crate::RemovalCurrentOutcome::Detached { removed_item_id },
            None => crate::RemovalCurrentOutcome::Preserved(self.traversal_current),
        };
        Ok(BulkRemoveOutcome::Removed {
            removed_item_count: committed_item_ids_to_remove.len(),
            traversal_current_effect,
            current_outcome,
        })
    }
}
