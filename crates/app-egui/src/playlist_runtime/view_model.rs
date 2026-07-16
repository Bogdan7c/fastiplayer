//! Runtime-owned immutable model для read-only Playlist sidebar.

use std::ops::Range;
use std::sync::Arc;

use playlist_core::PlaylistItemId;

use super::PlaylistRuntime;
use super::discovery::{PlaylistDiscoveryNavigationStatus, PlaylistDiscoveryStatus};
use super::persistence::{
    PlaylistPersistenceFault, PlaylistPersistenceView, PlaylistSaveDurability,
};
use super::startup::{PlaylistStartupPhase, PlaylistStartupView};
use super::view::{PlaylistStructuralRevision, PlaylistViewSnapshot, PlaylistVisibleRow};

/// Startup gate визуально не смешивается с доказанно пустой очередью.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistLoadingView {
    Loading,
    Ready,
}

/// Bounded probe/discovery summary уже не содержит filesystem locator-ов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistProbeView {
    Idle,
    Enumerating,
    Probing { processed: usize, total: usize },
    ManualProbe { processed: usize, total: usize },
    Completed,
    Warning,
}

/// Persistence summary сохраняет различие block, retryable warning и terminal fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistSaveView {
    Idle,
    Saving,
    WarningRetryAvailable { occurrence_count: u32 },
    Blocked,
    Fault(PlaylistPersistenceFault),
}

/// Navigation UI не получает cursor/preview ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaylistNavigationView {
    Idle,
    WaitingForCandidate,
    AwaitingUserAfterFailure,
    Exhausted,
    Cancelled,
    Fatal,
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
    loading: PlaylistLoadingView,
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
        loading: PlaylistLoadingView,
    ) -> Self {
        Self {
            snapshot: Arc::new(PlaylistViewSnapshot::for_queue_with_revision(
                queue,
                structural_revision,
            )),
            loading,
            probe: PlaylistProbeView::Idle,
            save: PlaylistSaveView::Idle,
            navigation: PlaylistNavigationView::Idle,
            startup_warning: PlaylistStartupWarningView::None,
        }
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

    pub(crate) fn visible_rows(&self, range: Range<usize>) -> Vec<PlaylistVisibleRow> {
        self.snapshot.visible_rows(range)
    }

    pub(crate) fn row_index(&self, item_id: PlaylistItemId) -> Option<usize> {
        self.snapshot.row_index(item_id)
    }

    pub(crate) fn item_id_at(&self, row_index: usize) -> Option<PlaylistItemId> {
        self.snapshot.item_id_at(row_index)
    }

    pub(crate) fn structural_actions_enabled(&self) -> bool {
        self.snapshot.structural_actions_enabled()
    }

    pub(crate) fn has_active_tombstone(&self) -> bool {
        self.snapshot.has_active_tombstone()
    }

    pub(crate) const fn loading(&self) -> PlaylistLoadingView {
        self.loading
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
        let loading = loading_view(startup);
        let probe = probe_view(
            self.playlist_discovery_status(),
            self.discovery.manual_probe_progress(),
        );
        let save = save_view(persistence);
        let navigation = navigation_view(
            snapshot.awaiting_user_after_navigation_failure(),
            self.playlist_discovery_navigation_status(),
        );
        let startup_warning = if startup.warning.is_some() {
            PlaylistStartupWarningView::Present
        } else {
            PlaylistStartupWarningView::None
        };
        PlaylistViewModel {
            snapshot,
            loading,
            probe,
            save,
            navigation,
            startup_warning,
        }
    }
}

fn loading_view(startup: PlaylistStartupView) -> PlaylistLoadingView {
    match startup.phase {
        PlaylistStartupPhase::PendingLoadDecision
        | PlaylistStartupPhase::Inspecting
        | PlaylistStartupPhase::ApplyingQuarantine => PlaylistLoadingView::Loading,
        PlaylistStartupPhase::Ready | PlaylistStartupPhase::Shutdown => PlaylistLoadingView::Ready,
    }
}

fn probe_view(
    discovery: &PlaylistDiscoveryStatus,
    manual_progress: Option<playlist_discovery::DiscoveryProgress>,
) -> PlaylistProbeView {
    if let Some(progress) = manual_progress {
        return PlaylistProbeView::ManualProbe {
            processed: progress.processed,
            total: progress.total,
        };
    }
    match discovery {
        PlaylistDiscoveryStatus::Idle => PlaylistProbeView::Idle,
        PlaylistDiscoveryStatus::Enumerating { .. } => PlaylistProbeView::Enumerating,
        PlaylistDiscoveryStatus::Probing {
            processed, total, ..
        } => PlaylistProbeView::Probing {
            processed: *processed,
            total: *total,
        },
        PlaylistDiscoveryStatus::Completed { .. } => PlaylistProbeView::Completed,
        PlaylistDiscoveryStatus::TargetOnlyWarning { .. } => PlaylistProbeView::Warning,
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
            occurrence_count: warning.occurrence_count,
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
    awaiting_user_after_failure: bool,
    discovery: PlaylistDiscoveryNavigationStatus,
) -> PlaylistNavigationView {
    if awaiting_user_after_failure {
        return PlaylistNavigationView::AwaitingUserAfterFailure;
    }
    match discovery {
        PlaylistDiscoveryNavigationStatus::Idle
        | PlaylistDiscoveryNavigationStatus::TargetReady { .. } => PlaylistNavigationView::Idle,
        PlaylistDiscoveryNavigationStatus::WaitingManual { .. }
        | PlaylistDiscoveryNavigationStatus::WaitingAutomatic { .. } => {
            PlaylistNavigationView::WaitingForCandidate
        }
        PlaylistDiscoveryNavigationStatus::Exhausted { .. } => PlaylistNavigationView::Exhausted,
        PlaylistDiscoveryNavigationStatus::Cancelled { .. } => PlaylistNavigationView::Cancelled,
        PlaylistDiscoveryNavigationStatus::Fatal { .. } => PlaylistNavigationView::Fatal,
    }
}
