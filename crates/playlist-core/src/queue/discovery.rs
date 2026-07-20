//! Атомарная вставка уже admitted sibling batch по stable-ID anchor.

use crate::{PlaylistEntryId, PlaylistItemDraft, PlaylistItemId};

use super::structural::StructuralEntryLookupError;
use super::{
    AddItemsError, AllocatedPlaylistItemIds, MAX_PLAYLIST_ITEMS, PlaylistQueue,
    QueueRevisionSnapshot, map_add_allocation_error,
};

/// Stable-ID позиция вставки, не зависящая от текущего числового row index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StableInsertionAnchor {
    before_entry_id: Option<PlaylistEntryId>,
}

impl StableInsertionAnchor {
    /// Вставляет batch непосредственно перед committed row.
    #[must_use]
    pub const fn before(entry_id: PlaylistEntryId) -> Self {
        Self {
            before_entry_id: Some(entry_id),
        }
    }

    /// Вставляет batch после последней committed row.
    #[must_use]
    pub const fn at_end() -> Self {
        Self {
            before_entry_id: None,
        }
    }

    /// Stable row, перед которой произошла вставка; `None` означает canonical end.
    #[must_use]
    pub const fn before_entry_id(self) -> Option<PlaylistEntryId> {
        self.before_entry_id
    }
}

/// Успешный exact outcome одной discovery domain mutation.
#[derive(Debug, PartialEq, Eq)]
pub struct DiscoveryBatchInsertOutcome {
    /// IDs выделены только внутри успешного commit-а и соответствуют входным drafts.
    pub item_ids: AllocatedPlaylistItemIds,
    /// Anchor, относительно которого UI может сохранить viewport identity.
    pub anchor: StableInsertionAnchor,
}

/// Typed preflight/commit failure без allocator/queue mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoveryBatchInsertError {
    /// Caller продолжает уже устаревший discovery scope.
    RevisionMismatch {
        expected: QueueRevisionSnapshot,
        actual: QueueRevisionSnapshot,
    },
    /// Stable anchor был удалён structural mutation-ом.
    AnchorNotCommitted { entry_id: PlaylistEntryId },
    /// Caller передал subordinate part вместо owning compound anchor.
    CompoundPartAnchor {
        part_item_id: PlaylistItemId,
        compound_entry_id: PlaylistEntryId,
    },
    /// Общий atomic add preflight отклонил batch.
    Add(AddItemsError),
}

impl PlaylistQueue {
    /// Атомарно вставляет ID-less drafts перед stable anchor и выполняет D14b merge один раз.
    pub fn insert_discovery_batch(
        &mut self,
        expected_revision: QueueRevisionSnapshot,
        anchor: StableInsertionAnchor,
        drafts: Vec<PlaylistItemDraft>,
    ) -> Result<DiscoveryBatchInsertOutcome, DiscoveryBatchInsertError> {
        let mut random = rand::rng();
        self.insert_discovery_batch_with_rng(expected_revision, anchor, drafts, &mut random)
    }

