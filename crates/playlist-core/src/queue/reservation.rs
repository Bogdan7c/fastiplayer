//! Runtime-only D08 reservation lock и opaque prepared mutation token.

use std::collections::HashSet;
use std::fmt;
use std::num::NonZeroUsize;

use crate::id::{ItemIdAllocationError, ItemIdAllocationPlan};
use crate::{PlaylistItem, PlaylistItemDraft, PlaylistItemId};

use super::{
    AllocatedPlaylistItemIds, MAX_PLAYLIST_ITEMS, PlaylistQueue, PrepareReservedMutationError,
    QueueRevision, QueueRevisionSnapshot, ReservedMutationCommit, TraversalCurrentItemId,
    items_from_drafts,
};

/// Opaque intent D08, который полностью проверяется до player authorization.
pub struct ReservedQueueMutation {
    mutation_kind: ReservedQueueMutationKind,
}

/// Private representation не позволяет обходить intent-named constructors.
enum ReservedQueueMutationKind {
    /// Media install выбирает уже committed row и не выдаёт новый ID.
    SelectCommitted { item_id: PlaylistItemId },
    /// Candidate replacement задаёт current структурно, без numeric index API.
    ReplaceWithCurrent {
        /// Drafts перед target в candidate canonical order.
        items_before_current: Vec<PlaylistItemDraft>,
        /// Exact target draft будущего media install.
        current_item: Box<PlaylistItemDraft>,
        /// Drafts после target в candidate canonical order.
        items_after_current: Vec<PlaylistItemDraft>,
    },
}

impl ReservedQueueMutation {
    /// Создаёт intent выбора существующего committed target.
    pub const fn select_committed(item_id: PlaylistItemId) -> Self {
        Self {
            mutation_kind: ReservedQueueMutationKind::SelectCommitted { item_id },
        }
    }

    /// Создаёт replacement candidate, где current нельзя перепутать с индексом.
    pub fn replace_with_current(
        items_before_current: Vec<PlaylistItemDraft>,
        current_item: PlaylistItemDraft,
        items_after_current: Vec<PlaylistItemDraft>,
    ) -> Self {
        Self {
            mutation_kind: ReservedQueueMutationKind::ReplaceWithCurrent {
                items_before_current,
                current_item: Box::new(current_item),
                items_after_current,
            },
        }
    }
}

impl fmt::Debug for ReservedQueueMutation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.mutation_kind {
            ReservedQueueMutationKind::SelectCommitted { item_id } => formatter
                .debug_struct("ReservedQueueMutation::SelectCommitted")
                .field("item_id", item_id)
                .finish(),
            ReservedQueueMutationKind::ReplaceWithCurrent {
                items_before_current,
                items_after_current,
                ..
            } => formatter
                .debug_struct("ReservedQueueMutation::ReplaceWithCurrent")
                .field("items_before_current", &items_before_current.len())
                .field("current_item", &"<redacted-draft>")
                .field("items_after_current", &items_after_current.len())
                .finish(),
        }
    }
}

/// Pointer-stable private identity exact token/lock pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReservationKey(NonZeroUsize);

/// Opaque one-shot token: future IDs остаются private до `commit_reserved`.
#[must_use = "reservation token нужно exact commit-нуть либо abort-нуть"]
pub struct PreparedQueueMutationToken {
    key: ReservationKey,
    prepared_mutation: Box<PreparedMutation>,
}

impl fmt::Debug for PreparedQueueMutationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreparedQueueMutationToken(<opaque>)")
    }
}

/// Полностью проверенная mutation без remaining business-error branches.
enum PreparedMutation {
    SelectCommitted {
        traversal_current: TraversalCurrentItemId,
        traversal_revision_after_commit: Option<QueueRevision>,
    },
    ReplaceWithCurrent {
        replacement_items: Vec<PlaylistItem>,
        allocation_plan: ItemIdAllocationPlan,
        traversal_current: TraversalCurrentItemId,
        structural_revision_after_commit: QueueRevision,
        traversal_revision_after_commit: QueueRevision,
    },
}

