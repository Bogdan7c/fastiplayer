//! Revision-stable read-only view без per-frame полного clone/scan очереди.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use playlist_core::{PlaylistItemId, PlaylistQueue, RepeatMode, TraversalCurrentItemId};

use super::identity::{ActiveMediaIdentity, PendingTarget, PlaylistItemRuntimeError};

/// Controller-owned structural revision для shared row storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PlaylistStructuralRevision(u64);

impl PlaylistStructuralRevision {
    pub(super) const INITIAL: Self = Self(0);

    pub(super) fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    #[cfg(test)]
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

/// Любое изменение presentation snapshot-а получает отдельную revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PlaylistViewRevision(u64);

impl PlaylistViewRevision {
    pub(super) const INITIAL: Self = Self(0);

    pub(super) fn next_or_saturating(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    #[cfg(test)]
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

/// Persistence-facing signal остаётся typed, но store wiring намеренно отсутствует.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PlaylistDirtyRevision(u64);

impl PlaylistDirtyRevision {
    pub(super) const CLEAN: Self = Self(0);

    pub(super) fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistDirtySignal {
    revision: PlaylistDirtyRevision,
}

impl PlaylistDirtySignal {
    pub(super) const fn new(revision: PlaylistDirtyRevision) -> Self {
        Self { revision }
    }

    pub(crate) const fn revision(self) -> PlaylistDirtyRevision {
        self.revision
    }
}

/// Worker availability показывается отдельно от committed queue state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistWorkerAvailability {
    Available,
    Unavailable,
}

/// Shared immutable строка; locator label создаётся только при structural rebuild-е.
#[derive(Debug, Clone)]
struct PlaylistViewRow {
    item_id: PlaylistItemId,
    safe_label: Arc<str>,
}

/// Только запрошенные видимые строки получают лёгкие clones `Arc`.
#[derive(Debug, Clone)]
pub(crate) struct PlaylistVisibleRow {
    item_id: PlaylistItemId,
    safe_label: Arc<str>,
    runtime_error: Option<PlaylistItemRuntimeError>,
}

impl PlaylistVisibleRow {
    pub(crate) const fn item_id(&self) -> PlaylistItemId {
        self.item_id
    }

    pub(crate) fn safe_label(&self) -> &str {
        &self.safe_label
    }

    pub(crate) const fn runtime_error(&self) -> Option<&PlaylistItemRuntimeError> {
        self.runtime_error.as_ref()
    }
}

/// Cheap-clone snapshot для renderer-bound consumer-а.
#[derive(Debug, Clone)]
pub(crate) struct PlaylistViewSnapshot {
    revision: PlaylistViewRevision,
    structural_revision: PlaylistStructuralRevision,
    rows: Arc<[PlaylistViewRow]>,
    errors: Arc<HashMap<PlaylistItemId, PlaylistItemRuntimeError>>,
    selected_item_id: Option<PlaylistItemId>,
    traversal_current: Option<TraversalCurrentItemId>,
    active_media: Option<ActiveMediaIdentity>,
    pending_target: Option<PendingTarget>,
    repeat_mode: RepeatMode,
    shuffle_enabled: bool,
    structural_actions_enabled: bool,
    worker_availability: PlaylistWorkerAvailability,
}

impl PlaylistViewSnapshot {
    pub(super) fn initial(queue: &PlaylistQueue) -> Self {
        Self {
            revision: PlaylistViewRevision::INITIAL,
            structural_revision: PlaylistStructuralRevision::INITIAL,
            rows: build_rows(queue),
            errors: Arc::new(HashMap::new()),
            selected_item_id: None,
            traversal_current: queue.traversal_current(),
            active_media: None,
            pending_target: None,
            repeat_mode: RepeatMode::StopAtEnd,
            shuffle_enabled: queue.shuffle_enabled(),
            structural_actions_enabled: true,
            worker_availability: PlaylistWorkerAvailability::Available,
        }
    }

    pub(crate) const fn revision(&self) -> PlaylistViewRevision {
        self.revision
    }

    pub(crate) const fn structural_revision(&self) -> PlaylistStructuralRevision {
        self.structural_revision
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    /// Сложность строго пропорциональна bounded visible range, а не всей queue.
    pub(crate) fn visible_rows(&self, requested: Range<usize>) -> Vec<PlaylistVisibleRow> {
        let start = requested.start.min(self.rows.len());
        let end = requested.end.min(self.rows.len()).max(start);
        self.rows[start..end]
            .iter()
            .map(|row| PlaylistVisibleRow {
                item_id: row.item_id,
                safe_label: row.safe_label.clone(),
                runtime_error: self.errors.get(&row.item_id).cloned(),
            })
            .collect()
    }

    pub(crate) const fn selected_item_id(&self) -> Option<PlaylistItemId> {
        self.selected_item_id
    }

    pub(crate) const fn traversal_current(&self) -> Option<TraversalCurrentItemId> {
        self.traversal_current
    }

    pub(crate) const fn active_media(&self) -> Option<ActiveMediaIdentity> {
        self.active_media
    }

    pub(crate) const fn pending_target(&self) -> Option<PendingTarget> {
        self.pending_target
    }

    pub(crate) const fn structural_actions_enabled(&self) -> bool {
        self.structural_actions_enabled
    }

    pub(crate) const fn worker_availability(&self) -> PlaylistWorkerAvailability {
        self.worker_availability
    }

    /// Test-only pointer доказывает reuse shared structural storage между snapshots.
    #[cfg(test)]
    pub(crate) fn shared_rows_identity(&self) -> usize {
        self.rows.as_ptr() as usize
    }
}

pub(super) struct PlaylistViewState<'a> {
    pub queue: &'a PlaylistQueue,
    pub structural_revision: PlaylistStructuralRevision,
    pub errors: &'a HashMap<PlaylistItemId, PlaylistItemRuntimeError>,
    pub selected_item_id: Option<PlaylistItemId>,
    pub active_media: Option<ActiveMediaIdentity>,
    pub pending_target: Option<PendingTarget>,
    pub repeat_mode: RepeatMode,
    pub structural_actions_enabled: bool,
    pub worker_availability: PlaylistWorkerAvailability,
}

pub(super) fn rebuild_snapshot(
    previous: &PlaylistViewSnapshot,
    state: PlaylistViewState<'_>,
    structural_rows_changed: bool,
) -> PlaylistViewSnapshot {
    PlaylistViewSnapshot {
        revision: previous.revision.next_or_saturating(),
        structural_revision: state.structural_revision,
        rows: if structural_rows_changed {
            build_rows(state.queue)
        } else {
            previous.rows.clone()
        },
        errors: Arc::new(state.errors.clone()),
        selected_item_id: state.selected_item_id,
        traversal_current: state.queue.traversal_current(),
        active_media: state.active_media,
        pending_target: state.pending_target,
        repeat_mode: state.repeat_mode,
        shuffle_enabled: state.queue.shuffle_enabled(),
        structural_actions_enabled: state.structural_actions_enabled,
        worker_availability: state.worker_availability,
    }
}

fn build_rows(queue: &PlaylistQueue) -> Arc<[PlaylistViewRow]> {
    queue
        .items()
        .iter()
        .map(|item| PlaylistViewRow {
            item_id: item.item_id(),
            safe_label: Arc::from(item.cached_metadata().fallback_display_name()),
        })
        .collect::<Vec<_>>()
        .into()
}
