//! Canonical queue owner и атомарные structural/traversal boundaries.

mod automatic;
mod discovery;
mod metadata_patch;
mod navigation;
mod outcomes;
mod removal;
mod reservation;
mod shuffle;
mod sort;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::fmt;

use crate::id::{ItemIdAllocationError, ItemIdAllocationPlan};
use crate::{
    NextPlaylistItemId, PlaylistItem, PlaylistItemDraft, PlaylistItemId, PlaylistItemIdAllocator,
    RestoredPlaylistItem,
};

pub use discovery::{
    DiscoveryBatchInsertError, DiscoveryBatchInsertOutcome, StableInsertionAnchor,
};
pub use metadata_patch::{
    MetadataPatchBatchError, MetadataPatchBatchOutcome, MetadataPatchItemOutcome,
    PlaylistMetadataPatch, PreparedMetadataPatchBatchCommit,
};
pub use navigation::{
    AutomaticEndedIntent, AutomaticNavigationOutcome, AutomaticStopReason,
    DiscardedManualNavigationPreview, FailedManualNavigationTarget, ManualNavigationCommit,
    ManualNavigationDirection, ManualNavigationIntent, ManualNavigationNoItem,
    ManualNavigationOrigin, ManualNavigationOutcome, ManualNavigationPreview,
    ManualNavigationPreviewError, ManualNavigationPreviewState, PrepareManualNavigationFailure,
    PreparedManualNavigationToken,
};
pub use outcomes::{
    AddItemsError, AddItemsOutcome, AllocatedPlaylistItemIds, CappedTailAppendOutcome,
    ClearQueueOutcome, MoveItemIntent, MoveItemOutcome, PrepareReservedMutationError,
    QueueRestoreError, RemoveItemOutcome, ReplaceQueueError, ReplaceQueueOutcome,
    ReservedMutationCommit, TraversalCurrentEffect, TraversalCurrentMutationError,
    TraversalCurrentMutationOutcome, TraversalCurrentValidationError,
};
pub use removal::{
    PlaylistRemovalSnapshot, RemovalCurrentOutcome, RemovalSnapshotRestoreError,
    RemovalSnapshotRestoreOutcome,
};
pub use reservation::{PreparedQueueMutationToken, ReservedQueueMutation};
pub use shuffle::{
    BulkRemoveError, BulkRemoveOutcome, MAX_SHUFFLE_HISTORY_ENTRIES, ShuffleHistoryCursor,
    ShuffleQueueRestoreError, ShuffleToggleError, ShuffleToggleOutcome,
    ShuffleTraversalRestoreError, ShuffleTraversalSnapshot,
};
pub use sort::{
    ApplyPreparedCanonicalSortError, ApplyPreparedCanonicalSortOutcome,
    CanonicalSortPreparationCancelled, CanonicalSortPreparationStatistics, CanonicalSortSnapshot,
    PlaylistSortKey, PreparedCanonicalSort, PreparedCanonicalSortCommit, SortCanonicalQueue,
    SortCanonicalQueueOutcome, SortDirection,
};

/// Hard safety cap любой committed/candidate queue.
pub const MAX_PLAYLIST_ITEMS: usize = 50_000;

/// Opaque revision одного независимого queue state dimension.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueueRevision(u64);

impl QueueRevision {
    /// Начальное значение новой/restored runtime queue.
    const INITIAL: Self = Self(0);

    /// Checked preflight следующей revision без mutation.
    fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

impl fmt::Debug for QueueRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("QueueRevision")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for QueueRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "queue-revision-{}", self.0)
    }
}

/// Snapshot независимых structural/traversal/metadata revisions.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct QueueRevisionSnapshot {
    structural: QueueRevision,
    traversal: QueueRevision,
    metadata: QueueRevision,
}

impl QueueRevisionSnapshot {
    /// Возвращает revision canonical membership/order.
    pub const fn structural(self) -> QueueRevision {
        self.structural
    }

    /// Возвращает revision persisted traversal current.
    pub const fn traversal(self) -> QueueRevision {
        self.traversal
    }

