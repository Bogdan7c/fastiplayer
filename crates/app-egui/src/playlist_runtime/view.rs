//! Revision-stable read-only view без per-frame полного clone/scan очереди.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use media_core::MediaDuration;
use playlist_core::{
    CachedPlaylistMetadata, PlaylistEntry, PlaylistEntryId, PlaylistItemId, PlaylistMediaKind,
    PlaylistQueue, RepeatMode, TraversalCurrentItemId,
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
    entry_id: PlaylistEntryId,
    item_id: PlaylistItemId,
    fallback_display_name: Arc<str>,
    display_title: Arc<str>,
    duration: Option<MediaDuration>,
    media_kind: PlaylistMediaKind,
}

/// Только запрошенные видимые строки получают лёгкие clones `Arc`.
#[derive(Debug, Clone)]
pub(crate) struct PlaylistVisibleRow {
    entry_id: PlaylistEntryId,
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

/// Именованное runtime-состояние не заставляет projection callsite передавать мутные bool-ы.
pub(super) struct PlaylistVisibleRowState {
    /// Подтверждённый installed media относится к этой presentation-строке.
    pub(super) active: bool,
    /// Strong-open этой presentation-строки ещё не завершён.
    pub(super) pending: bool,
    /// Structural selection применяется только к top-level entry.
    pub(super) selected: bool,
    /// Exact item error не смешивается с pending либо отсутствующим ресурсом.
    pub(super) runtime_error: Option<PlaylistItemRuntimeError>,
}

impl PlaylistVisibleRow {
    /// Строит renderer-neutral presentation из owner-provided metadata и runtime state.
    pub(super) fn from_cached_metadata(
        entry_id: PlaylistEntryId,
        item_id: PlaylistItemId,
        metadata: &CachedPlaylistMetadata,
        state: PlaylistVisibleRowState,
    ) -> Self {
        // Fallback сохраняется отдельно для tooltip/accessibility, даже если есть title.
        let fallback_display_name: Arc<str> = Arc::from(metadata.fallback_display_name());
        // Пустой metadata title не должен вытеснять безопасное fallback-имя.
        let display_title = metadata
            .title()
            .filter(|title| !title.trim().is_empty())
            .map_or_else(|| fallback_display_name.clone(), Arc::from);
        Self {
            entry_id,
            item_id,
            fallback_display_name,
            display_title,
            duration: metadata.duration(),
            media_kind: metadata.media_kind(),
            active: state.active,
            pending: state.pending,
            selected: state.selected,
            runtime_error: state.runtime_error,
        }
    }
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
    /// Возвращает top-level structural identity строки.
    pub(crate) const fn entry_id(&self) -> PlaylistEntryId {
        self.entry_id
    }

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
            entry_id: PlaylistEntryId::Single(fixture.item_id),
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
    entry_indices: Arc<HashMap<PlaylistEntryId, usize>>,
    errors: Arc<HashMap<PlaylistItemId, PlaylistItemRuntimeError>>,
    selection: PlaylistSelectionSnapshot,
    traversal_current: Option<TraversalCurrentItemId>,
    active_media: Option<ActiveMediaIdentity>,
    pending_target: Option<PendingTarget>,
    repeat_mode: RepeatMode,
    shuffle_enabled: bool,
    structural_action_availability: PlaylistStructuralActionAvailability,
    worker_availability: PlaylistWorkerAvailability,
    navigation_failure_target: Option<PlaylistItemId>,
    active_tombstone: bool,
}

impl PlaylistViewSnapshot {
    pub(super) fn initial(queue: &PlaylistQueue) -> Self {
        let built_rows = build_rows(queue);
        Self {
            revision: PlaylistViewRevision::INITIAL,
            structural_revision: PlaylistStructuralRevision::INITIAL,
            rows: built_rows.rows,
            row_indices: built_rows.row_indices,
            entry_indices: built_rows.entry_indices,
            errors: Arc::new(HashMap::new()),
            selection: PlaylistSelectionSnapshot::empty(),
            traversal_current: queue.traversal_current(),
            active_media: None,
            pending_target: None,
            repeat_mode: RepeatMode::StopAtEnd,
            shuffle_enabled: queue.shuffle_enabled(),
            structural_action_availability: PlaylistStructuralActionAvailability::Available,
            worker_availability: PlaylistWorkerAvailability::Available,
            navigation_failure_target: None,
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

    /// O(1) lookup structural header не зависит от числа частей группы.
    pub(crate) fn entry_row_index(&self, entry_id: PlaylistEntryId) -> Option<usize> {
        self.entry_indices.get(&entry_id).copied()
    }

    /// Один direct row access обновляет top-visible anchor без поиска по queue.
    pub(crate) fn item_id_at(&self, row_index: usize) -> Option<PlaylistItemId> {
        self.rows.get(row_index).map(|row| row.item_id)
    }

    /// Возвращает top-level structural identity строки.
    pub(crate) fn entry_id_at(&self, row_index: usize) -> Option<PlaylistEntryId> {
        self.rows.get(row_index).map(|row| row.entry_id)
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
                entry_id: row.entry_id,
                item_id: row.item_id,
                fallback_display_name: row.fallback_display_name.clone(),
                display_title: row.display_title.clone(),
                duration: row.duration,
                media_kind: row.media_kind,
                active: active_item_id.and_then(|item_id| self.row_indices.get(&item_id))
                    == self.entry_indices.get(&row.entry_id),
                pending: pending_item_id.and_then(|item_id| self.row_indices.get(&item_id))
                    == self.entry_indices.get(&row.entry_id),
                selected: self.selection.is_selected(row.entry_id),
                runtime_error: self.errors.get(&row.item_id).cloned(),
            })
            .collect()
    }

