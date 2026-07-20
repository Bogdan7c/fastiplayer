//! Runtime-owned immutable model для read-only Playlist sidebar.

use std::ops::Range;
use std::sync::Arc;

use playlist_core::{PlaylistEntryId, PlaylistItemId};
use playlist_state::SaveRevision;

use super::PlaylistRuntime;
use super::controller::SiblingDiscoveryScopeId;
use super::discovery::{PlaylistDiscoveryNavigationStatus, PlaylistDiscoveryStatus};
use super::persistence::{
    PlaylistPersistenceFault, PlaylistPersistenceView, PlaylistSaveDurability,
};
use super::view::{PlaylistStructuralRevision, PlaylistViewSnapshot, PlaylistVisibleRow};

/// Bounded probe/discovery summary уже не содержит filesystem locator-ов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistProbeView {
    Idle,
    Warning { scope_id: SiblingDiscoveryScopeId },
}

/// Typed identity одной persistence-попытки не зависит от форматированного текста.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaylistSaveAttempt {
    revision: SaveRevision,
    occurrence_count: u32,
}

impl PlaylistSaveAttempt {
    /// Presentation читает только счётчик для privacy-safe текста.
    pub(crate) const fn occurrence_count(self) -> u32 {
        self.occurrence_count
    }

    #[cfg(test)]
    pub(crate) const fn for_test(occurrence_count: u32) -> Self {
        Self {
            revision: SaveRevision::FIRST,
            occurrence_count,
        }
    }
}

/// Persistence summary сохраняет различие block, retryable warning и terminal fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistSaveView {
    Idle,
    Saving,
    WarningRetryAvailable { attempt: PlaylistSaveAttempt },
    Blocked,
    Fault(PlaylistPersistenceFault),
}

/// Navigation UI получает только typed problem state без cursor/preview ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistNavigationView {
    Idle,
    AwaitingUserAfterFailure {
        item_id: PlaylistItemId,
        origin_already_ended: bool,
    },
    Fatal {
        scope_id: SiblingDiscoveryScopeId,
    },
}

/// Startup warning намеренно остаётся общей privacy-safe категорией.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistStartupWarningView {
    None,
    Present,
}

/// Cheap-clone model: full queue strings живут только в revision-stable snapshot-е.
#[derive(Debug, Clone)]
pub(crate) struct PlaylistViewModel {
    snapshot: Arc<PlaylistViewSnapshot>,
    probe: PlaylistProbeView,
    save: PlaylistSaveView,
    navigation: PlaylistNavigationView,
    startup_warning: PlaylistStartupWarningView,
}

impl PlaylistViewModel {
    #[cfg(test)]
    pub(crate) fn for_queue_with_revision(
        queue: &playlist_core::PlaylistQueue,
        structural_revision: u64,
    ) -> Self {
        Self {
            snapshot: Arc::new(PlaylistViewSnapshot::for_queue_with_revision(
                queue,
                structural_revision,
            )),
            probe: PlaylistProbeView::Idle,
            save: PlaylistSaveView::Idle,
            navigation: PlaylistNavigationView::Idle,
            startup_warning: PlaylistStartupWarningView::None,
        }
    }

    /// Строит UI fixture с подтверждённым active Item ID без controller mutation.
    #[cfg(test)]
    pub(crate) fn for_queue_with_active_item_for_test(
        queue: &playlist_core::PlaylistQueue,
        structural_revision: u64,
        active_item_id: Option<PlaylistItemId>,
    ) -> Self {
        // Test-only boundary использует production snapshot row/index construction.
        Self {
            snapshot: Arc::new(PlaylistViewSnapshot::for_queue_with_active_item_for_test(
                queue,
                structural_revision,
                active_item_id,
            )),
            probe: PlaylistProbeView::Idle,
            save: PlaylistSaveView::Idle,
            navigation: PlaylistNavigationView::Idle,
            startup_warning: PlaylistStartupWarningView::None,
        }
    }

    /// Focused status tests меняют только bounded problem summaries.
    #[cfg(test)]
    pub(crate) fn with_status_for_test(
        mut self,
        probe: PlaylistProbeView,
        save: PlaylistSaveView,
        navigation: PlaylistNavigationView,
        startup_warning: PlaylistStartupWarningView,
    ) -> Self {
        self.probe = probe;
        self.save = save;
        self.navigation = navigation;
        self.startup_warning = startup_warning;
        self
    }

    #[cfg(test)]
    pub(crate) fn revision(&self) -> super::view::PlaylistViewRevision {
        self.snapshot.revision()
    }

    pub(crate) fn structural_revision(&self) -> PlaylistStructuralRevision {
        self.snapshot.structural_revision()
    }

    pub(crate) fn item_count(&self) -> usize {
        self.snapshot.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.snapshot.is_empty()
    }

    /// Возвращает только Item ID уже установленного media instance.
    ///
    /// Pending target и runtime error намеренно не участвуют: UI-анимация
    /// должна реагировать лишь на подтверждённую смену authoritative identity.
    pub(crate) fn active_item_id(&self) -> Option<PlaylistItemId> {
        self.snapshot
            .active_media()
            .and_then(|active_media| active_media.item_id())
    }

    pub(crate) fn visible_rows(&self, range: Range<usize>) -> Vec<PlaylistVisibleRow> {
        self.snapshot.visible_rows(range)
    }

    pub(crate) fn row_index(&self, item_id: PlaylistItemId) -> Option<usize> {
        self.snapshot.row_index(item_id)
    }

    /// Возвращает индекс top-level header по structural identity.
    pub(crate) fn entry_row_index(&self, entry_id: PlaylistEntryId) -> Option<usize> {
        self.snapshot.entry_row_index(entry_id)
    }