    /// Возвращает revision cached metadata commits.
    pub const fn metadata(self) -> QueueRevision {
        self.metadata
    }

    /// D08 reservation сравнивает только влияющие на него dimensions.
    fn same_reservation_preconditions(self, other: Self) -> bool {
        self.structural == other.structural && self.traversal == other.traversal
    }
}

impl fmt::Debug for QueueRevisionSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QueueRevisionSnapshot")
            .field("structural", &self.structural)
            .field("traversal", &self.traversal)
            .field("metadata", &self.metadata)
            .finish()
    }
}

impl fmt::Display for QueueRevisionSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "structural {}, traversal {}, metadata {}",
            self.structural, self.traversal, self.metadata
        )
    }
}

/// Validated persisted cursor, который всегда ссылается на committed Item ID.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraversalCurrentItemId(PlaylistItemId);

impl TraversalCurrentItemId {
    /// Возвращает stable Item ID без превращения cursor в player identity.
    pub const fn item_id(self) -> PlaylistItemId {
        self.0
    }
}

impl fmt::Debug for TraversalCurrentItemId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("TraversalCurrentItemId")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for TraversalCurrentItemId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// Serde-neutral input полного persistence restore.
pub struct PlaylistQueueRestore {
    restored_items: Vec<RestoredPlaylistItem>,
    next_item_id: NextPlaylistItemId,
    traversal_current_item_id: Option<PlaylistItemId>,
}

impl PlaylistQueueRestore {
    /// Собирает DTO-mapped restore input; все cross-field invariants проверяет queue.
    pub fn new(
        restored_items: Vec<RestoredPlaylistItem>,
        next_item_id: NextPlaylistItemId,
        traversal_current_item_id: Option<PlaylistItemId>,
    ) -> Self {
        Self {
            restored_items,
            next_item_id,
            traversal_current_item_id,
        }
    }
}

impl fmt::Debug for PlaylistQueueRestore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistQueueRestore")
            .field("restored_item_count", &self.restored_items.len())
            .field("next_item_id", &self.next_item_id)
            .field("traversal_current_item_id", &self.traversal_current_item_id)
            .finish()
    }
}

/// Единственный владелец canonical order и его mutation invariants.
pub struct PlaylistQueue {
    items: Vec<PlaylistItem>,
    item_id_allocator: PlaylistItemIdAllocator,
    traversal_current: Option<TraversalCurrentItemId>,
    structural_revision: QueueRevision,
    traversal_revision: QueueRevision,
    metadata_revision: QueueRevision,
    active_reservation: Option<reservation::ReservationKey>,
    shuffle_traversal: Option<shuffle::ShuffleTraversal>,
}

