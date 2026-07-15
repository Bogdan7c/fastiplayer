//! Opaque domain token adapter: обычная reservation и manual shuffle preview.

use playlist_core::{
    AutomaticTraversalPlan, PreparedAutomaticTraversalToken, PreparedManualNavigationToken,
    PreparedQueueMutationToken,
};

pub(super) enum GuardedInstallToken {
    Queue(PreparedQueueMutationToken),
    ManualNavigation(PreparedManualNavigationToken),
    AutomaticTraversal(PreparedAutomaticTraversalToken),
}

pub(super) enum GuardedInstallAbort {
    Queue,
    ManualNavigation(playlist_core::ManualNavigationPreview),
    AutomaticTraversal(AutomaticTraversalPlan),
}

pub(super) struct GuardedInstallCommit {
    pub traversal_current: playlist_core::TraversalCurrentItemId,
    pub structural_changed: bool,
    pub manual_navigation: bool,
}

impl GuardedInstallToken {
    pub(super) fn abort(self, queue: &mut playlist_core::PlaylistQueue) -> GuardedInstallAbort {
        match self {
            Self::Queue(token) => {
                queue.abort_reserved(token);
                GuardedInstallAbort::Queue
            }
            Self::ManualNavigation(token) => {
                GuardedInstallAbort::ManualNavigation(queue.abort_manual_navigation(token))
            }
            Self::AutomaticTraversal(token) => {
                GuardedInstallAbort::AutomaticTraversal(queue.abort_automatic_traversal(token))
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
                    manual_navigation: false,
                }
            }
            Self::ManualNavigation(token) => {
                let commit = queue.commit_manual_navigation(token);
                GuardedInstallCommit {
                    traversal_current: commit.traversal_current(),
                    structural_changed: false,
                    manual_navigation: true,
                }
            }
            Self::AutomaticTraversal(token) => {
                let commit = queue.commit_automatic_traversal(token);
                GuardedInstallCommit {
                    traversal_current: commit.traversal_current(),
                    structural_changed: false,
                    manual_navigation: false,
                }
            }
        }
    }
}
