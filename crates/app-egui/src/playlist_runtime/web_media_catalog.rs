use std::collections::HashMap;
use std::time::Instant;

use playlist_core::PlaylistItemId;

use super::{PlaylistController, PlaylistRuntime};
use crate::app_wake::AppWakePort;
use crate::process_shutdown::ProcessOwnerShutdownOutcome;
use crate::web_media_catalog::{
    WebMediaCatalogAttachment, WebMediaCatalogCoordinator, WebMediaCatalogCorrelation,
    WebMediaCatalogState, WebMediaRememberedPreference,
};

pub(super) struct PlaylistWebMediaCatalogOwner {
    coordinator: WebMediaCatalogCoordinator,
    preferences: HashMap<PlaylistItemId, WebMediaRememberedPreference>,
}

impl PlaylistWebMediaCatalogOwner {
    pub(super) fn new(wake_port: AppWakePort) -> Self {
        Self {
            coordinator: WebMediaCatalogCoordinator::new(wake_port),
            preferences: HashMap::new(),
        }
    }

    pub(super) fn drain(&mut self) -> bool {
        self.coordinator.drain()
    }

    pub(super) fn shutdown_until(&mut self, deadline: Instant) -> ProcessOwnerShutdownOutcome {
        let report = self.coordinator.shutdown_until(deadline);
        if report.detached_deadline_workers > 0 {
            ProcessOwnerShutdownOutcome::TimedOut {
                pending_threads: report.detached_deadline_workers,
            }
        } else if report.panicked_workers > 0 {
            ProcessOwnerShutdownOutcome::ThreadPanicked {
                panicked_threads: report.panicked_workers,
                pending_threads: 0,
            }
        } else {
            ProcessOwnerShutdownOutcome::Completed
        }
    }

    fn prune_preferences(&mut self, controller: Option<&PlaylistController>) {
        let Some(controller) = controller else {
            self.preferences.clear();
            return;
        };
        self.preferences
            .retain(|item_id, _| controller.queue().item(*item_id).is_some());
    }
}

impl PlaylistRuntime {
    pub(crate) fn ensure_web_media_catalog(
        &mut self,
        correlation: WebMediaCatalogCorrelation,
        attachment: WebMediaCatalogAttachment,
    ) {
        self.web_media_catalog
            .coordinator
            .ensure(correlation, attachment);
    }

    pub(crate) fn clear_web_media_catalog(&mut self) {
        self.web_media_catalog.coordinator.clear();
    }

    pub(crate) fn web_media_catalog_state(&mut self) -> WebMediaCatalogState {
        self.web_media_catalog
            .prune_preferences(self.controller.as_ref());
        self.web_media_catalog.coordinator.state()
    }

    pub(crate) fn remembered_web_media_preference(
        &mut self,
        item_id: PlaylistItemId,
    ) -> Option<WebMediaRememberedPreference> {
        self.web_media_catalog
            .prune_preferences(self.controller.as_ref());
        self.web_media_catalog.preferences.get(&item_id).cloned()
    }

    pub(crate) fn remember_web_media_preference(
        &mut self,
        item_id: PlaylistItemId,
        preference: WebMediaRememberedPreference,
    ) {
        self.web_media_catalog
            .prune_preferences(self.controller.as_ref());
        if self
            .controller
            .as_ref()
            .is_some_and(|controller| controller.queue().item(item_id).is_some())
        {
            self.web_media_catalog
                .preferences
                .insert(item_id, preference);
        }
    }
}

pub(super) const fn requires_process_exit(outcome: ProcessOwnerShutdownOutcome) -> bool {
    matches!(
        outcome,
        ProcessOwnerShutdownOutcome::TimedOut { .. }
            | ProcessOwnerShutdownOutcome::ThreadPanicked {
                pending_threads: 1..,
                ..
            }
    )
}

pub(super) const fn has_terminal_failure(outcome: ProcessOwnerShutdownOutcome) -> bool {
    matches!(outcome, ProcessOwnerShutdownOutcome::ThreadPanicked { .. })
}
