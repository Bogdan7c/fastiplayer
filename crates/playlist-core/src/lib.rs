//! Чистая доменная модель canonical очереди воспроизведения.
//!
//! Crate намеренно не знает про UI, player, I/O, serde и filesystem discovery.
//! Он владеет стабильной идентичностью строк, allocator high-watermark,
//! canonical order и атомарными mutation boundaries Session 02.

mod id;
mod item;
mod locator;
mod metadata;
mod queue;

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
    AddItemsError, AddItemsOutcome, AllocatedPlaylistItemIds, ClearQueueOutcome,
    MAX_PLAYLIST_ITEMS, MetadataPatchBatchError, MetadataPatchBatchOutcome,
    MetadataPatchItemOutcome, MoveItemIntent, MoveItemOutcome, PlaylistMetadataPatch,
    PlaylistQueue, PlaylistQueueRestore, PrepareReservedMutationError, PreparedQueueMutationToken,
    QueueRestoreError, QueueRevision, QueueRevisionSnapshot, RemoveItemOutcome, ReplaceQueueError,
    ReplaceQueueOutcome, ReservedMutationCommit, ReservedQueueMutation, TraversalCurrentEffect,
    TraversalCurrentItemId, TraversalCurrentMutationError, TraversalCurrentMutationOutcome,
    TraversalCurrentValidationError,
};
