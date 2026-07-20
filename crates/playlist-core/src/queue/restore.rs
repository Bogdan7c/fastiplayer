//! Serde-neutral exact persistence restore owner.

use std::collections::HashSet;
use std::fmt;

use crate::{
    AllocatorRestoreError, CompoundGroupAllocatorRestoreError, MAX_PLAYLIST_ITEMS,
    NextPlaylistCompoundGroupId, NextPlaylistItemId, PlaylistCompoundGroupId,
    PlaylistCompoundGroupIdAllocator, PlaylistItemId, PlaylistItemIdAllocator,
    RestoredPlaylistEntry, RestoredPlaylistItem,
};

use super::{PlaylistQueue, QueueRevision, TraversalCurrentItemId};

/// Input полного persistence restore без serde либо I/O dependency.
pub struct PlaylistQueueRestore {
    restored_entries: Vec<RestoredPlaylistEntry>,
    next_item_id: NextPlaylistItemId,
    next_compound_group_id: NextPlaylistCompoundGroupId,
    traversal_current_item_id: Option<PlaylistItemId>,
}

impl PlaylistQueueRestore {
    /// Собирает legacy DTO-mapped Singles restore.
    pub fn new(
        restored_items: Vec<RestoredPlaylistItem>,
        next_item_id: NextPlaylistItemId,
        traversal_current_item_id: Option<PlaylistItemId>,
    ) -> Self {
        Self {
            restored_entries: restored_items
                .into_iter()
                .map(RestoredPlaylistEntry::Single)
                .collect(),
            next_item_id,
            next_compound_group_id: NextPlaylistCompoundGroupId::initial(),
            traversal_current_item_id,
        }
    }

    /// Собирает schema-v2 restore с exact top-level entries и обоими watermarks.
    pub fn from_entries(
        restored_entries: Vec<RestoredPlaylistEntry>,
        next_item_id: NextPlaylistItemId,
        next_compound_group_id: NextPlaylistCompoundGroupId,
        traversal_current_item_id: Option<PlaylistItemId>,
    ) -> Self {
        Self {
            restored_entries,
            next_item_id,
            next_compound_group_id,
            traversal_current_item_id,
        }
    }
}

impl fmt::Debug for PlaylistQueueRestore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistQueueRestore")
            .field("restored_entry_count", &self.restored_entries.len())
            .field(
                "restored_item_count",
                &self
                    .restored_entries
                    .iter()
                    .map(RestoredPlaylistEntry::retained_item_count)
                    .sum::<usize>(),
            )
            .field("next_item_id", &self.next_item_id)
            .field("next_compound_group_id", &self.next_compound_group_id)
            .field("traversal_current_item_id", &self.traversal_current_item_id)
            .finish()
    }
}

/// Ошибка restore полного queue snapshot.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum QueueRestoreError {
    CapacityExceeded { restored: usize, maximum: usize },
    DuplicateItemId { item_id: PlaylistItemId },
    DuplicateCompoundGroupId { group_id: PlaylistCompoundGroupId },
    InvalidAllocator(AllocatorRestoreError),
    InvalidCompoundGroupAllocator(CompoundGroupAllocatorRestoreError),
    CurrentItemNotCommitted { item_id: PlaylistItemId },
}

impl fmt::Debug for QueueRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapacityExceeded { restored, maximum } => formatter
                .debug_struct("CapacityExceeded")
                .field("restored", restored)
                .field("maximum", maximum)
                .finish(),
            Self::DuplicateItemId { item_id } => formatter
                .debug_struct("DuplicateItemId")
                .field("item_id", item_id)
                .finish(),
            Self::DuplicateCompoundGroupId { group_id } => formatter
                .debug_struct("DuplicateCompoundGroupId")
                .field("group_id", group_id)
                .finish(),
            Self::InvalidAllocator(error) => formatter
                .debug_tuple("InvalidAllocator")
                .field(error)
                .finish(),
            Self::InvalidCompoundGroupAllocator(error) => formatter
                .debug_tuple("InvalidCompoundGroupAllocator")
                .field(error)
                .finish(),
            Self::CurrentItemNotCommitted { item_id } => formatter
                .debug_struct("CurrentItemNotCommitted")
                .field("item_id", item_id)
                .finish(),
        }
    }
}

