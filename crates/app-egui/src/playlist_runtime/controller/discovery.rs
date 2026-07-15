//! Controller boundary для progressive sibling commits.

use playlist_core::{
    DiscoveryBatchInsertError, PlaylistItemDraft, PlaylistItemId, QueueRevisionSnapshot,
    StableInsertionAnchor,
};

use super::{
    ManualNavigationInvalidation, PlaylistController, PlaylistDirtySignal,
    PlaylistStructuralRevision,
};

/// Controller-owned continuation revision одного progressive discovery stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DiscoveryContinuationRevision(u64);

impl DiscoveryContinuationRevision {
    pub(super) const INITIAL: Self = Self(0);

    fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Exact scope continuation captured immediately after target-only commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DiscoveryContinuation {
    pub revision: DiscoveryContinuationRevision,
    pub queue_revision: QueueRevisionSnapshot,
    pub structural_revision: PlaylistStructuralRevision,
}

/// Accepted batch publishes IDs, revisions и stable insertion anchor together.
#[derive(Debug)]
pub(crate) struct DiscoveryBatchCommitOutcome {
    pub item_ids: Vec<PlaylistItemId>,
    pub anchor: StableInsertionAnchor,
    pub continuation: DiscoveryContinuation,
    pub dirty: PlaylistDirtySignal,
    pub manual_navigation_invalidation: Option<ManualNavigationInvalidation>,
}

/// Rejection leaves drafts ID-less and does not advance dirty/high-watermark state.
#[derive(Debug)]
pub(crate) enum DiscoveryBatchCommitError {
    FatalInvariant,
    ContinuationMismatch,
    DirtyRevisionExhausted,
    StructuralRevisionExhausted,
    ContinuationRevisionExhausted,
    Domain(DiscoveryBatchInsertError),
}

impl PlaylistController {
    /// Starts a new scope continuation after exact target-only Installed commit.
    pub(crate) fn begin_discovery_continuation(
        &mut self,
    ) -> Result<DiscoveryContinuation, DiscoveryBatchCommitError> {
        if self.fatal_invariant.is_some() {
            return Err(DiscoveryBatchCommitError::FatalInvariant);
        }
        let revision = self
            .discovery_continuation_revision
            .checked_next()
            .ok_or(DiscoveryBatchCommitError::ContinuationRevisionExhausted)?;
        self.discovery_continuation_revision = revision;
        Ok(DiscoveryContinuation {
            revision,
            queue_revision: self.queue.revision_snapshot(),
            structural_revision: self.structural_revision,
        })
    }

    /// Проверяет scope/revisions и одной domain mutation вставляет ID-less batch.
    pub(crate) fn commit_discovery_batch(
        &mut self,
        expected: DiscoveryContinuation,
        anchor: StableInsertionAnchor,
        drafts: Vec<PlaylistItemDraft>,
    ) -> Result<DiscoveryBatchCommitOutcome, DiscoveryBatchCommitError> {
        if self.fatal_invariant.is_some() {
            return Err(DiscoveryBatchCommitError::FatalInvariant);
        }
        if expected.revision != self.discovery_continuation_revision
            || expected.structural_revision != self.structural_revision
        {
            return Err(DiscoveryBatchCommitError::ContinuationMismatch);
        }
        let next_dirty = self
            .dirty_revision
            .checked_next()
            .ok_or(DiscoveryBatchCommitError::DirtyRevisionExhausted)?;
        let next_structural = self
            .structural_revision
            .checked_next()
            .ok_or(DiscoveryBatchCommitError::StructuralRevisionExhausted)?;
        let next_continuation_revision = expected
            .revision
            .checked_next()
            .ok_or(DiscoveryBatchCommitError::ContinuationRevisionExhausted)?;
        let committed = self
            .queue
            .insert_discovery_batch(expected.queue_revision, anchor, drafts)
            .map_err(DiscoveryBatchCommitError::Domain)?;
        let item_ids = committed.item_ids.into_vec();
        let manual_navigation_invalidation =
            self.invalidate_manual_navigation_after_structural_mutation();
        self.structural_revision = next_structural;
        self.discovery_continuation_revision = next_continuation_revision;
        let dirty = self.commit_dirty(next_dirty);
        self.publish_view(true);
        Ok(DiscoveryBatchCommitOutcome {
            item_ids,
            anchor: committed.anchor,
            continuation: DiscoveryContinuation {
                revision: next_continuation_revision,
                queue_revision: self.queue.revision_snapshot(),
                structural_revision: next_structural,
            },
            dirty,
            manual_navigation_invalidation,
        })
    }
}

#[cfg(test)]
mod tests;
