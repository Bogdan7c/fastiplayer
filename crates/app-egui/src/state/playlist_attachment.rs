//! Renderer-bound read-only attachment к process-lifetime playlist runtime.

use super::AppState;
use crate::playlist_runtime::{
    PlaylistAppStateAttachment, PlaylistRuntimeBinding, PlaylistViewModel,
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
    pub(crate) fn update_playlist_view_model(
        &mut self,
        binding: PlaylistRuntimeBinding,
        view_model: PlaylistViewModel,
    ) -> bool {
        let Some(attachment) = &mut self.playlist_attachment else {
            return false;
        };
        if attachment.binding() != binding {
            return false;
        }
        attachment.replace_view_model(view_model);
        true
    }

    /// Возвращает renderer consumer-у только immutable snapshot текущего attachment-а.
    pub(crate) fn playlist_view_model(&self) -> Option<PlaylistViewModel> {
        self.playlist_attachment
            .as_ref()
            .map(PlaylistAppStateAttachment::view_model)
    }

    /// Сохраняет одноразовый D80 scroll/focus intent до следующего authoritative UI frame.
    pub(crate) fn request_playlist_go_current(
        &mut self,
        target: crate::playlist_runtime::PlaylistGoCurrentTarget,
    ) {
        self.playlist_ui_state.request_go_current(target);
    }
}