    pub(crate) fn selected_entry_id(&self) -> Option<PlaylistEntryId> {
        self.selection
            .interaction_cursor()
            .filter(|entry_id| self.selection.is_selected(*entry_id))
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

    pub(crate) const fn navigation_failure_target(&self) -> Option<PlaylistItemId> {
        self.navigation_failure_target
    }

    pub(crate) const fn awaiting_user_after_navigation_failure(&self) -> bool {
        self.navigation_failure_target.is_some()
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

    /// Строит deterministic read model подтверждённого active Item ID для UI tests.
    #[cfg(test)]
    pub(super) fn for_queue_with_active_item_for_test(
        queue: &PlaylistQueue,
        structural_revision: u64,
        active_item_id: Option<PlaylistItemId>,
    ) -> Self {
        // Базовый helper сохраняет production row/index construction.
        let mut snapshot = Self::for_queue_with_revision(queue, structural_revision);
        // Ненулевые fixture identities не участвуют в UI assertions.
        let fixture_identity = std::num::NonZeroU64::new(1).expect("fixture identity is non-zero");
        // Только active media меняется относительно обычного read model fixture.
        snapshot.active_media = active_item_id.map(|item_id| {
            ActiveMediaIdentity::installed(
                Some(item_id),
                super::identity::ActiveMediaLineageId::from_non_zero(fixture_identity),
                player_core::MediaInstanceId::from_non_zero(fixture_identity),
                super::PlaylistBindingGeneration(1),
            )
        });
        // Snapshot остаётся immutable после передачи ViewModel.
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
    pub navigation_failure_target: Option<PlaylistItemId>,
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
            .map_or_else(|| previous.rows.clone(), |built| built.rows.clone()),
        row_indices: rebuilt_rows.as_ref().map_or_else(
            || previous.row_indices.clone(),
            |built| built.row_indices.clone(),
        ),
        entry_indices: rebuilt_rows.map_or_else(
            || previous.entry_indices.clone(),
            |built| built.entry_indices,
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
        navigation_failure_target: state.navigation_failure_target,
        active_tombstone: state.active_tombstone,
    }
}

/// Результат одного structural прохода не смешивает индексы playable item и top-level entry.
struct BuiltPlaylistRows {
    rows: Arc<[PlaylistViewRow]>,
    row_indices: Arc<HashMap<PlaylistItemId, usize>>,
    entry_indices: Arc<HashMap<PlaylistEntryId, usize>>,
}

fn build_rows(queue: &PlaylistQueue) -> BuiltPlaylistRows {
    let mut rows = Vec::with_capacity(queue.top_level_entry_count());
    let mut row_indices = HashMap::with_capacity(queue.retained_item_count());
    let mut entry_indices = HashMap::with_capacity(queue.top_level_entry_count());
    for (row_index, entry) in queue.iter_top_level_entries().enumerate() {
        let (representative_item, metadata) = match entry {
            PlaylistEntry::Single(item) => (item, item.cached_metadata()),
            PlaylistEntry::Compound(group) => {
                let first_part = group
                    .parts()
                    .next()
                    .expect("validated compound group always retains a part");
                (first_part.item(), group.cached_summary())
            }
        };
        let fallback_display_name: Arc<str> = Arc::from(metadata.fallback_display_name());
        let display_title = metadata
            .title()
            .filter(|title| !title.trim().is_empty())
            .map_or_else(|| fallback_display_name.clone(), Arc::from);
        rows.push(PlaylistViewRow {
            entry_id: entry.entry_id(),
            item_id: representative_item.item_id(),
            fallback_display_name,
            display_title,
            duration: metadata.duration(),
            media_kind: metadata.media_kind(),
        });
        match entry {
            PlaylistEntry::Single(item) => {
                row_indices.insert(item.item_id(), row_index);
            }
            PlaylistEntry::Compound(group) => {
                for part in group.parts() {
                    row_indices.insert(part.item().item_id(), row_index);
                }
            }
        }
        entry_indices.insert(entry.entry_id(), row_index);
    }
    BuiltPlaylistRows {
        rows: rows.into(),
        row_indices: Arc::new(row_indices),
        entry_indices: Arc::new(entry_indices),
    }
}
