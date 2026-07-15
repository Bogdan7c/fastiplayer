//! Opaque runtime-only snapshot для destructive removal Undo.

use std::fmt;
use std::sync::Arc;

use crate::{PlaylistItem, PlaylistItemId, TraversalCurrentItemId};

use super::{PlaylistQueue, QueueRevision};

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

/// Immutable pre-mutation snapshot не раскрывает shuffle/allocator storage наружу.
#[derive(Clone)]
pub struct PlaylistRemovalSnapshot {
    items: Arc<[PlaylistItem]>,
    item_id_allocator: crate::PlaylistItemIdAllocator,
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
            .field("item_count", &self.items.len())
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
    /// Новую structural/traversal revision невозможно выделить.
    RevisionExhausted,
}

impl PlaylistQueue {
    /// Захватывает bounded shared snapshot до destructive mutation.
    pub fn capture_removal_snapshot(&self) -> PlaylistRemovalSnapshot {
        PlaylistRemovalSnapshot {
            // Clone копирует только lightweight handles: payload каждой строки остаётся в Arc.
            items: Arc::from(self.items.clone()),
            item_id_allocator: self.item_id_allocator.clone(),
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
        {
            return Err(RemovalSnapshotRestoreError::StalePersistentState);
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

        self.items = snapshot.items.iter().cloned().collect();
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
        let snapshot_item = self.items.iter().find(|item| item.item_id() == item_id);
        let queue_item = queue.item(item_id);
        snapshot_item
            .zip(queue_item)
            .is_some_and(|(snapshot_item, queue_item)| {
                snapshot_item.shares_payload_with(queue_item)
            })
    }
}