impl PlaylistQueue {
    /// Выполняет все fallible checks и только затем устанавливает один lock.
    pub fn prepare_reserved_mutation(
        &mut self,
        expected_revision: QueueRevisionSnapshot,
        mutation: ReservedQueueMutation,
    ) -> Result<PreparedQueueMutationToken, PrepareReservedMutationError> {
        if self.active_reservation.is_some() {
            return Err(PrepareReservedMutationError::InstallCommitLinearizing);
        }

        let actual_revision = self.revision_snapshot();
        if !expected_revision.same_reservation_preconditions(actual_revision) {
            return Err(PrepareReservedMutationError::RevisionMismatch {
                expected: expected_revision,
                actual: actual_revision,
            });
        }

        let prepared_mutation = match mutation.mutation_kind {
            ReservedQueueMutationKind::SelectCommitted { item_id } => {
                let traversal_current = self
                    .validate_traversal_current(item_id)
                    .map_err(|_| PrepareReservedMutationError::ItemNotCommitted { item_id })?;
                let records_same_item_shuffle_visit = self.shuffle_traversal.is_some()
                    && self.traversal_current == Some(traversal_current);
                let traversal_revision_after_commit = if self.traversal_current
                    == Some(traversal_current)
                    && !records_same_item_shuffle_visit
                {
                    None
                } else {
                    Some(
                        self.traversal_revision
                            .checked_next()
                            .ok_or(PrepareReservedMutationError::TraversalRevisionExhausted)?,
                    )
                };

                PreparedMutation::SelectCommitted {
                    traversal_current,
                    traversal_revision_after_commit,
                }
            }
            ReservedQueueMutationKind::ReplaceWithCurrent {
                items_before_current,
                current_item,
                items_after_current,
            } => self.prepare_replacement_reservation(
                items_before_current,
                *current_item,
                items_after_current,
            )?,
        };
        let prepared_mutation = Box::new(prepared_mutation);
        let allocation_address = (&*prepared_mutation as *const PreparedMutation) as usize;
        let key = ReservationKey(
            NonZeroUsize::new(allocation_address)
                .expect("Box allocation address is always non-zero"),
        );
        let token = PreparedQueueMutationToken {
            key,
            prepared_mutation,
        };

        self.active_reservation = Some(key);
        Ok(token)
    }

    /// Exact abort снимает lock без allocator/revision/current mutation.
    pub fn abort_reserved(&mut self, token: PreparedQueueMutationToken) {
        self.assert_matching_reservation(token.key);
        self.active_reservation = None;
        drop(token);
    }

    /// Exact-token commit публикует queue/current/IDs одним business-infallible шагом.
    ///
    /// Чужой или protocol-stale token является invariant violation и panic-ит,
    /// а не маскируется как recoverable post-player failure.
    pub fn commit_reserved(&mut self, token: PreparedQueueMutationToken) -> ReservedMutationCommit {
        self.assert_matching_reservation(token.key);
        self.active_reservation = None;

        match *token.prepared_mutation {
            PreparedMutation::SelectCommitted {
                traversal_current,
                traversal_revision_after_commit,
            } => {
                let target_identity = self
                    .shuffle_visit_identity(traversal_current.item_id())
                    .expect("reserved committed target must have a top-level owner");
                if let Some(shuffle_traversal) = &mut self.shuffle_traversal {
                    shuffle_traversal.commit_direct_transition(target_identity);
                }
                self.traversal_current = Some(traversal_current);
                if let Some(next_revision) = traversal_revision_after_commit {
                    self.traversal_revision = next_revision;
                }
                ReservedMutationCommit {
                    allocated_item_ids: AllocatedPlaylistItemIds(Vec::new()),
                    traversal_current,
                }
            }
            PreparedMutation::ReplaceWithCurrent {
                replacement_items,
                allocation_plan,
                traversal_current,
                structural_revision_after_commit,
                traversal_revision_after_commit,
            } => {
                let shuffle_was_enabled = self.shuffle_traversal.is_some();
                let allocated_item_ids = allocation_plan.allocated_item_ids.clone();
                self.item_id_allocator.commit_allocation(&allocation_plan);
                self.entries = replacement_items
                    .into_iter()
                    .map(crate::PlaylistEntry::Single)
                    .collect();
                self.traversal_current = Some(traversal_current);
                if shuffle_was_enabled {
                    let canonical_entry_ids: Vec<_> = self.iter_top_level_entry_ids().collect();
                    let current = self
                        .shuffle_visit_identity(traversal_current.item_id())
                        .expect("reserved replacement current must have a top-level owner");
                    let mut random = rand::rng();
                    self.shuffle_traversal = Some(super::shuffle::ShuffleTraversal::fresh(
                        &canonical_entry_ids,
                        Some(current),
                        &mut random,
                    ));
                }
                self.structural_revision = structural_revision_after_commit;
                self.traversal_revision = traversal_revision_after_commit;
                ReservedMutationCommit {
                    allocated_item_ids: AllocatedPlaylistItemIds(allocated_item_ids),
                    traversal_current,
                }
            }
        }
    }