impl PlaylistQueue {
    /// Создаёт пустую новую lineage с первым будущим Item ID = 1.
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            item_id_allocator: PlaylistItemIdAllocator::initial(),
            traversal_current: None,
            structural_revision: QueueRevision::INITIAL,
            traversal_revision: QueueRevision::INITIAL,
            metadata_revision: QueueRevision::INITIAL,
            active_reservation: None,
            shuffle_traversal: None,
        }
    }

    /// Атомарно валидирует capacity, unique IDs, current и allocator watermark.
    pub fn restore(snapshot: PlaylistQueueRestore) -> Result<Self, QueueRestoreError> {
        if snapshot.restored_items.len() > MAX_PLAYLIST_ITEMS {
            return Err(QueueRestoreError::CapacityExceeded {
                restored: snapshot.restored_items.len(),
                maximum: MAX_PLAYLIST_ITEMS,
            });
        }

        let mut unique_item_ids = HashSet::with_capacity(snapshot.restored_items.len());
        let mut restored_item_ids = Vec::with_capacity(snapshot.restored_items.len());

        for restored_item in &snapshot.restored_items {
            let item_id = restored_item.item_id();
            if !unique_item_ids.insert(item_id) {
                return Err(QueueRestoreError::DuplicateItemId { item_id });
            }
            restored_item_ids.push(item_id);
        }

        let item_id_allocator =
            PlaylistItemIdAllocator::restore(snapshot.next_item_id, &restored_item_ids)
                .map_err(QueueRestoreError::InvalidAllocator)?;
        let traversal_current = match snapshot.traversal_current_item_id {
            Some(item_id) if unique_item_ids.contains(&item_id) => {
                Some(TraversalCurrentItemId(item_id))
            }
            Some(item_id) => return Err(QueueRestoreError::CurrentItemNotCommitted { item_id }),
            None => None,
        };
        let items = snapshot
            .restored_items
            .into_iter()
            .map(RestoredPlaylistItem::into_item)
            .collect();

        Ok(Self {
            items,
            item_id_allocator,
            traversal_current,
            structural_revision: QueueRevision::INITIAL,
            traversal_revision: QueueRevision::INITIAL,
            metadata_revision: QueueRevision::INITIAL,
            active_reservation: None,
            shuffle_traversal: None,
        })
    }

    /// Возвращает число committed canonical rows за O(1).
    pub const fn len(&self) -> usize {
        self.items.len()
    }

    /// Сообщает emptiness без сканирования rows.
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Возвращает revision-stable read-only canonical slice.
    pub fn items(&self) -> &[PlaylistItem] {
        &self.items
    }

    /// Выполняет read-only lookup по stable identity.
    pub fn item(&self, item_id: PlaylistItemId) -> Option<&PlaylistItem> {
        self.items.iter().find(|item| item.item_id() == item_id)
    }

    /// Возвращает optional persisted cursor отдельно от active player state.
    pub const fn traversal_current(&self) -> Option<TraversalCurrentItemId> {
        self.traversal_current
    }

    /// Снимает allocator high-watermark для persistence state.
    pub const fn next_item_id_snapshot(&self) -> NextPlaylistItemId {
        self.item_id_allocator.snapshot()
    }

    /// Снимает независимые mutation revisions для precondition checks.
    pub const fn revision_snapshot(&self) -> QueueRevisionSnapshot {
        QueueRevisionSnapshot {
            structural: self.structural_revision,
            traversal: self.traversal_revision,
            metadata: self.metadata_revision,
        }
    }

    /// Проверяет committed membership и создаёт opaque current boundary.
    pub fn validate_traversal_current(
        &self,
        item_id: PlaylistItemId,
    ) -> Result<TraversalCurrentItemId, TraversalCurrentValidationError> {
        self.item(item_id)
            .map(|_| TraversalCurrentItemId(item_id))
            .ok_or(TraversalCurrentValidationError::ItemNotCommitted { item_id })
    }

    /// Атомарно добавляет одну ID-less строку в конец canonical order.
    pub fn append_one(
        &mut self,
        draft: PlaylistItemDraft,
    ) -> Result<AddItemsOutcome, AddItemsError> {
        self.append_batch(vec![draft])
    }

    /// Атомарно добавляет весь ID-less batch либо не меняет state.
    pub fn append_batch(
        &mut self,
        drafts: Vec<PlaylistItemDraft>,
    ) -> Result<AddItemsOutcome, AddItemsError> {
        let mut random = rand::rng();
        self.append_batch_with_rng(drafts, &mut random)
    }

    /// Вариант batch append с injectable RNG для deterministic shuffle tests.
    pub fn append_batch_with_rng<R: rand::Rng + ?Sized>(
        &mut self,
        drafts: Vec<PlaylistItemDraft>,
        random: &mut R,
    ) -> Result<AddItemsOutcome, AddItemsError> {
        if self.active_reservation.is_some() {
            return Err(AddItemsError::InstallCommitLinearizing);
        }
        if drafts.is_empty() {
            return Ok(AddItemsOutcome::NoItemsProvided);
        }
        let requested = drafts.len();
        let resulting_len = self
            .items
            .len()
            .checked_add(requested)
            .filter(|resulting_len| *resulting_len <= MAX_PLAYLIST_ITEMS)
            .ok_or(AddItemsError::CapacityExceeded {
                current: self.items.len(),
                requested,
                maximum: MAX_PLAYLIST_ITEMS,
            })?;
        let _ = resulting_len;
        let next_structural_revision = self
            .structural_revision
            .checked_next()
            .ok_or(AddItemsError::StructuralRevisionExhausted)?;
        let existing_item_ids = self.existing_item_ids();
        let allocation_plan = self
            .item_id_allocator
            .preflight_allocation(requested, &existing_item_ids)
            .map_err(map_add_allocation_error)?;
        let allocated_item_ids = allocation_plan.allocated_item_ids.clone();
        let new_items = drafts
            .into_iter()
            .zip(allocated_item_ids.iter().copied())
            .map(|(draft, item_id)| draft.into_item(item_id));

        if let Some(shuffle_traversal) = &mut self.shuffle_traversal {
            shuffle_traversal.merge_new_items(&allocated_item_ids, random);
        }
        self.item_id_allocator.commit_allocation(&allocation_plan);
        self.items.extend(new_items);
        self.structural_revision = next_structural_revision;

        Ok(AddItemsOutcome::Added(AllocatedPlaylistItemIds(
            allocated_item_ids,
        )))
    }

    /// D67 атомарно добавляет caller-ordered prefix, который помещается в hard cap.
    ///
    /// Rejected tail не получает Item ID и не меняет allocator/revisions.
    pub fn append_capped_tail(
        &mut self,
        mut drafts: Vec<PlaylistItemDraft>,
    ) -> Result<CappedTailAppendOutcome, AddItemsError> {
        let requested = drafts.len();
        let remaining_capacity = MAX_PLAYLIST_ITEMS.saturating_sub(self.items.len());
        let accepted = requested.min(remaining_capacity);
        let capacity_rejected = requested.saturating_sub(accepted);
        drafts.truncate(accepted);

        let allocated_item_ids = match self.append_batch(drafts)? {
            AddItemsOutcome::Added(item_ids) => item_ids,
            AddItemsOutcome::NoItemsProvided => AllocatedPlaylistItemIds(Vec::new()),
        };
        Ok(CappedTailAppendOutcome {
            allocated_item_ids,
            capacity_rejected,
        })
    }

    /// Атомарно заменяет canonical queue новыми ID-less drafts и очищает current.
    pub fn replace_all(
        &mut self,
        drafts: Vec<PlaylistItemDraft>,
    ) -> Result<ReplaceQueueOutcome, ReplaceQueueError> {
        let mut random = rand::rng();
        self.replace_all_with_rng(drafts, &mut random)
    }

    /// Вариант atomic replace с injectable RNG для enabled shuffle cycle.
    pub fn replace_all_with_rng<R: rand::Rng + ?Sized>(
        &mut self,
        drafts: Vec<PlaylistItemDraft>,
        random: &mut R,
    ) -> Result<ReplaceQueueOutcome, ReplaceQueueError> {
        if self.active_reservation.is_some() {
            return Err(ReplaceQueueError::InstallCommitLinearizing);
        }
        if drafts.len() > MAX_PLAYLIST_ITEMS {
            return Err(ReplaceQueueError::CapacityExceeded {
                requested: drafts.len(),
                maximum: MAX_PLAYLIST_ITEMS,
            });
        }
        if drafts.is_empty() && self.items.is_empty() && self.traversal_current.is_none() {
            return Ok(ReplaceQueueOutcome::AlreadyEmpty);
        }

        let next_structural_revision = self
            .structural_revision
            .checked_next()
            .ok_or(ReplaceQueueError::StructuralRevisionExhausted)?;
        let next_traversal_revision = self
            .traversal_current
            .map(|_| {
                self.traversal_revision
                    .checked_next()
                    .ok_or(ReplaceQueueError::TraversalRevisionExhausted)
            })
            .transpose()?;
        let traversal_current_effect = if self.traversal_current.is_some() {
            TraversalCurrentEffect::Cleared
        } else {
            TraversalCurrentEffect::Preserved
        };

        if drafts.is_empty() {
            let removed_item_count = self.items.len();
            let replacement_shuffle = self
                .shuffle_traversal
                .as_ref()
                .map(|_| shuffle::ShuffleTraversal::fresh(&[], None, random));
            self.items.clear();
            self.traversal_current = None;
            self.shuffle_traversal = replacement_shuffle;
            self.structural_revision = next_structural_revision;
            if let Some(next_revision) = next_traversal_revision {
                self.traversal_revision = next_revision;
            }
            return Ok(ReplaceQueueOutcome::Cleared {
                removed_item_count,
                traversal_current_effect,
            });
        }

        let existing_item_ids = self.existing_item_ids();
        let allocation_plan = self
            .item_id_allocator
            .preflight_allocation(drafts.len(), &existing_item_ids)
            .map_err(map_replace_allocation_error)?;
        let allocated_item_ids = allocation_plan.allocated_item_ids.clone();
        let replacement_items = drafts
            .into_iter()
            .zip(allocated_item_ids.iter().copied())
            .map(|(draft, item_id)| draft.into_item(item_id))
            .collect();
        let replacement_shuffle = self
            .shuffle_traversal
            .as_ref()
            .map(|_| shuffle::ShuffleTraversal::fresh(&allocated_item_ids, None, random));

        self.item_id_allocator.commit_allocation(&allocation_plan);
        self.items = replacement_items;
        self.traversal_current = None;
        self.shuffle_traversal = replacement_shuffle;
        self.structural_revision = next_structural_revision;
        if let Some(next_revision) = next_traversal_revision {
            self.traversal_revision = next_revision;
        }

        Ok(ReplaceQueueOutcome::Replaced {
            allocated_item_ids: AllocatedPlaylistItemIds(allocated_item_ids),
            traversal_current_effect,
        })
    }

    /// Удаляет exact committed identity, не выбирая successor автоматически.
    pub fn remove(&mut self, item_id: PlaylistItemId) -> RemoveItemOutcome {
        if self.active_reservation.is_some() {
            return RemoveItemOutcome::InstallCommitLinearizing;
        }
        let Some(item_index) = self.index_of(item_id) else {
            return RemoveItemOutcome::NotFound { item_id };
        };
        let Some(next_structural_revision) = self.structural_revision.checked_next() else {
            return RemoveItemOutcome::StructuralRevisionExhausted;
        };
        let clears_current = self
            .traversal_current
            .is_some_and(|current| current.item_id() == item_id);
        let next_traversal_revision = if clears_current {
            let Some(next_revision) = self.traversal_revision.checked_next() else {
                return RemoveItemOutcome::TraversalRevisionExhausted;
            };
            Some(next_revision)
        } else {
            None
        };

        if let Some(shuffle_traversal) = &mut self.shuffle_traversal {
            let removed_item_ids = HashSet::from([item_id]);
            let remaining_canonical_item_ids: Vec<_> = self
                .items
                .iter()
                .filter(|item| item.item_id() != item_id)
                .map(|item| item.item_id())
                .collect();
            shuffle_traversal.remove_items(
                &removed_item_ids,
                &remaining_canonical_item_ids,
                clears_current,
            );
        }
        self.items.remove(item_index);
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
                removed_item_id: item_id,
            }
        } else {
            RemovalCurrentOutcome::Preserved(self.traversal_current)
        };
        RemoveItemOutcome::Removed {
            item_id,
            traversal_current_effect,
            current_outcome,
        }
    }

    /// Перемещает exact Item ID относительно intent-named anchor.
    pub fn move_item(
        &mut self,
        item_id: PlaylistItemId,
        intent: MoveItemIntent,
    ) -> MoveItemOutcome {
        if self.active_reservation.is_some() {
            return MoveItemOutcome::InstallCommitLinearizing;
        }
        let Some(source_index) = self.index_of(item_id) else {
            return MoveItemOutcome::ItemNotFound { item_id };
        };
        let target_index = match self.move_target_index(source_index, intent) {
            Ok(target_index) => target_index,
            Err(anchor_item_id) => return MoveItemOutcome::AnchorNotFound { anchor_item_id },
        };

        if source_index == target_index {
            return MoveItemOutcome::AlreadyInPlace { item_id };
        }
        let Some(next_structural_revision) = self.structural_revision.checked_next() else {
            return MoveItemOutcome::StructuralRevisionExhausted;
        };

        let moved_item = self.items.remove(source_index);
        self.items.insert(target_index, moved_item);
        self.structural_revision = next_structural_revision;

        MoveItemOutcome::Moved { item_id }
    }

    /// Очищает canonical queue, current и сохраняет allocator high-watermark.
    pub fn clear(&mut self) -> ClearQueueOutcome {
        if self.active_reservation.is_some() {
            return ClearQueueOutcome::InstallCommitLinearizing;
        }
        if self.items.is_empty() && self.traversal_current.is_none() {
            return ClearQueueOutcome::AlreadyEmpty;
        }
        let Some(next_structural_revision) = self.structural_revision.checked_next() else {
            return ClearQueueOutcome::StructuralRevisionExhausted;
        };
        let next_traversal_revision = if self.traversal_current.is_some() {
            let Some(next_revision) = self.traversal_revision.checked_next() else {
                return ClearQueueOutcome::TraversalRevisionExhausted;
            };
            Some(next_revision)
        } else {
            None
        };
        let current_before = self.traversal_current;
        let removed_item_count = self.items.len();
        let traversal_current_effect = if self.traversal_current.is_some() {
            TraversalCurrentEffect::Cleared
        } else {
            TraversalCurrentEffect::Preserved
        };

        self.items.clear();
        self.traversal_current = None;
        if self.shuffle_traversal.is_some() {
            self.shuffle_traversal = Some(shuffle::ShuffleTraversal::empty_idle());
        }
        self.structural_revision = next_structural_revision;
        if let Some(next_revision) = next_traversal_revision {
            self.traversal_revision = next_revision;
        }

        let current_outcome = match traversal_current_effect {
            TraversalCurrentEffect::Cleared => match current_before {
                Some(current) => RemovalCurrentOutcome::Detached {
                    removed_item_id: current.item_id(),
                },
                None => RemovalCurrentOutcome::Preserved(None),
            },
            TraversalCurrentEffect::Preserved => RemovalCurrentOutcome::Preserved(None),
        };
        ClearQueueOutcome::Cleared {
            removed_item_count,
            traversal_current_effect,
            current_outcome,
        }
    }

    /// Устанавливает current только после committed-membership validation.
    pub fn set_traversal_current(
        &mut self,
        item_id: PlaylistItemId,
    ) -> Result<TraversalCurrentMutationOutcome, TraversalCurrentMutationError> {
        if self.active_reservation.is_some() {
            return Err(TraversalCurrentMutationError::InstallCommitLinearizing);
        }
        let validated = self
            .validate_traversal_current(item_id)
            .map_err(|_| TraversalCurrentMutationError::ItemNotCommitted { item_id })?;
        if self.traversal_current == Some(validated) {
            return Ok(TraversalCurrentMutationOutcome::AlreadyCurrent(validated));
        }
        let next_revision = self
            .traversal_revision
            .checked_next()
            .ok_or(TraversalCurrentMutationError::TraversalRevisionExhausted)?;

        if let Some(shuffle_traversal) = &mut self.shuffle_traversal {
            shuffle_traversal.commit_direct_transition(item_id);
        }
        self.traversal_current = Some(validated);
        self.traversal_revision = next_revision;
        Ok(TraversalCurrentMutationOutcome::Set(validated))
    }

    /// Очищает optional current без выбора нового item.
    pub fn clear_traversal_current(
        &mut self,
    ) -> Result<TraversalCurrentMutationOutcome, TraversalCurrentMutationError> {
        if self.active_reservation.is_some() {
            return Err(TraversalCurrentMutationError::InstallCommitLinearizing);
        }
        if self.traversal_current.is_none() {
            return Ok(TraversalCurrentMutationOutcome::AlreadyAbsent);
        }
        let next_revision = self
            .traversal_revision
            .checked_next()
            .ok_or(TraversalCurrentMutationError::TraversalRevisionExhausted)?;

        if let Some(shuffle_traversal) = &mut self.shuffle_traversal {
            let canonical_item_ids: Vec<_> = self.items.iter().map(|item| item.item_id()).collect();
            shuffle_traversal.make_idle(&canonical_item_ids);
        }
        self.traversal_current = None;
        self.traversal_revision = next_revision;
        Ok(TraversalCurrentMutationOutcome::Cleared)
    }

    /// Возвращает set committed IDs для allocator collision preflight.
    fn existing_item_ids(&self) -> HashSet<PlaylistItemId> {
        self.items.iter().map(PlaylistItem::item_id).collect()
    }

    /// Ищет canonical position только внутри owner implementation.
    fn index_of(&self, item_id: PlaylistItemId) -> Option<usize> {
        self.items.iter().position(|item| item.item_id() == item_id)
    }

    /// Вычисляет final insertion index после удаления source row.
    fn move_target_index(
        &self,
        source_index: usize,
        intent: MoveItemIntent,
    ) -> Result<usize, PlaylistItemId> {
        match intent {
            MoveItemIntent::ToFront => Ok(0),
            MoveItemIntent::ToBack => Ok(self.items.len().saturating_sub(1)),
            MoveItemIntent::Before(anchor_item_id) => {
                let anchor_index = self.index_of(anchor_item_id).ok_or(anchor_item_id)?;
                if anchor_index == source_index {
                    return Ok(source_index);
                }
                Ok(if source_index < anchor_index {
                    anchor_index - 1
                } else {
                    anchor_index
                })
            }
            MoveItemIntent::After(anchor_item_id) => {
                let anchor_index = self.index_of(anchor_item_id).ok_or(anchor_item_id)?;
                if anchor_index == source_index {
                    return Ok(source_index);
                }
                let anchor_after_removal = if source_index < anchor_index {
                    anchor_index - 1
                } else {
                    anchor_index
                };
                Ok(anchor_after_removal + 1)
            }
        }
    }
}