impl fmt::Display for QueueRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapacityExceeded { maximum, .. } => {
                write!(formatter, "restored queue превышает лимит {maximum}")
            }
            Self::DuplicateItemId { item_id } => {
                write!(formatter, "restored queue повторяет {item_id}")
            }
            Self::DuplicateCompoundGroupId { group_id } => {
                write!(formatter, "restored queue повторяет {group_id}")
            }
            Self::InvalidAllocator(error) => fmt::Display::fmt(error, formatter),
            Self::InvalidCompoundGroupAllocator(error) => fmt::Display::fmt(error, formatter),
            Self::CurrentItemNotCommitted { item_id } => {
                write!(formatter, "restored current {item_id} отсутствует в queue")
            }
        }
    }
}

impl std::error::Error for QueueRestoreError {}

impl PlaylistQueue {
    /// Атомарно валидирует entries, memberships, IDs, current и оба allocator-а.
    pub fn restore(snapshot: PlaylistQueueRestore) -> Result<Self, QueueRestoreError> {
        let restored_item_count = snapshot
            .restored_entries
            .iter()
            .try_fold(0usize, |count, entry| {
                count.checked_add(entry.retained_item_count())
            })
            .ok_or(QueueRestoreError::CapacityExceeded {
                restored: usize::MAX,
                maximum: MAX_PLAYLIST_ITEMS,
            })?;
        if restored_item_count > MAX_PLAYLIST_ITEMS {
            return Err(QueueRestoreError::CapacityExceeded {
                restored: restored_item_count,
                maximum: MAX_PLAYLIST_ITEMS,
            });
        }

        let mut restored_item_ids = Vec::with_capacity(restored_item_count);
        for restored_entry in &snapshot.restored_entries {
            restored_entry.extend_item_ids(&mut restored_item_ids);
        }
        let mut unique_item_ids = HashSet::with_capacity(restored_item_ids.len());
        for item_id in restored_item_ids.iter().copied() {
            if !unique_item_ids.insert(item_id) {
                return Err(QueueRestoreError::DuplicateItemId { item_id });
            }
        }

        let item_id_allocator =
            PlaylistItemIdAllocator::restore(snapshot.next_item_id, &restored_item_ids)
                .map_err(QueueRestoreError::InvalidAllocator)?;
        let restored_group_ids = snapshot
            .restored_entries
            .iter()
            .filter_map(RestoredPlaylistEntry::compound_group_id)
            .collect::<Vec<PlaylistCompoundGroupId>>();
        let mut unique_group_ids = HashSet::with_capacity(restored_group_ids.len());
        for group_id in restored_group_ids.iter().copied() {
            if !unique_group_ids.insert(group_id) {
                return Err(QueueRestoreError::DuplicateCompoundGroupId { group_id });
            }
        }
        let compound_group_id_allocator = PlaylistCompoundGroupIdAllocator::restore(
            snapshot.next_compound_group_id,
            &restored_group_ids,
        )
        .map_err(QueueRestoreError::InvalidCompoundGroupAllocator)?;
        let traversal_current = match snapshot.traversal_current_item_id {
            Some(item_id) if unique_item_ids.contains(&item_id) => {
                Some(TraversalCurrentItemId(item_id))
            }
            Some(item_id) => return Err(QueueRestoreError::CurrentItemNotCommitted { item_id }),
            None => None,
        };
        let entries = snapshot
            .restored_entries
            .into_iter()
            .map(RestoredPlaylistEntry::into_entry)
            .collect();

        Ok(Self {
            entries,
            item_id_allocator,
            compound_group_id_allocator,
            traversal_current,
            structural_revision: QueueRevision::INITIAL,
            traversal_revision: QueueRevision::INITIAL,
            metadata_revision: QueueRevision::INITIAL,
            active_reservation: None,
            shuffle_traversal: None,
        })
    }
}
