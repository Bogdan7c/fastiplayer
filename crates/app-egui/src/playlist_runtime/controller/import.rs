//! Atomic source-neutral import commit boundary.

use playlist_core::{
    AddPlaylistEntriesOutcome, AllocatedPlaylistEntries, PlaylistEntriesMutationError,
    PlaylistEntryDraft, ReplacePlaylistEntriesOutcome,
};

use super::{
    ManualNavigationInvalidation, PlaylistController, PlaylistDirtySignal,
    PlaylistStructuralRevision,
};

/// Семантика replacement задаётся типом, а не позиционным `bool`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImportReplacementDisposition {
    /// Пользовательский Replace сохраняет старое media как detached active.
    InteractiveDetached,
    /// Trusted startup commit не включает special manual-navigation projection.
    Startup,
}

/// Отдельный marker интерактивного replacement-detached lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReplacementDetachedDisposition;

/// Exact receipt successful import mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ControllerImportCommitOutcome {
    /// Пустой accepted prefix не меняет queue/revisions/allocator-ы.
    NoEntriesProvided,
    /// Весь переданный prefix committed одной domain mutation.
    Committed {
        /// Structural и playable IDs, выделенные только domain owner-ом.
        allocated: AllocatedPlaylistEntries,
        /// Новый dirty receipt для persistence owner-а.
        dirty: PlaylistDirtySignal,
        /// Отмена stale manual navigation без скрытого target reuse.
        manual_navigation_invalidation: Option<ManualNavigationInvalidation>,
    },
}

/// До domain commit все ошибки оставляют queue и allocator-ы неизменными.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ControllerImportCommitError {
    /// Runtime/controller уже находится в terminal invariant state.
    FatalInvariant,
    /// Preview относится к другой structural revision.
    StaleStructuralRevision {
        /// Revision, при которой preview был опубликован.
        expected: PlaylistStructuralRevision,
        /// Текущая serialized owner revision.
        actual: PlaylistStructuralRevision,
    },
    /// Любой install state должен terminal-resolve до destructive import.
    InstallInProgress,
    /// App dirty revision исчерпана до domain mutation.
    DirtyRevisionExhausted,
    /// App structural revision исчерпана до domain mutation.
    StructuralRevisionExhausted,
    /// Domain preflight/allocation error до публикации IDs.
    Domain(PlaylistEntriesMutationError),
}

impl PlaylistController {
    /// Append сохраняет active/current/playback и не запускает media open.
    pub(crate) fn commit_import_append(
        &mut self,
        expected_revision: PlaylistStructuralRevision,
        drafts: Vec<PlaylistEntryDraft>,
    ) -> Result<ControllerImportCommitOutcome, ControllerImportCommitError> {
        let revisions = self.preflight_import_commit(expected_revision, drafts.is_empty())?;
        let Some((next_dirty, next_structural)) = revisions else {
            return Ok(ControllerImportCommitOutcome::NoEntriesProvided);
        };
        let allocated = match self
            .queue
            .append_entries(drafts)
            .map_err(ControllerImportCommitError::Domain)?
        {
            AddPlaylistEntriesOutcome::NoEntriesProvided => {
                return Ok(ControllerImportCommitOutcome::NoEntriesProvided);
            }
            AddPlaylistEntriesOutcome::Added(allocated) => allocated,
        };
        Ok(self.finish_import_commit(allocated, next_dirty, next_structural))
    }

    /// Replace атомарно публикует новую queue и отдельно detach-ит old active.
    pub(crate) fn commit_import_replace(
        &mut self,
        expected_revision: PlaylistStructuralRevision,
        drafts: Vec<PlaylistEntryDraft>,
        disposition: ImportReplacementDisposition,
    ) -> Result<ControllerImportCommitOutcome, ControllerImportCommitError> {
        let revisions = self.preflight_import_commit(expected_revision, drafts.is_empty())?;
        let Some((next_dirty, next_structural)) = revisions else {
            return Ok(ControllerImportCommitOutcome::NoEntriesProvided);
        };
        let allocated = match self
            .queue
            .replace_entries(drafts)
            .map_err(ControllerImportCommitError::Domain)?
        {
            ReplacePlaylistEntriesOutcome::Replaced {
                allocated_entries, ..
            } => allocated_entries,
            ReplacePlaylistEntriesOutcome::AlreadyEmpty
            | ReplacePlaylistEntriesOutcome::Cleared { .. } => {
                return Ok(ControllerImportCommitOutcome::NoEntriesProvided);
            }
        };

        self.active_media = self.active_media.map(super::ActiveMediaIdentity::detached);
        self.detached_active_tombstone = None;
        self.replacement_detached_disposition = match disposition {
            ImportReplacementDisposition::InteractiveDetached => {
                Some(ReplacementDetachedDisposition)
            }
            ImportReplacementDisposition::Startup => None,
        };
        self.selection = super::PlaylistSelectionState::default();
        self.runtime_errors.clear();
        self.pending_target = None;
        self.pending_manual_traversal = None;

        let outcome = self.finish_import_commit(allocated, next_dirty, next_structural);
        self.automatic_lifecycle = Default::default();
        Ok(outcome)
    }

    /// Проверяет serialized revision и app counters до domain mutation.
    fn preflight_import_commit(
        &self,
        expected_revision: PlaylistStructuralRevision,
        empty_batch: bool,
    ) -> Result<
        Option<(super::PlaylistDirtyRevision, PlaylistStructuralRevision)>,
        ControllerImportCommitError,
    > {
        if self.fatal_invariant.is_some() {
            return Err(ControllerImportCommitError::FatalInvariant);
        }
        let actual_revision = self.view_snapshot().structural_revision();
        if actual_revision != expected_revision {
            return Err(ControllerImportCommitError::StaleStructuralRevision {
                expected: expected_revision,
                actual: actual_revision,
            });
        }
        if self.install_state.is_some() {
            return Err(ControllerImportCommitError::InstallInProgress);
        }
        if empty_batch {
            return Ok(None);
        }
        let next_dirty = self
            .dirty_revision
            .checked_next()
            .ok_or(ControllerImportCommitError::DirtyRevisionExhausted)?;
        let next_structural = self
            .structural_revision
            .checked_next()
            .ok_or(ControllerImportCommitError::StructuralRevisionExhausted)?;
        Ok(Some((next_dirty, next_structural)))
    }

    /// Завершает общую app-side publication после successful domain commit.
    fn finish_import_commit(
        &mut self,
        allocated: AllocatedPlaylistEntries,
        next_dirty: super::PlaylistDirtyRevision,
        next_structural: PlaylistStructuralRevision,
    ) -> ControllerImportCommitOutcome {
        let manual_navigation_invalidation =
            self.invalidate_manual_navigation_after_structural_mutation();
        self.structural_revision = next_structural;
        let dirty = self.commit_dirty(next_dirty);
        self.publish_view(true);
        ControllerImportCommitOutcome::Committed {
            allocated,
            dirty,
            manual_navigation_invalidation,
        }
    }
}