    pub(crate) fn item_id_at(&self, row_index: usize) -> Option<PlaylistItemId> {
        self.snapshot.item_id_at(row_index)
    }

    /// Возвращает structural identity строки независимо от её play target.
    pub(crate) fn entry_id_at(&self, row_index: usize) -> Option<PlaylistEntryId> {
        self.snapshot.entry_id_at(row_index)
    }

    /// Возвращает Arc-backed selection read model без копирования selected set.
    pub(crate) fn selection(&self) -> &super::PlaylistSelectionSnapshot {
        self.snapshot.selection()
    }

    /// Собирает selected IDs в canonical порядке только для explicit bulk event.
    pub(crate) fn selected_entry_ids(&self) -> Arc<[PlaylistEntryId]> {
        (0..self.item_count())
            .filter_map(|row_index| self.entry_id_at(row_index))
            .filter(|entry_id| self.selection().is_selected(*entry_id))
            .collect::<Vec<_>>()
            .into()
    }

    /// Разрешает inclusive Shift-range одним bounded canonical slice traversal.
    pub(crate) fn range_entry_ids(
        &self,
        anchor_entry_id: PlaylistEntryId,
        target_entry_id: PlaylistEntryId,
    ) -> Option<Arc<[PlaylistEntryId]>> {
        let anchor_index = self.entry_row_index(anchor_entry_id)?;
        let target_index = self.entry_row_index(target_entry_id)?;
        let start = anchor_index.min(target_index);
        let end = anchor_index.max(target_index);
        Some(
            (start..=end)
                .filter_map(|row_index| self.entry_id_at(row_index))
                .collect::<Vec<_>>()
                .into(),
        )
    }

    pub(crate) const fn probe(&self) -> PlaylistProbeView {
        self.probe
    }

    pub(crate) const fn save(&self) -> PlaylistSaveView {
        self.save
    }

    pub(crate) const fn navigation(&self) -> PlaylistNavigationView {
        self.navigation
    }

    pub(crate) const fn startup_warning(&self) -> PlaylistStartupWarningView {
        self.startup_warning
    }

    #[cfg(test)]
    pub(crate) fn shared_rows_identity(&self) -> usize {
        self.snapshot.shared_rows_identity()
    }

    #[cfg(test)]
    pub(crate) fn shared_title_identity(&self, row_index: usize) -> Option<usize> {
        self.snapshot.shared_title_identity(row_index)
    }
}

impl PlaylistRuntime {
    /// Собирает только O(1)/bounded runtime summaries поверх shared controller snapshot-а.
    pub(crate) fn playlist_view_model(&self) -> PlaylistViewModel {
        let snapshot = self.playlist_view_snapshot();
        let startup = self.playlist_startup_view();
        let persistence = self.playlist_persistence_view();
        let probe = probe_view(self.playlist_discovery_status());
        let save = save_view(persistence);
        let navigation = navigation_view(
            snapshot.navigation_failure_target(),
            self.controller.as_ref().is_some_and(|controller| {
                controller.awaiting_manual_navigation_failure_origin_ended()
            }),
            self.playlist_discovery_navigation_status(),
        );
        let startup_warning = if startup.warning.is_some() {
            PlaylistStartupWarningView::Present
        } else {
            PlaylistStartupWarningView::None
        };
        PlaylistViewModel {
            snapshot,
            probe,
            save,
            navigation,
            startup_warning,
        }
    }
}

fn probe_view(discovery: &PlaylistDiscoveryStatus) -> PlaylistProbeView {
    match discovery {
        PlaylistDiscoveryStatus::TargetOnlyWarning { scope_id, .. } => PlaylistProbeView::Warning {
            scope_id: *scope_id,
        },
        PlaylistDiscoveryStatus::Idle
        | PlaylistDiscoveryStatus::Enumerating { .. }
        | PlaylistDiscoveryStatus::Probing { .. }
        | PlaylistDiscoveryStatus::Completed { .. } => PlaylistProbeView::Idle,
    }
}

fn save_view(persistence: PlaylistPersistenceView) -> PlaylistSaveView {
    if persistence.save_block.is_some() {
        return PlaylistSaveView::Blocked;
    }
    if let Some(fault) = persistence.fault {
        return PlaylistSaveView::Fault(fault);
    }
    if let Some(warning) = persistence.warning {
        return PlaylistSaveView::WarningRetryAvailable {
            attempt: PlaylistSaveAttempt {
                revision: warning.revision,
                occurrence_count: warning.occurrence_count,
            },
        };
    }
    if matches!(
        persistence.durability,
        PlaylistSaveDurability::Pending { .. }
    ) {
        PlaylistSaveView::Saving
    } else {
        PlaylistSaveView::Idle
    }
}

fn navigation_view(
    failed_target: Option<PlaylistItemId>,
    origin_already_ended: bool,
    discovery: PlaylistDiscoveryNavigationStatus,
) -> PlaylistNavigationView {
    if let Some(item_id) = failed_target {
        return PlaylistNavigationView::AwaitingUserAfterFailure {
            item_id,
            origin_already_ended,
        };
    }
    match discovery {
        PlaylistDiscoveryNavigationStatus::Idle
        | PlaylistDiscoveryNavigationStatus::WaitingManual { .. }
        | PlaylistDiscoveryNavigationStatus::WaitingAutomatic { .. }
        | PlaylistDiscoveryNavigationStatus::TargetReady { .. }
        | PlaylistDiscoveryNavigationStatus::Exhausted { .. }
        | PlaylistDiscoveryNavigationStatus::Cancelled { .. } => PlaylistNavigationView::Idle,
        PlaylistDiscoveryNavigationStatus::Fatal { scope_id } => {
            PlaylistNavigationView::Fatal { scope_id }
        }
    }
}
