//! Controller boundary для preflighted combined metadata/order Sort commit.

#[cfg(test)]
mod tests;

use playlist_core::{
    ApplyPreparedCanonicalSortError, ApplyPreparedCanonicalSortOutcome, PlaylistMetadataPatch,
    PreparedCanonicalSort, PreparedCanonicalSortCommit,
};

use super::*;

/// Combined Sort preflight различает stale app snapshot и domain failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControllerCanonicalSortError {
    FatalInvariant,
    StaleStructuralRevision,
    DirtyRevisionExhausted,
    StructuralRevisionExhausted,
    Domain(ApplyPreparedCanonicalSortError),
}

/// Linear commit удерживается только внутри одного runtime terminal drain.
pub(crate) struct PreparedControllerCanonicalSortCommit {
    domain: PreparedCanonicalSortCommit,
    next_dirty: Option<PlaylistDirtyRevision>,
    next_structural: Option<PlaylistStructuralRevision>,
}

impl PreparedControllerCanonicalSortCommit {
    pub(crate) fn changed_persistent_state(&self) -> bool {
        self.domain.changed_persistent_state()
    }
}

/// Один app dirty publication поверх metadata+order domain commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControllerCanonicalSortOutcome {
    pub(crate) domain: ApplyPreparedCanonicalSortOutcome,
    pub(crate) dirty: Option<PlaylistDirtySignal>,
    pub(crate) manual_navigation_invalidation: Option<ManualNavigationInvalidation>,
}

impl PlaylistController {
    /// Проверяет app/domain revisions и строит linear exactly-one commit.
    pub(crate) fn preflight_canonical_sort(
        &self,
        expected_structural_revision: PlaylistStructuralRevision,
        prepared: PreparedCanonicalSort,
        metadata_patches: Vec<PlaylistMetadataPatch>,
    ) -> Result<PreparedControllerCanonicalSortCommit, ControllerCanonicalSortError> {
        if self.fatal_invariant.is_some() {
            return Err(ControllerCanonicalSortError::FatalInvariant);
        }
        if self.structural_revision != expected_structural_revision {
            return Err(ControllerCanonicalSortError::StaleStructuralRevision);
        }
        let domain = self
            .queue
            .preflight_prepared_canonical_sort(prepared, metadata_patches)
            .map_err(ControllerCanonicalSortError::Domain)?;
        let changed = domain.changed_persistent_state();
        let next_dirty = if changed {
            Some(
                self.dirty_revision
                    .checked_next()
                    .ok_or(ControllerCanonicalSortError::DirtyRevisionExhausted)?,
            )
        } else {
            None
        };
        let next_structural = if domain.reordered() {
            Some(
                self.structural_revision
                    .checked_next()
                    .ok_or(ControllerCanonicalSortError::StructuralRevisionExhausted)?,
            )
        } else {
            None
        };
        Ok(PreparedControllerCanonicalSortCommit {
            domain,
            next_dirty,
            next_structural,
        })
    }

    /// Применяет только что проверенный Sort без fallible шага или промежуточного view.
    pub(crate) fn commit_canonical_sort(
        &mut self,
        prepared: PreparedControllerCanonicalSortCommit,
    ) -> ControllerCanonicalSortOutcome {
        let reordered = prepared.domain.reordered();
        let domain = self.queue.commit_prepared_canonical_sort(prepared.domain);
        let manual_navigation_invalidation = reordered
            .then(|| self.invalidate_manual_navigation_after_structural_mutation())
            .flatten();
        if let Some(next_structural) = prepared.next_structural {
            self.structural_revision = next_structural;
        }
        let dirty = prepared
            .next_dirty
            .map(|revision| self.commit_dirty(revision));
        if dirty.is_some() {
            self.publish_view(reordered);
        }
        ControllerCanonicalSortOutcome {
            domain,
            dirty,
            manual_navigation_invalidation,
        }
    }
}
