//! Revision-stable read-only view без per-frame полного clone/scan очереди.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use media_core::MediaDuration;
use playlist_core::{
    PlaylistItemId, PlaylistMediaKind, PlaylistQueue, RepeatMode, TraversalCurrentItemId,
};

use super::identity::{ActiveMediaIdentity, PendingTarget, PlaylistItemRuntimeError};
use super::selection::PlaylistSelectionSnapshot;

/// Controller-owned structural revision для shared row storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PlaylistStructuralRevision(u64);

impl PlaylistStructuralRevision {
    pub(super) const INITIAL: Self = Self(0);

    pub(super) fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

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

/// Доступность структурных действий без утечки конкретной install-фазы в UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistStructuralActionAvailability {
    /// Controller может сразу принять действие над составом или порядком очереди.
    Available,
    /// Краткий commit-guard запрещает действие, но не является пользовательской ошибкой.
    TemporarilyBlocked,
    /// Controller больше не может безопасно принимать структурные действия.
    Unavailable,
}

impl PlaylistStructuralActionAvailability {
    /// Разрешает публиковать structural intent только в устойчиво доступном состоянии.
    pub(crate) const fn allows_interaction(self) -> bool {
        matches!(self, Self::Available)
    }

    /// Inline-объяснение нужно только для устойчивой недоступности.
    pub(crate) const fn requires_status_notice(self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

/// Shared immutable строка; locator label создаётся только при structural rebuild-е.
#[derive(Debug, Clone)]
struct PlaylistViewRow {
    item_id: PlaylistItemId,
    fallback_display_name: Arc<str>,
    display_title: Arc<str>,
    duration: Option<MediaDuration>,
    media_kind: PlaylistMediaKind,
}

/// Только запрошенные видимые строки получают лёгкие clones `Arc`.
#[derive(Debug, Clone)]
pub(crate) struct PlaylistVisibleRow {
    item_id: PlaylistItemId,
    fallback_display_name: Arc<str>,
    display_title: Arc<str>,
    duration: Option<MediaDuration>,
    media_kind: PlaylistMediaKind,
    active: bool,
    pending: bool,
    selected: bool,
    runtime_error: Option<PlaylistItemRuntimeError>,
}

/// Именованный fixture не даёт тестам перепутать визуальные состояния строки.
#[cfg(test)]
pub(crate) struct PlaylistVisibleRowTestFixture {
    pub(crate) item_id: PlaylistItemId,
    pub(crate) fallback_display_name: String,
    pub(crate) display_title: String,
    pub(crate) duration: Option<MediaDuration>,
    pub(crate) media_kind: PlaylistMediaKind,
    pub(crate) active: bool,
    pub(crate) pending: bool,
    pub(crate) selected: bool,
    pub(crate) safe_error_summary: Option<String>,
}

impl PlaylistVisibleRow {
    pub(crate) const fn item_id(&self) -> PlaylistItemId {
        self.item_id
    }

    pub(crate) fn fallback_display_name(&self) -> &str {
        &self.fallback_display_name
    }

    pub(crate) fn display_title(&self) -> &str {
        &self.display_title
    }

    pub(crate) const fn duration(&self) -> Option<MediaDuration> {
        self.duration
    }

    pub(crate) const fn media_kind(&self) -> PlaylistMediaKind {
        self.media_kind
    }

    pub(crate) const fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) const fn is_pending(&self) -> bool {
        self.pending
    }

    pub(crate) const fn is_selected(&self) -> bool {
        self.selected
    }

    pub(crate) const fn runtime_error(&self) -> Option<&PlaylistItemRuntimeError> {
        self.runtime_error.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn from_test_fixture(fixture: PlaylistVisibleRowTestFixture) -> Self {
        let runtime_error = fixture.safe_error_summary.map(|summary| {
            PlaylistItemRuntimeError::first(
                super::identity::PlaylistItemErrorPhase::SourceUnavailable,
                super::identity::PlaylistItemErrorCategory::Unavailable,
                Arc::from(summary),
                None,
                None,
            )
        });
        Self {
            item_id: fixture.item_id,
            fallback_display_name: Arc::from(fixture.fallback_display_name),
            display_title: Arc::from(fixture.display_title),
            duration: fixture.duration,
            media_kind: fixture.media_kind,
            active: fixture.active,
            pending: fixture.pending,
            selected: fixture.selected,
            runtime_error,
        }
    }
}

/// Cheap-clone snapshot для renderer-bound consumer-а.
#[derive(Debug, Clone)]
pub(crate) struct PlaylistViewSnapshot {
    revision: PlaylistViewRevision,
    structural_revision: PlaylistStructuralRevision,
    rows: Arc<[PlaylistViewRow]>,
    row_indices: Arc<HashMap<PlaylistItemId, usize>>,
    errors: Arc<HashMap<PlaylistItemId, PlaylistItemRuntimeError>>,
    selection: PlaylistSelectionSnapshot,
    traversal_current: Option<TraversalCurrentItemId>,
    active_media: Option<ActiveMediaIdentity>,
    pending_target: Option<PendingTarget>,
    repeat_mode: RepeatMode,
    shuffle_enabled: bool,
    structural_action_availability: PlaylistStructuralActionAvailability,
    worker_availability: PlaylistWorkerAvailability,
    awaiting_user_after_navigation_failure: bool,
    active_tombstone: bool,
}

impl PlaylistViewSnapshot {
    pub(super) fn initial(queue: &PlaylistQueue) -> Self {
        let (rows, row_indices) = build_rows(queue);
        Self {
            revision: PlaylistViewRevision::INITIAL,
            structural_revision: PlaylistStructuralRevision::INITIAL,
            rows,
            row_indices,
            errors: Arc::new(HashMap::new()),
            selection: PlaylistSelectionSnapshot::empty(),
            traversal_current: queue.traversal_current(),
            active_media: None,
            pending_target: None,
            repeat_mode: RepeatMode::StopAtEnd,
            shuffle_enabled: queue.shuffle_enabled(),
            structural_action_availability: PlaylistStructuralActionAvailability::Available,
            worker_availability: PlaylistWorkerAvailability::Available,
            awaiting_user_after_navigation_failure: false,
            active_tombstone: false,
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

    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// O(1) lookup нужен UI-якорю после incremental structural publication.
    pub(crate) fn row_index(&self, item_id: PlaylistItemId) -> Option<usize> {
        self.row_indices.get(&item_id).copied()
    }

    /// Один direct row access обновляет top-visible anchor без поиска по queue.
    pub(crate) fn item_id_at(&self, row_index: usize) -> Option<PlaylistItemId> {
        self.rows.get(row_index).map(|row| row.item_id)
    }

    /// Сложность строго пропорциональна bounded visible range, а не всей queue.
    pub(crate) fn visible_rows(&self, requested: Range<usize>) -> Vec<PlaylistVisibleRow> {
        let start = requested.start.min(self.rows.len());
        let end = requested.end.min(self.rows.len()).max(start);
        let active_item_id = self.active_media.and_then(ActiveMediaIdentity::item_id);
        let pending_item_id = self.pending_target.and_then(PendingTarget::item_id);
        self.rows[start..end]
            .iter()
            .map(|row| PlaylistVisibleRow {
                item_id: row.item_id,
                fallback_display_name: row.fallback_display_name.clone(),
                display_title: row.display_title.clone(),
                duration: row.duration,
                media_kind: row.media_kind,
                active: active_item_id == Some(row.item_id),
                pending: pending_item_id == Some(row.item_id),
                selected: self.selection.is_selected(row.item_id),
                runtime_error: self.errors.get(&row.item_id).cloned(),
            })
            .collect()
    }

    pub(crate) fn selected_item_id(&self) -> Option<PlaylistItemId> {
        self.selection
            .interaction_cursor()
            .filter(|item_id| self.selection.is_selected(*item_id))
    }

    /// Возвращает Arc-backed selection snapshot без копирования selected set.
    pub(crate) fn selection(&self) -> &PlaylistSelectionSnapshot {
        &self.selection
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

    pub(crate) const fn structural_action_availability(
        &self,
    ) -> PlaylistStructuralActionAvailability {
        self.structural_action_availability
    }

    pub(crate) const fn worker_availability(&self) -> PlaylistWorkerAvailability {
        self.worker_availability
    }

    pub(crate) const fn awaiting_user_after_navigation_failure(&self) -> bool {
        self.awaiting_user_after_navigation_failure
    }

    pub(crate) const fn has_active_tombstone(&self) -> bool {
        self.active_tombstone
    }

    /// Test-only pointer доказывает reuse shared structural storage между snapshots.
    #[cfg(test)]
    pub(crate) fn shared_rows_identity(&self) -> usize {
        self.rows.as_ptr() as usize
    }

    #[cfg(test)]
    pub(crate) fn shared_title_identity(&self, row_index: usize) -> Option<usize> {
        self.rows
            .get(row_index)
            .map(|row| row.display_title.as_ptr() as usize)
    }

    #[cfg(test)]
    pub(super) fn for_queue_with_revision(queue: &PlaylistQueue, structural_revision: u64) -> Self {
        let mut snapshot = Self::initial(queue);
        snapshot.structural_revision = PlaylistStructuralRevision(structural_revision);
        snapshot
    }
}

pub(super) struct PlaylistViewState<'a> {
    pub queue: &'a PlaylistQueue,
    pub structural_revision: PlaylistStructuralRevision,
    pub errors: &'a HashMap<PlaylistItemId, PlaylistItemRuntimeError>,
    pub selection: PlaylistSelectionSnapshot,
    pub active_media: Option<ActiveMediaIdentity>,
    pub pending_target: Option<PendingTarget>,
    pub repeat_mode: RepeatMode,
    pub structural_action_availability: PlaylistStructuralActionAvailability,
    pub worker_availability: PlaylistWorkerAvailability,
    pub awaiting_user_after_navigation_failure: bool,
    pub active_tombstone: bool,
}

pub(super) fn rebuild_snapshot(
    previous: &PlaylistViewSnapshot,
    state: PlaylistViewState<'_>,
    structural_rows_changed: bool,
) -> PlaylistViewSnapshot {
    let rebuilt_rows = structural_rows_changed.then(|| build_rows(state.queue));
    PlaylistViewSnapshot {
        revision: previous.revision.next_or_saturating(),
        structural_revision: state.structural_revision,
        rows: rebuilt_rows
            .as_ref()
            .map_or_else(|| previous.rows.clone(), |(rows, _)| rows.clone()),
        row_indices: rebuilt_rows.map_or_else(
            || previous.row_indices.clone(),
            |(_, row_indices)| row_indices,
        ),
        errors: Arc::new(state.errors.clone()),
        selection: state.selection,
        traversal_current: state.queue.traversal_current(),
        active_media: state.active_media,
        pending_target: state.pending_target,
        repeat_mode: state.repeat_mode,
        shuffle_enabled: state.queue.shuffle_enabled(),
        structural_action_availability: state.structural_action_availability,
        worker_availability: state.worker_availability,
        awaiting_user_after_navigation_failure: state.awaiting_user_after_navigation_failure,
        active_tombstone: state.active_tombstone,
    }
}

fn build_rows(
    queue: &PlaylistQueue,
) -> (Arc<[PlaylistViewRow]>, Arc<HashMap<PlaylistItemId, usize>>) {
    let mut rows = Vec::with_capacity(queue.len());
    let mut row_indices = HashMap::with_capacity(queue.len());
    for (row_index, item) in queue.items().iter().enumerate() {
        let metadata = item.cached_metadata();
        let fallback_display_name: Arc<str> = Arc::from(metadata.fallback_display_name());
        let display_title = metadata
            .title()
            .filter(|title| !title.trim().is_empty())
            .map_or_else(|| fallback_display_name.clone(), Arc::from);
        rows.push(PlaylistViewRow {
            item_id: item.item_id(),
            fallback_display_name,
            display_title,
            duration: metadata.duration(),
            media_kind: metadata.media_kind(),
        });
        row_indices.insert(item.item_id(), row_index);
    }
    (rows.into(), Arc::new(row_indices))
}
