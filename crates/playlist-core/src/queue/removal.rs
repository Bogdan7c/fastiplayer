//! Opaque runtime-only snapshot для destructive removal Undo.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use crate::{PlaylistEntry, PlaylistEntryId, PlaylistItemId, TraversalCurrentItemId};

use super::structural::StructuralEntryLookupError;
use super::{PlaylistQueue, QueueRevision, RemoveItemOutcome, TraversalCurrentEffect};

#[cfg(test)]
mod tests;

/// Typed D71-исход для persisted traversal current после removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalCurrentOutcome {
    /// Removal не затронул committed current.
    Preserved(Option<TraversalCurrentItemId>),
    /// Удалённый current отсоединён без назначения successor-а.
    Detached { removed_item_id: PlaylistItemId },
}

impl PlaylistQueue {
    /// Удаляет exact top-level identity, не выбирая successor автоматически.
    pub fn remove(&mut self, entry_id: PlaylistEntryId) -> RemoveItemOutcome {
        if self.active_reservation.is_some() {
            return RemoveItemOutcome::InstallCommitLinearizing;
        }
        let entry_index = match self.resolve_top_level_entry_index(entry_id) {
            Ok(entry_index) => entry_index,
            Err(StructuralEntryLookupError::NotFound) => {
                return RemoveItemOutcome::NotFound { entry_id };
            }
            Err(StructuralEntryLookupError::CompoundPart {
                part_item_id,
                compound_entry_id,
            }) => {
                return RemoveItemOutcome::CompoundPartTarget {
                    part_item_id,
                    compound_entry_id,
                };
            }
        };
        let Some(next_structural_revision) = self.structural_revision.checked_next() else {
            return RemoveItemOutcome::StructuralRevisionExhausted;
        };
        let removed_entry = &self.entries[entry_index];
        let removed_item_ids = match removed_entry {
            PlaylistEntry::Single(item) => HashSet::from([item.item_id()]),
            PlaylistEntry::Compound(group) => group
                .parts()
                .map(|part| part.item().item_id())
                .collect::<HashSet<_>>(),
        };
        let removed_current_item_id = self.traversal_current.and_then(|current| {
            removed_item_ids
                .contains(&current.item_id())
                .then_some(current.item_id())
        });
        let clears_current = removed_current_item_id.is_some();
        let next_traversal_revision = if clears_current {
            let Some(next_revision) = self.traversal_revision.checked_next() else {
                return RemoveItemOutcome::TraversalRevisionExhausted;
            };
            Some(next_revision)
        } else {
            None
        };

        let remaining_canonical_entry_ids: Vec<_> = self
            .iter_top_level_entry_ids()
            .filter(|candidate_entry_id| *candidate_entry_id != entry_id)
            .collect();
        if let Some(shuffle_traversal) = &mut self.shuffle_traversal {
            shuffle_traversal.remove_entries_and_items(
                &HashSet::from([entry_id]),
                &removed_item_ids,
                &remaining_canonical_entry_ids,
                clears_current,
            );
        }
        self.entries.remove(entry_index);
        self.structural_revision = next_structural_revision;
        let traversal_current_effect = if clears_current {
            self.traversal_current = None;
            self.traversal_revision =
                next_traversal_revision.expect("preflighted traversal revision");
            TraversalCurrentEffect::Cleared
        } else {
            TraversalCurrentEffect::Preserved
        };

        let current_outcome = if clears_current {
            RemovalCurrentOutcome::Detached {
                removed_item_id: removed_current_item_id
                    .expect("clears_current requires a removed current part"),
            }
        } else {
            RemovalCurrentOutcome::Preserved(self.traversal_current)
        };
        RemoveItemOutcome::Removed {
            entry_id,
            traversal_current_effect,
            current_outcome,
        }
    }
}

/// Immutable pre-mutation snapshot не раскрывает shuffle/allocator storage наружу.
#[derive(Clone)]
pub struct PlaylistRemovalSnapshot {
    entries: Arc<[PlaylistEntry]>,
    item_id_allocator: crate::PlaylistItemIdAllocator,
    compound_group_id_allocator: crate::PlaylistCompoundGroupIdAllocator,
    traversal_current: Option<TraversalCurrentItemId>,
    structural_revision: QueueRevision,
    traversal_revision: QueueRevision,
    metadata_revision: QueueRevision,
    shuffle_traversal: Option<super::shuffle::ShuffleTraversal>,
}

impl fmt::Debug for PlaylistRemovalSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistRemovalSnapshot")
            .field("top_level_entry_count", &self.entries.len())
            .field("traversal_current", &self.traversal_current)
            .finish_non_exhaustive()
    }
}

/// Успешное восстановление является новой domain mutation, а не откатом revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemovalSnapshotRestoreOutcome {
    traversal_current: Option<TraversalCurrentItemId>,
}