    /// Готовит replacement plan, сохраняя proposed IDs только внутри token.
    fn prepare_replacement_reservation(
        &self,
        items_before_current: Vec<PlaylistItemDraft>,
        current_item: PlaylistItemDraft,
        items_after_current: Vec<PlaylistItemDraft>,
    ) -> Result<PreparedMutation, PrepareReservedMutationError> {
        let candidate_item_count = items_before_current
            .len()
            .checked_add(1)
            .and_then(|count| count.checked_add(items_after_current.len()))
            .filter(|count| *count <= MAX_PLAYLIST_ITEMS)
            .ok_or(PrepareReservedMutationError::CapacityExceeded {
                requested: items_before_current
                    .len()
                    .saturating_add(1)
                    .saturating_add(items_after_current.len()),
                maximum: MAX_PLAYLIST_ITEMS,
            })?;
        let structural_revision_after_commit = self
            .structural_revision
            .checked_next()
            .ok_or(PrepareReservedMutationError::StructuralRevisionExhausted)?;
        let traversal_revision_after_commit = self
            .traversal_revision
            .checked_next()
            .ok_or(PrepareReservedMutationError::TraversalRevisionExhausted)?;
        let existing_item_ids: HashSet<_> = self.iter_playable_ids().collect();
        let allocation_plan = self
            .item_id_allocator
            .preflight_allocation(candidate_item_count, &existing_item_ids)
            .map_err(map_reservation_allocation_error)?;
        let current_index = items_before_current.len();
        let traversal_current =
            TraversalCurrentItemId(allocation_plan.allocated_item_ids[current_index]);
        let mut drafts = Vec::with_capacity(candidate_item_count);
        drafts.extend(items_before_current);
        drafts.push(current_item);
        drafts.extend(items_after_current);
        let replacement_items = items_from_drafts(drafts, &allocation_plan);

        Ok(PreparedMutation::ReplaceWithCurrent {
            replacement_items,
            allocation_plan,
            traversal_current,
            structural_revision_after_commit,
            traversal_revision_after_commit,
        })
    }

    /// Проверяет protocol pairing; recoverable branch после Installed запрещён.
    fn assert_matching_reservation(&self, token_key: ReservationKey) {
        assert_eq!(
            self.active_reservation,
            Some(token_key),
            "prepared queue mutation token does not match the active reservation"
        );
    }
}

