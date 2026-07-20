//! Cached pure CUE export availability поверх immutable controller snapshot-а.

use playlist_io::{
    CueExportScopeIneligibility, PlaylistExportAvailability, PlaylistExportScope,
    PlaylistExportSnapshot, cue_export_scope_availability,
};

use crate::playlist_runtime::PlaylistRuntime;

/// Cached availability keyed by общей presentation revision queue и selection.
#[derive(Clone, Copy)]
pub(in crate::playlist_runtime) struct CueExportAvailabilityCache {
    view_revision: crate::playlist_runtime::view::PlaylistViewRevision,
    full: PlaylistExportAvailability,
    selected: PlaylistExportAvailability,
}

impl PlaylistRuntime {
    /// Возвращает CUE availability без повторного O(N) scan на каждом renderer frame.
    pub(in crate::playlist_runtime) fn cue_export_availabilities(
        &self,
    ) -> (PlaylistExportAvailability, PlaylistExportAvailability) {
        let unavailable =
            || PlaylistExportAvailability::Disabled(CueExportScopeIneligibility::EmptyScope);
        let Some(controller) = self.controller.as_ref() else {
            return (unavailable(), unavailable());
        };
        let view = controller.view_snapshot();
        if let Some(cache) = self
            .cue_export_availability_cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.view_revision == view.revision())
        {
            return (cache.full, cache.selected);
        }
        let queue = controller.queue();
        let full = PlaylistExportSnapshot::capture(queue, PlaylistExportScope::Full).map_or_else(
            |_| unavailable(),
            |snapshot| cue_export_scope_availability(&snapshot),
        );
        let selected_entry_ids = super::selected_export_entry_ids(controller);
        let selected = if selected_entry_ids.is_empty() {
            unavailable()
        } else {
            PlaylistExportSnapshot::capture(
                queue,
                PlaylistExportScope::Selected(&selected_entry_ids),
            )
            .map_or_else(
                |_| unavailable(),
                |snapshot| cue_export_scope_availability(&snapshot),
            )
        };
        *self.cue_export_availability_cache.borrow_mut() = Some(CueExportAvailabilityCache {
            view_revision: view.revision(),
            full,
            selected,
        });
        (full, selected)
    }
}
