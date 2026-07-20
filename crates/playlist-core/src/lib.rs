//! Чистая доменная модель canonical очереди воспроизведения.
//!
//! Crate намеренно не знает про UI, player, I/O, serde и filesystem discovery.
//! Он владеет стабильной идентичностью строк, allocator high-watermark,
//! canonical order, repeat/navigation policy и атомарными mutation boundaries.

mod entry;
mod id;
mod item;
mod locator;
mod metadata;
mod queue;
mod repeat;

pub use entry::{
    CompoundGroupAllocatorRestoreError, EmptyPlaylistCompoundDraft, NextPlaylistCompoundGroupId,
    PlaylistCompoundGroup, PlaylistCompoundGroupDraft, PlaylistCompoundGroupId,
    PlaylistCompoundGroupIdAllocator, PlaylistCompoundGroupIdPersistenceError,
    PlaylistCompoundMembership, PlaylistCompoundPart, PlaylistCompoundPartOrdinal, PlaylistEntry,
    PlaylistEntryDraft, PlaylistEntryId,
};
pub use id::{
    AllocatorRestoreError, NextPlaylistItemId, PlaylistItemId, PlaylistItemIdAllocator,
    PlaylistItemIdPersistenceError,
};
pub use item::{PlaylistItem, PlaylistItemDraft, RestoredPlaylistItem};
pub use locator::{
    ForeignPathEncoding, ForeignPathPlatform, ForeignPlatformPath, LocalLocator, PlaylistLocator,
    PlaylistLocatorBuildError, SecretUrlLocator,
};
pub use metadata::{
    CachedMetadataError, CachedPlaylistMetadata, LocalSourceFingerprint, MAX_CACHED_ARTISTS,
    PlaylistMediaKind,
};
pub use queue::{
    AddItemsError, AddItemsOutcome, AddPlaylistEntriesOutcome, AllocatedPlaylistEntries,
    AllocatedPlaylistItemIds, ApplyPreparedCanonicalSortError, ApplyPreparedCanonicalSortOutcome,
    AutomaticEndedIntent, AutomaticNavigationOutcome, AutomaticStopReason,
    AutomaticTraversalAdvance, AutomaticTraversalCommit, AutomaticTraversalPlan,
    AutomaticTraversalStart, BulkRemoveError, BulkRemoveOutcome, CanonicalSortPreparationCancelled,
    CanonicalSortPreparationStatistics, CanonicalSortSnapshot, CappedPlaylistEntriesAppendOutcome,
    CappedTailAppendOutcome, ClearQueueOutcome, DiscardedManualNavigationPreview,
    DiscoveryBatchInsertError, DiscoveryBatchInsertOutcome, FailedManualNavigationTarget,
    MAX_PLAYLIST_ITEMS, MAX_SHUFFLE_HISTORY_ENTRIES, ManualNavigationCommit,
    ManualNavigationDirection, ManualNavigationIntent, ManualNavigationNoItem,
    ManualNavigationOrigin, ManualNavigationOutcome, ManualNavigationPreview,
    ManualNavigationPreviewError, ManualNavigationPreviewState, MetadataPatchBatchError,
    MetadataPatchBatchOutcome, MetadataPatchItemOutcome, MoveItemIntent, MoveItemOutcome,
    MoveItemsOutcome, OwnedPlayableItemsSnapshot, PlaylistEntriesMutationError,
    PlaylistMetadataPatch, PlaylistQueue, PlaylistQueueRestore, PlaylistRemovalSnapshot,
    PlaylistSortKey, PrepareAutomaticTraversalFailure, PrepareManualNavigationFailure,
    PrepareReservedMutationError, PreparedAutomaticTraversalToken, PreparedCanonicalSort,
    PreparedCanonicalSortCommit, PreparedManualNavigationToken, PreparedMetadataPatchBatchCommit,
    PreparedQueueMutationToken, QueueRestoreError, QueueRevision, QueueRevisionSnapshot,
    RemovalCurrentOutcome, RemovalSnapshotRestoreError, RemovalSnapshotRestoreOutcome,
    RemoveItemOutcome, ReplacePlaylistEntriesOutcome, ReplaceQueueError, ReplaceQueueOutcome,
    ReservedMutationCommit, ReservedQueueMutation, ShuffleHistoryCursor, ShuffleQueueRestoreError,
    ShuffleToggleError, ShuffleToggleOutcome, ShuffleTraversalRestoreError,
    ShuffleTraversalSnapshot, SortCanonicalQueue, SortCanonicalQueueOutcome, SortDirection,
    StableInsertionAnchor, TraversalCurrentEffect, TraversalCurrentItemId,
    TraversalCurrentMutationError, TraversalCurrentMutationOutcome,
    TraversalCurrentValidationError,
};
pub use repeat::RepeatMode;