impl Default for PlaylistQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for PlaylistQueue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaylistQueue")
            .field("item_count", &self.items.len())
            .field("next_item_id", &self.item_id_allocator.snapshot())
            .field("traversal_current", &self.traversal_current)
            .field("revisions", &self.revision_snapshot())
            .field("shuffle_enabled", &self.shuffle_traversal.is_some())
            .field("has_active_reservation", &self.active_reservation.is_some())
            .finish()
    }
}

/// Сохраняет typed distinction allocator exhaustion/collision для append API.
fn map_add_allocation_error(error: ItemIdAllocationError) -> AddItemsError {
    match error {
        ItemIdAllocationError::ArithmeticExhausted => AddItemsError::ItemIdExhausted,
        ItemIdAllocationError::Collision { item_id } => AddItemsError::ItemIdCollision { item_id },
    }
}

/// Сохраняет typed distinction allocator exhaustion/collision для replace API.
fn map_replace_allocation_error(error: ItemIdAllocationError) -> ReplaceQueueError {
    match error {
        ItemIdAllocationError::ArithmeticExhausted => ReplaceQueueError::ItemIdExhausted,
        ItemIdAllocationError::Collision { item_id } => {
            ReplaceQueueError::ItemIdCollision { item_id }
        }
    }
}

/// Строит committed items только после полного allocation preflight.
fn items_from_drafts(
    drafts: Vec<PlaylistItemDraft>,
    allocation_plan: &ItemIdAllocationPlan,
) -> Vec<PlaylistItem> {
    drafts
        .into_iter()
        .zip(allocation_plan.allocated_item_ids.iter().copied())
        .map(|(draft, item_id)| draft.into_item(item_id))
        .collect()
}
pub use automatic::{
    AutomaticTraversalAdvance, AutomaticTraversalCommit, AutomaticTraversalPlan,
    AutomaticTraversalStart, PrepareAutomaticTraversalFailure, PreparedAutomaticTraversalToken,
};