impl RemovalSnapshotRestoreOutcome {
    /// Возвращает exact pre-removal current после нового commit-а.
    pub const fn traversal_current(self) -> Option<TraversalCurrentItemId> {
        self.traversal_current
    }
}

/// Почему runtime snapshot нельзя применить поверх изменившейся domain state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalSnapshotRestoreError {
    /// D08 reservation удерживает linearization boundary.
    InstallCommitLinearizing,
    /// После removal произошла другая structural mutation.
    StaleStructuralRevision,
    /// Metadata либо allocator изменились после захвата snapshot.
    StalePersistentState,
    /// Current queue не является exact order-preserving результатом удаления из snapshot.
    NotRemovalResult,
    /// Новую structural/traversal revision невозможно выделить.
    RevisionExhausted,
}

impl PlaylistQueue {
    /// Захватывает bounded shared snapshot до destructive mutation.
    pub fn capture_removal_snapshot(&self) -> PlaylistRemovalSnapshot {
        PlaylistRemovalSnapshot {
            // Clone копирует только lightweight handles: payload каждой строки остаётся в Arc.
            entries: Arc::from(self.entries.clone()),
            item_id_allocator: self.item_id_allocator.clone(),
            compound_group_id_allocator: self.compound_group_id_allocator.clone(),
            traversal_current: self.traversal_current,
            structural_revision: self.structural_revision,
            traversal_revision: self.traversal_revision,
            metadata_revision: self.metadata_revision,
            shuffle_traversal: self.shuffle_traversal.clone(),
        }
    }

    /// Восстанавливает snapshot как новую mutation и запрещает revision/allocator regression.
    pub fn restore_removal_snapshot(
        &mut self,
        snapshot: PlaylistRemovalSnapshot,
    ) -> Result<RemovalSnapshotRestoreOutcome, RemovalSnapshotRestoreError> {
        if self.active_reservation.is_some() {
            return Err(RemovalSnapshotRestoreError::InstallCommitLinearizing);
        }
        if self.structural_revision
            != snapshot
                .structural_revision
                .checked_next()
                .ok_or(RemovalSnapshotRestoreError::RevisionExhausted)?
        {
            return Err(RemovalSnapshotRestoreError::StaleStructuralRevision);
        }
        if self.metadata_revision != snapshot.metadata_revision
            || self.item_id_allocator != snapshot.item_id_allocator
            || self.compound_group_id_allocator != snapshot.compound_group_id_allocator
        {
            return Err(RemovalSnapshotRestoreError::StalePersistentState);
        }
        let current_entry_ids = self
            .entries
            .iter()
            .map(crate::PlaylistEntry::entry_id)
            .collect::<HashSet<_>>();
        let expected_retained_entries = snapshot
            .entries
            .iter()
            .filter(|entry| current_entry_ids.contains(&entry.entry_id()))
            .cloned()
            .collect::<Vec<_>>();
        if self.entries.len() >= snapshot.entries.len()
            || expected_retained_entries != self.entries
            || current_entry_ids.len() != self.entries.len()
        {
            return Err(RemovalSnapshotRestoreError::NotRemovalResult);
        }
        let snapshot_next_traversal = snapshot.traversal_revision.checked_next();
        if self.traversal_revision != snapshot.traversal_revision
            && Some(self.traversal_revision) != snapshot_next_traversal
        {
            return Err(RemovalSnapshotRestoreError::StalePersistentState);
        }

        let next_structural_revision = self
            .structural_revision
            .checked_next()
            .ok_or(RemovalSnapshotRestoreError::RevisionExhausted)?;
        let traversal_changed = self.traversal_current != snapshot.traversal_current;
        let next_traversal_revision = traversal_changed
            .then(|| {
                self.traversal_revision
                    .checked_next()
                    .ok_or(RemovalSnapshotRestoreError::RevisionExhausted)
            })
            .transpose()?;

        self.entries = snapshot.entries.iter().cloned().collect();
        self.traversal_current = snapshot.traversal_current;
        self.shuffle_traversal = snapshot.shuffle_traversal;
        self.structural_revision = next_structural_revision;
        if let Some(next_revision) = next_traversal_revision {
            self.traversal_revision = next_revision;
        }

        Ok(RemovalSnapshotRestoreOutcome {
            traversal_current: self.traversal_current,
        })
    }
}

#[cfg(test)]
impl PlaylistRemovalSnapshot {
    /// Доказывает, что snapshot делит locator/metadata allocation с исходной queue.
    pub(crate) fn shares_item_payload_with(
        &self,
        queue: &PlaylistQueue,
        item_id: PlaylistItemId,
    ) -> bool {
        let snapshot_item = self.entries.iter().find_map(|entry| entry.item(item_id));
        let queue_item = queue.item(item_id);
        snapshot_item
            .zip(queue_item)
            .is_some_and(|(snapshot_item, queue_item)| {
                snapshot_item.shares_payload_with(queue_item)
            })
    }
}
