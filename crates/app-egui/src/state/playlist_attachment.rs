//! Renderer-bound read-only attachment к process-lifetime playlist runtime.

use std::sync::Arc;

use super::AppState;
use crate::playlist_runtime::{
    PlaylistAppStateAttachment, PlaylistRuntimeBinding, PlaylistViewSnapshot,
};

impl AppState {
    /// Присоединяет exact runtime port identity и immutable view одним согласованным значением.
    pub(crate) fn attach_playlist_runtime(&mut self, attachment: PlaylistAppStateAttachment) {
        self.playlist_attachment = Some(attachment);
    }

    /// Exact process-runtime binding текущего renderer-bound AppState.
    pub(crate) fn playlist_runtime_binding(
        &self,
    ) -> Option<crate::playlist_runtime::PlaylistRuntimeBinding> {
        self.playlist_attachment
            .as_ref()
            .map(PlaylistAppStateAttachment::binding)
    }

    /// Обновляет только read-only view; mutable controller остаётся в `PlaylistRuntime`.
    #[allow(
        dead_code,
        reason = "playlist UI starts consuming refreshed snapshots in Session 12"
    )]
    pub(crate) fn update_playlist_view_snapshot(
        &mut self,
        binding: PlaylistRuntimeBinding,
        view_snapshot: Arc<PlaylistViewSnapshot>,
    ) -> bool {
        let Some(attachment) = &mut self.playlist_attachment else {
            return false;
        };
        if attachment.binding() != binding {
            return false;
        }
        attachment.replace_view_snapshot(view_snapshot);
        true
    }

    /// Возвращает renderer consumer-у только immutable snapshot текущего attachment-а.
    #[allow(
        dead_code,
        reason = "playlist UI starts reading the attached snapshot in Session 12"
    )]
    pub(crate) fn playlist_view_snapshot(&self) -> Option<Arc<PlaylistViewSnapshot>> {
        self.playlist_attachment
            .as_ref()
            .map(PlaylistAppStateAttachment::view_snapshot)
    }
}
