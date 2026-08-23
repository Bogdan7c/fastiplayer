use super::*;

pub(super) fn sync_before_ui(
    app_state: &mut AppState,
    playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
) {
    if let Some(binding) = app_state.playlist_runtime_binding() {
        let playlist_view_model = playlist_runtime.playlist_view_model();
        let _model_was_applied = app_state.update_playlist_view_model(binding, playlist_view_model);
    }
    app_state.sync_web_media_catalog(playlist_runtime);
}

pub(super) fn advance_after_actions(
    app_state: &mut AppState,
    playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
    renderer: &Renderer,
) {
    let _automatic_switch =
        app_state.apply_automatic_web_media_preference(playlist_runtime, renderer);
    app_state.poll_same_item_switch(playlist_runtime);
    app_state.poll_vod_endpoint_recovery(playlist_runtime, renderer);
}
