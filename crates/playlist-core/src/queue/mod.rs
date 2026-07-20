//! Canonical queue owner и атомарные structural/traversal boundaries.

mod automatic;
mod discovery;
mod entries;
mod metadata_patch;
mod navigation;
mod outcomes;
mod read;
mod removal;
mod reordering;
mod reservation;
mod shuffle;
mod sort;
mod structural;

#[cfg(test)]
mod group_structural_tests;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::fmt;

use crate::id::{ItemIdAllocationError, ItemIdAllocationPlan};
use crate::{
    NextPlaylistCompoundGroupId, NextPlaylistItemId, PlaylistCompoundGroupIdAllocator,
    PlaylistEntry, PlaylistItem, PlaylistItemDraft, PlaylistItemId, PlaylistItemIdAllocator,
    RestoredPlaylistItem,
};

pub use discovery::{
    DiscoveryBatchInsertError, DiscoveryBatchInsertOutcome, StableInsertionAnchor,
};
pub use entries::{
    AddPlaylistEntriesOutcome, AllocatedPlaylistEntries, CappedPlaylistEntriesAppendOutcome,
    PlaylistEntriesMutationError, ReplacePlaylistEntriesOutcome,
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
pub use read::OwnedPlayableItemsSnapshot;
pub use removal::{
    PlaylistRemovalSnapshot, RemovalCurrentOutcome, RemovalSnapshotRestoreError,
    RemovalSnapshotRestoreOutcome,
};
pub use reordering::MoveItemsOutcome;
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
    entries: Vec<PlaylistEntry>,
    item_id_allocator: PlaylistItemIdAllocator,
    compound_group_id_allocator: PlaylistCompoundGroupIdAllocator,
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
            entries: Vec::new(),
            item_id_allocator: PlaylistItemIdAllocator::initial(),
            compound_group_id_allocator: PlaylistCompoundGroupIdAllocator::initial(),
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
        let entries = snapshot
            .restored_items
            .into_iter()
            .map(RestoredPlaylistItem::into_item)
            .map(PlaylistEntry::Single)
            .collect();

        Ok(Self {
            entries,
            item_id_allocator,
            compound_group_id_allocator: PlaylistCompoundGroupIdAllocator::initial(),
            traversal_current,
            structural_revision: QueueRevision::INITIAL,
            traversal_revision: QueueRevision::INITIAL,
            metadata_revision: QueueRevision::INITIAL,
            active_reservation: None,
            shuffle_traversal: None,
        })
    }

    /// Сообщает emptiness без сканирования rows.
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Выполняет read-only lookup по stable Item ID.
    pub fn item(&self, item_id: PlaylistItemId) -> Option<&PlaylistItem> {
        self.iter_playable_items()
            .find(|item| item.item_id() == item_id)
    }

    /// Возвращает optional persisted cursor отдельно от active player state.
    pub const fn traversal_current(&self) -> Option<TraversalCurrentItemId> {
        self.traversal_current
    }

    /// Снимает allocator high-watermark для persistence state.
    pub const fn next_item_id_snapshot(&self) -> NextPlaylistItemId {
        self.item_id_allocator.snapshot()
    }

    /// Снимает независимый Group ID high-watermark для будущего persistence v2.
    pub const fn next_compound_group_id_snapshot(&self) -> NextPlaylistCompoundGroupId {
        self.compound_group_id_allocator.snapshot()
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
        let entry_drafts = drafts
            .into_iter()
            .map(crate::PlaylistEntryDraft::Single)
            .collect();

        match self
            .append_entries_with_rng(entry_drafts, random)
            .map_err(map_add_entries_error)?
        {
            AddPlaylistEntriesOutcome::NoEntriesProvided => Ok(AddItemsOutcome::NoItemsProvided),
            AddPlaylistEntriesOutcome::Added(allocated) => Ok(AddItemsOutcome::Added(
                AllocatedPlaylistItemIds(allocated.into_playable_item_ids()),
            )),
        }
    }

    /// D67 атомарно добавляет caller-ordered prefix, который помещается в hard cap.
    ///
    /// Rejected tail не получает Item ID и не меняет allocator/revisions.
    pub fn append_capped_tail(
        &mut self,
        mut drafts: Vec<PlaylistItemDraft>,
    ) -> Result<CappedTailAppendOutcome, AddItemsError> {
        let requested = drafts.len();
        let remaining_capacity = MAX_PLAYLIST_ITEMS.saturating_sub(self.retained_item_count());
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
        let entry_drafts = drafts
            .into_iter()
            .map(crate::PlaylistEntryDraft::Single)
            .collect();

        match self
            .replace_entries_with_rng(entry_drafts, random)
            .map_err(map_replace_entries_error)?
        {
            ReplacePlaylistEntriesOutcome::AlreadyEmpty => Ok(ReplaceQueueOutcome::AlreadyEmpty),
            ReplacePlaylistEntriesOutcome::Cleared {
                removed_item_count,
                traversal_current_effect,
            } => Ok(ReplaceQueueOutcome::Cleared {
                removed_item_count,
                traversal_current_effect,
            }),
            ReplacePlaylistEntriesOutcome::Replaced {
                allocated_entries,
                traversal_current_effect,
            } => Ok(ReplaceQueueOutcome::Replaced {
                allocated_item_ids: AllocatedPlaylistItemIds(
                    allocated_entries.into_playable_item_ids(),
                ),
                traversal_current_effect,
            }),
        }
    }

    /// Очищает canonical queue, current и сохраняет allocator high-watermark.
    pub fn clear(&mut self) -> ClearQueueOutcome {
        if self.active_reservation.is_some() {
            return ClearQueueOutcome::InstallCommitLinearizing;
        }
        if self.entries.is_empty() && self.traversal_current.is_none() {
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
        let removed_item_count = self.retained_item_count();
        let traversal_current_effect = if self.traversal_current.is_some() {
            TraversalCurrentEffect::Cleared
        } else {
            TraversalCurrentEffect::Preserved
        };

        self.entries.clear();
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

        let canonical_item_ids: Vec<_> = self.iter_playable_ids().collect();
        if let Some(shuffle_traversal) = &mut self.shuffle_traversal {
            shuffle_traversal.make_idle(&canonical_item_ids);
        }
        self.traversal_current = None;
        self.traversal_revision = next_revision;
        Ok(TraversalCurrentMutationOutcome::Cleared)
    }

    /// Возвращает set committed IDs для allocator collision preflight.
    fn existing_item_ids(&self) -> HashSet<PlaylistItemId> {
        self.iter_playable_ids().collect()
    }

    /// Ищет canonical position только внутри owner implementation.
    fn index_of(&self, item_id: PlaylistItemId) -> Option<usize> {
        self.entries.iter().position(|entry| {
            entry
                .as_single()
                .is_some_and(|item| item.item_id() == item_id)
        })
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
            .field("top_level_entry_count", &self.entries.len())
            .field("retained_item_count", &self.retained_item_count())
            .field(
                "next_compound_group_id",
                &self.compound_group_id_allocator.snapshot(),
            )
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

/// Адаптирует общий entry failure к legacy single-only append contract.
fn map_add_entries_error(error: PlaylistEntriesMutationError) -> AddItemsError {
    match error {
        PlaylistEntriesMutationError::InstallCommitLinearizing => {
            AddItemsError::InstallCommitLinearizing
        }
        PlaylistEntriesMutationError::CapacityExceeded {
            current_retained_items,
            requested_retained_items,
            maximum,
        } => AddItemsError::CapacityExceeded {
            current: current_retained_items,
            requested: requested_retained_items,
            maximum,
        },
        PlaylistEntriesMutationError::ItemIdArithmeticExhausted => AddItemsError::ItemIdExhausted,
        PlaylistEntriesMutationError::ItemIdCollision { item_id } => {
            AddItemsError::ItemIdCollision { item_id }
        }
        PlaylistEntriesMutationError::StructuralRevisionExhausted => {
            AddItemsError::StructuralRevisionExhausted
        }
        PlaylistEntriesMutationError::CompoundGroupIdArithmeticExhausted
        | PlaylistEntriesMutationError::CompoundGroupIdCollision { .. }
        | PlaylistEntriesMutationError::TraversalRevisionExhausted => {
            unreachable!("single-only append does not allocate groups or traversal revision")
        }
    }
}

/// Адаптирует общий entry failure к legacy single-only replace contract.
fn map_replace_entries_error(error: PlaylistEntriesMutationError) -> ReplaceQueueError {
    match error {
        PlaylistEntriesMutationError::InstallCommitLinearizing => {
            ReplaceQueueError::InstallCommitLinearizing
        }
        PlaylistEntriesMutationError::CapacityExceeded {
            requested_retained_items,
            maximum,
            ..
        } => ReplaceQueueError::CapacityExceeded {
            requested: requested_retained_items,
            maximum,
        },
        PlaylistEntriesMutationError::ItemIdArithmeticExhausted => {
            ReplaceQueueError::ItemIdExhausted
        }
        PlaylistEntriesMutationError::ItemIdCollision { item_id } => {
            ReplaceQueueError::ItemIdCollision { item_id }
        }
        PlaylistEntriesMutationError::StructuralRevisionExhausted => {
            ReplaceQueueError::StructuralRevisionExhausted
        }
        PlaylistEntriesMutationError::TraversalRevisionExhausted => {
            ReplaceQueueError::TraversalRevisionExhausted
        }
        PlaylistEntriesMutationError::CompoundGroupIdArithmeticExhausted
        | PlaylistEntriesMutationError::CompoundGroupIdCollision { .. } => {
            unreachable!("single-only replace does not allocate compound Group IDs")
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
