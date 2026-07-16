//! App preflight/commit boundary для stale-safe metadata-only mutation.

use playlist_core::{
    MetadataPatchBatchError, MetadataPatchBatchOutcome, PlaylistMetadataPatch,
    PreparedMetadataPatchBatchCommit,
};

use super::*;

/// Metadata cache mutation не смешивает domain revision и app dirty failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControllerMetadataPatchError {
    FatalInvariant,
    DirtyRevisionExhausted,
    Domain(MetadataPatchBatchError),
}

/// Metadata patch публикует dirty signal только при реальном cache изменении.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControllerMetadataPatchOutcome {
    pub(crate) domain: MetadataPatchBatchOutcome,
    pub(crate) dirty: Option<PlaylistDirtySignal>,
}

/// Проверенный app/domain commit без fallible шага после Undo invalidation.
pub(crate) struct PreparedControllerMetadataPatchCommit {
    domain: PreparedMetadataPatchBatchCommit,
    next_dirty: PlaylistDirtyRevision,
}

impl PreparedControllerMetadataPatchCommit {
    #[must_use]
    pub(crate) fn changed_persistent_state(&self) -> bool {
        self.domain.changed_metadata()
    }
}

impl PlaylistController {
    #[cfg(test)]
    pub(crate) fn force_metadata_dirty_revision_exhaustion_for_test(&mut self) {
        self.reject_metadata_dirty_preflight_for_test = true;
    }

    /// Revalidates весь batch и app/domain counters без mutation.
    pub(crate) fn preflight_metadata_patches(
        &self,
        patches: Vec<PlaylistMetadataPatch>,
    ) -> Result<PreparedControllerMetadataPatchCommit, ControllerMetadataPatchError> {
        if self.fatal_invariant.is_some() {
            return Err(ControllerMetadataPatchError::FatalInvariant);
        }
        #[cfg(test)]
        if self.reject_metadata_dirty_preflight_for_test {
            return Err(ControllerMetadataPatchError::DirtyRevisionExhausted);
        }
        let domain = self
            .queue
            .preflight_metadata_patch_batch(patches)
            .map_err(ControllerMetadataPatchError::Domain)?;
        let next_dirty = if domain.changed_metadata() {
            self.dirty_revision
                .checked_next()
                .ok_or(ControllerMetadataPatchError::DirtyRevisionExhausted)?
        } else {
            self.dirty_revision
        };
        Ok(PreparedControllerMetadataPatchCommit { domain, next_dirty })
    }

    /// Публикует только что preflighted metadata batch без fallible шага.
    pub(crate) fn commit_metadata_patches(
        &mut self,
        commit: PreparedControllerMetadataPatchCommit,
    ) -> ControllerMetadataPatchOutcome {
        let changed_metadata = commit.domain.changed_metadata();
        let domain = self.queue.commit_metadata_patch_batch(commit.domain);
        let dirty = changed_metadata.then(|| self.commit_dirty(commit.next_dirty));
        if dirty.is_some() {
            self.publish_view(true);
        }
        ControllerMetadataPatchOutcome { domain, dirty }
    }

    /// Compatibility intent-method делегирует в typed two-phase boundary.
    pub(crate) fn apply_metadata_patches(
        &mut self,
        patches: Vec<PlaylistMetadataPatch>,
    ) -> Result<ControllerMetadataPatchOutcome, ControllerMetadataPatchError> {
        let commit = self.preflight_metadata_patches(patches)?;
        Ok(self.commit_metadata_patches(commit))
    }
}
