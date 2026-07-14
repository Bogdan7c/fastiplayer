//! Opaque domain token adapter: обычная reservation и manual shuffle preview.

use playlist_core::{PreparedManualNavigationToken, PreparedQueueMutationToken};

pub(super) enum GuardedInstallToken {
    Queue(PreparedQueueMutationToken),
    ManualNavigation(PreparedManualNavigationToken),
}

pub(super) struct GuardedInstallCommit {
    pub traversal_current: playlist_core::TraversalCurrentItemId,
    pub structural_changed: bool,
}

impl GuardedInstallToken {
    pub(super) fn abort(self, queue: &mut playlist_core::PlaylistQueue) {
        match self {
            Self::Queue(token) => queue.abort_reserved(token),
            Self::ManualNavigation(token) => {
                let _discarded_preview = queue.abort_manual_navigation(token);
            }
        }
    }

    pub(super) fn commit(self, queue: &mut playlist_core::PlaylistQueue) -> GuardedInstallCommit {
        match self {
            Self::Queue(token) => {
                let commit = queue.commit_reserved(token);
                GuardedInstallCommit {
                    traversal_current: commit.traversal_current(),
                    structural_changed: !commit.allocated_item_ids().as_slice().is_empty(),
                }
            }
            Self::ManualNavigation(token) => {
                let commit = queue.commit_manual_navigation(token);
                GuardedInstallCommit {
                    traversal_current: commit.traversal_current(),
                    structural_changed: false,
                }
            }
        }
    }
}
