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
                let traversal_revision_after_commit =
                    if self.traversal_current == Some(traversal_current) {
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
                let allocated_item_ids = allocation_plan.allocated_item_ids.clone();
                self.item_id_allocator.commit_allocation(&allocation_plan);
                self.items = replacement_items;
                self.traversal_current = Some(traversal_current);
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
        let existing_item_ids: HashSet<_> = self.items.iter().map(PlaylistItem::item_id).collect();
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