/// Сохраняет typed allocator failure до установки reservation lock.
fn map_reservation_allocation_error(error: ItemIdAllocationError) -> PrepareReservedMutationError {
    match error {
        ItemIdAllocationError::ArithmeticExhausted => PrepareReservedMutationError::ItemIdExhausted,
        ItemIdAllocationError::Collision { item_id } => {
            PrepareReservedMutationError::ItemIdCollision { item_id }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::{
        CachedPlaylistMetadata, LocalLocator, NextPlaylistItemId, PlaylistItemDraft,
        PlaylistMediaKind, PlaylistQueueRestore,
    };

    use super::*;

    fn draft(index: usize) -> PlaylistItemDraft {
        let display_name = format!("reservation-{index}.mkv");
        PlaylistItemDraft::local(
            LocalLocator::Native(PathBuf::from(&display_name)),
            None,
            CachedPlaylistMetadata::new(display_name, PlaylistMediaKind::Video),
        )
    }

    fn appended_item_id(queue: &mut PlaylistQueue) -> PlaylistItemId {
        match queue.append_one(draft(0)).expect("fixture append") {
            crate::AddItemsOutcome::Added(item_ids) => item_ids.as_slice()[0],
            crate::AddItemsOutcome::NoItemsProvided => panic!("one draft cannot be empty"),
        }
    }

    #[test]
    fn every_fallible_reservation_preflight_has_exact_typed_outcome() {
        let mut locked_queue = PlaylistQueue::new();
        let locked_item_id = appended_item_id(&mut locked_queue);
        let first_token = locked_queue
            .prepare_reserved_mutation(
                locked_queue.revision_snapshot(),
                ReservedQueueMutation::select_committed(locked_item_id),
            )
            .expect("first reservation");
        assert!(matches!(
            locked_queue.prepare_reserved_mutation(
                locked_queue.revision_snapshot(),
                ReservedQueueMutation::select_committed(locked_item_id),
            ),
            Err(PrepareReservedMutationError::InstallCommitLinearizing)
        ));
        locked_queue.abort_reserved(first_token);

        let mut revision_queue = PlaylistQueue::new();
        let stale_revision = revision_queue.revision_snapshot();
        let committed_item_id = appended_item_id(&mut revision_queue);
        assert!(matches!(
            revision_queue.prepare_reserved_mutation(
                stale_revision,
                ReservedQueueMutation::select_committed(committed_item_id),
            ),
            Err(PrepareReservedMutationError::RevisionMismatch { .. })
        ));
        let missing_item_id =
            PlaylistItemId::from_persistence_value(99).expect("non-zero missing fixture identity");
        assert!(matches!(
            revision_queue.prepare_reserved_mutation(
                revision_queue.revision_snapshot(),
                ReservedQueueMutation::select_committed(missing_item_id),
            ),
            Err(PrepareReservedMutationError::ItemNotCommitted { item_id })
                if item_id == missing_item_id
        ));

        let mut capacity_queue = PlaylistQueue::new();
        let capacity_watermark = capacity_queue.next_item_id_snapshot();
        let over_capacity = vec![draft(1); MAX_PLAYLIST_ITEMS];
        assert!(matches!(
            capacity_queue.prepare_reserved_mutation(
                capacity_queue.revision_snapshot(),
                ReservedQueueMutation::replace_with_current(
                    over_capacity,
                    draft(2),
                    Vec::new(),
                ),
            ),
            Err(PrepareReservedMutationError::CapacityExceeded {
                requested,
                maximum: MAX_PLAYLIST_ITEMS,
            }) if requested == MAX_PLAYLIST_ITEMS + 1
        ));
        assert_eq!(capacity_queue.next_item_id_snapshot(), capacity_watermark);

        let max_watermark = NextPlaylistItemId::from_persistence_value(u64::MAX)
            .expect("non-zero maximum watermark");
        let mut exhausted_queue =
            PlaylistQueue::restore(PlaylistQueueRestore::new(Vec::new(), max_watermark, None))
                .expect("valid empty queue with exhausted allocator");
        assert!(matches!(
            exhausted_queue.prepare_reserved_mutation(
                exhausted_queue.revision_snapshot(),
                ReservedQueueMutation::replace_with_current(Vec::new(), draft(3), Vec::new(),),
            ),
            Err(PrepareReservedMutationError::ItemIdExhausted)
        ));

        let mut structural_exhausted = PlaylistQueue::new();
        structural_exhausted.structural_revision = QueueRevision(u64::MAX);
        assert!(matches!(
            structural_exhausted.prepare_reserved_mutation(
                structural_exhausted.revision_snapshot(),
                ReservedQueueMutation::replace_with_current(Vec::new(), draft(4), Vec::new(),),
            ),
            Err(PrepareReservedMutationError::StructuralRevisionExhausted)
        ));

        let mut traversal_exhausted = PlaylistQueue::new();
        traversal_exhausted.traversal_revision = QueueRevision(u64::MAX);
        assert!(matches!(
            traversal_exhausted.prepare_reserved_mutation(
                traversal_exhausted.revision_snapshot(),
                ReservedQueueMutation::replace_with_current(Vec::new(), draft(5), Vec::new(),),
            ),
            Err(PrepareReservedMutationError::TraversalRevisionExhausted)
        ));

        let collision_item_id = appended_item_id(&mut PlaylistQueue::new());
        assert_eq!(
            map_reservation_allocation_error(ItemIdAllocationError::Collision {
                item_id: collision_item_id,
            }),
            PrepareReservedMutationError::ItemIdCollision {
                item_id: collision_item_id,
            }
        );
    }

    #[test]
    fn reserved_same_item_reinstall_advances_shuffle_factual_visit_revision() {
        let mut queue = PlaylistQueue::new();
        let item_id = appended_item_id(&mut queue);
        queue
            .commit_manual_play(item_id)
            .expect("establish current item");
        queue.enable_shuffle().expect("enable shuffle");
        let revision_before = queue.revision_snapshot();
        let history_before = queue
            .shuffle_traversal_snapshot()
            .expect("shuffle snapshot")
            .history()
            .len();

        let token = queue
            .prepare_reserved_mutation(
                revision_before,
                ReservedQueueMutation::select_committed(item_id),
            )
            .expect("same-item reinstall reservation");
        queue.commit_reserved(token);

        let revision_after = queue.revision_snapshot();
        assert_eq!(revision_after.structural(), revision_before.structural());
        assert!(revision_after.traversal() > revision_before.traversal());
        assert_eq!(
            queue
                .shuffle_traversal_snapshot()
                .expect("updated shuffle snapshot")
                .history()
                .len(),
            history_before + 1
        );
    }
}