    /// Injectable RNG вариант для детерминированной проверки responsive shuffle.
    pub fn insert_discovery_batch_with_rng<R: rand::Rng + ?Sized>(
        &mut self,
        expected_revision: QueueRevisionSnapshot,
        anchor: StableInsertionAnchor,
        drafts: Vec<PlaylistItemDraft>,
        random: &mut R,
    ) -> Result<DiscoveryBatchInsertOutcome, DiscoveryBatchInsertError> {
        if self.active_reservation.is_some() {
            return Err(DiscoveryBatchInsertError::Add(
                AddItemsError::InstallCommitLinearizing,
            ));
        }
        let actual_revision = self.revision_snapshot();
        if actual_revision.structural() != expected_revision.structural() {
            return Err(DiscoveryBatchInsertError::RevisionMismatch {
                expected: expected_revision,
                actual: actual_revision,
            });
        }
        if drafts.is_empty() {
            return Ok(DiscoveryBatchInsertOutcome {
                item_ids: AllocatedPlaylistItemIds(Vec::new()),
                anchor,
            });
        }
        let insertion_index = match anchor.before_entry_id {
            Some(entry_id) => match self.resolve_top_level_entry_index(entry_id) {
                Ok(entry_index) => entry_index,
                Err(StructuralEntryLookupError::NotFound) => {
                    return Err(DiscoveryBatchInsertError::AnchorNotCommitted { entry_id });
                }
                Err(StructuralEntryLookupError::CompoundPart {
                    part_item_id,
                    compound_entry_id,
                }) => {
                    return Err(DiscoveryBatchInsertError::CompoundPartAnchor {
                        part_item_id,
                        compound_entry_id,
                    });
                }
            },
            None => self.entries.len(),
        };
        let requested = drafts.len();
        self.retained_item_count()
            .checked_add(requested)
            .filter(|resulting_len| *resulting_len <= MAX_PLAYLIST_ITEMS)
            .ok_or(DiscoveryBatchInsertError::Add(
                AddItemsError::CapacityExceeded {
                    current: self.retained_item_count(),
                    requested,
                    maximum: MAX_PLAYLIST_ITEMS,
                },
            ))?;
        let next_structural_revision =
            self.structural_revision
                .checked_next()
                .ok_or(DiscoveryBatchInsertError::Add(
                    AddItemsError::StructuralRevisionExhausted,
                ))?;
        let allocation_plan = self
            .item_id_allocator
            .preflight_allocation(requested, &self.existing_item_ids())
            .map_err(map_add_allocation_error)
            .map_err(DiscoveryBatchInsertError::Add)?;
        let allocated_item_ids = allocation_plan.allocated_item_ids.clone();
        let committed_items = drafts
            .into_iter()
            .zip(allocated_item_ids.iter().copied())
            .map(|(draft, item_id)| draft.into_item(item_id));
        let allocated_entry_ids = allocated_item_ids
            .iter()
            .copied()
            .map(PlaylistEntryId::Single)
            .collect::<Vec<_>>();

        if let Some(shuffle_traversal) = &mut self.shuffle_traversal {
            shuffle_traversal.merge_new_entries(&allocated_entry_ids, random);
        }
        self.item_id_allocator.commit_allocation(&allocation_plan);
        self.entries.splice(
            insertion_index..insertion_index,
            committed_items.map(crate::PlaylistEntry::Single),
        );
        self.structural_revision = next_structural_revision;

        Ok(DiscoveryBatchInsertOutcome {
            item_ids: AllocatedPlaylistItemIds(allocated_item_ids),
            anchor,
        })
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;

    use super::*;
    use crate::{CachedPlaylistMetadata, LocalLocator, PlaylistMediaKind};

    fn draft(name: &str) -> PlaylistItemDraft {
        PlaylistItemDraft::local(
            LocalLocator::Native(name.into()),
            None,
            CachedPlaylistMetadata::new(name, PlaylistMediaKind::Video),
        )
    }

    fn append_id(queue: &mut PlaylistQueue, name: &str) -> PlaylistItemId {
        match queue.append_one(draft(name)).expect("item commit") {
            super::super::AddItemsOutcome::Added(ids) => ids.into_vec()[0],
            super::super::AddItemsOutcome::NoItemsProvided => panic!("one draft is non-empty"),
        }
    }

    #[test]
    fn batch_is_atomic_and_ids_follow_natural_anchor_order() {
        let mut queue = PlaylistQueue::new();
        let target_id = append_id(&mut queue, "target");
        let revision = queue.revision_snapshot();
        let mut random = rand::rngs::StdRng::seed_from_u64(7);

        let outcome = queue
            .insert_discovery_batch_with_rng(
                revision,
                StableInsertionAnchor::before(crate::PlaylistEntryId::Single(target_id)),
                vec![draft("before-2"), draft("before-1")],
                &mut random,
            )
            .expect("batch commit");

        assert_eq!(outcome.item_ids.as_slice().len(), 2);
        assert_eq!(queue.iter_playable_ids().nth(2), Some(target_id));
        assert_eq!(
            queue.next_item_id_snapshot().expose_value_for_persistence(),
            4
        );
    }

    #[test]
    fn stale_revision_and_missing_anchor_do_not_consume_ids() {
        let mut queue = PlaylistQueue::new();
        let target_id = append_id(&mut queue, "target");
        let stale = queue.revision_snapshot();
        queue
            .append_one(draft("other"))
            .expect("structural mutation");
        let watermark = queue.next_item_id_snapshot();

        assert!(matches!(
            queue.insert_discovery_batch(
                stale,
                StableInsertionAnchor::before(crate::PlaylistEntryId::Single(target_id)),
                vec![draft("late")],
            ),
            Err(DiscoveryBatchInsertError::RevisionMismatch { .. })
        ));
        assert_eq!(queue.next_item_id_snapshot(), watermark);
    }
}
