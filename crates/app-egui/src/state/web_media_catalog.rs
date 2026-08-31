use crate::playlist_runtime::PlaylistRuntime;
use crate::web_media_catalog::{
    WebMediaCatalogCorrelation, WebMediaCatalogScope, WebMediaCatalogState, WebMediaSelectionTarget,
};

use super::AppState;

#[derive(Clone)]
pub(super) struct PendingAutomaticWebMediaSwitch {
    pub(super) parent_generation: crate::web_media_stream_model::WebMediaStreamGeneration,
    pub(super) catalog_generation: u64,
    pub(super) target: WebMediaSelectionTarget,
}

impl AppState {
    pub(super) fn url_sidebar_model(
        &self,
        player_snapshot: &player_core::PlayerSnapshot,
        playlist_view_model: Option<&crate::playlist_runtime::PlaylistViewModel>,
    ) -> crate::web_media_stream_model::UrlSidebarModel {
        self.url_sidebar_controller.model_with_catalog(
            self.active_media_source.as_ref(),
            player_snapshot,
            playlist_view_model,
            self.web_media_catalog_state.clone(),
            self.web_media_fallback_notice,
        )
    }

    pub(crate) fn sync_web_media_catalog(&mut self, playlist_runtime: &mut PlaylistRuntime) {
        let Some(source) = self.active_media_source.as_ref() else {
            playlist_runtime.clear_web_media_catalog();
            self.web_media_catalog_state = WebMediaCatalogState::Inactive;
            self.pending_automatic_web_media_switch = None;
            self.web_media_fallback_notice = false;
            return;
        };
        let Some(web_intent) = source.web_intent() else {
            playlist_runtime.clear_web_media_catalog();
            self.web_media_catalog_state = WebMediaCatalogState::Inactive;
            self.pending_automatic_web_media_switch = None;
            self.web_media_fallback_notice = false;
            return;
        };
        let projection = web_intent.read_only_projection();
        let stream_configuration = projection.stream_configuration;
        let catalog_attachment = web_intent
            .catalog_attachment()
            .cloned()
            .unwrap_or_else(crate::web_media_catalog::WebMediaCatalogAttachment::installed_only);
        let Some(active) = playlist_runtime.playlist_view_snapshot().active_media() else {
            playlist_runtime.clear_web_media_catalog();
            self.web_media_catalog_state = WebMediaCatalogState::Inactive;
            self.pending_automatic_web_media_switch = None;
            self.web_media_fallback_notice = false;
            return;
        };
        let Some(binding) = playlist_runtime.current_binding() else {
            self.pending_automatic_web_media_switch = None;
            return;
        };
        if self.last_player_snapshot.media_instance_id != Some(active.media_instance_id()) {
            self.pending_automatic_web_media_switch = None;
            return;
        }
        let scope = active
            .item_id()
            .map(WebMediaCatalogScope::Item)
            .unwrap_or(WebMediaCatalogScope::Detached);
        playlist_runtime.ensure_web_media_catalog(
            WebMediaCatalogCorrelation {
                scope,
                parent: catalog_attachment.parent().cloned(),
                media_instance: active.media_instance_id(),
                binding,
                parent_generation: stream_configuration
                    .map(crate::web_media_stream_model::WebMediaStreamConfiguration::generation),
            },
            catalog_attachment,
        );
        self.web_media_catalog_state = playlist_runtime.web_media_catalog_state();
        if stream_configuration.is_none() {
            self.pending_automatic_web_media_switch = None;
            self.web_media_fallback_notice = false;
            return;
        }
        let WebMediaCatalogScope::Item(item_id) = scope else {
            self.pending_automatic_web_media_switch = None;
            self.web_media_fallback_notice = false;
            return;
        };
        let crate::web_media_catalog::WebMediaCatalogState::Ready(catalog) =
            &self.web_media_catalog_state
        else {
            self.pending_automatic_web_media_switch = None;
            self.web_media_fallback_notice = false;
            return;
        };
        let Some(preference) = playlist_runtime.remembered_web_media_preference(item_id) else {
            self.pending_automatic_web_media_switch = None;
            self.web_media_fallback_notice = false;
            return;
        };
        match catalog.rematch_preference(&preference) {
            Some(target) if target != &catalog.active_choice().target => {
                if self.same_item_switch.is_none() {
                    self.pending_automatic_web_media_switch =
                        catalog.parent_generation().map(|parent_generation| {
                            PendingAutomaticWebMediaSwitch {
                                parent_generation,
                                catalog_generation: catalog.generation(),
                                target: target.clone(),
                            }
                        });
                } else {
                    self.pending_automatic_web_media_switch = None;
                }
            }
            Some(_) => {
                self.pending_automatic_web_media_switch = None;
            }
            None => {
                self.pending_automatic_web_media_switch = None;
                // Уже установленный yt-dlp choice прошёл exact Installed и является допустимым
                // fallback commit point без второй бессмысленной same-item транзакции.
                if let Some(preference) = catalog.active_choice().target.remembered() {
                    playlist_runtime.remember_web_media_preference(item_id, preference);
                    self.web_media_fallback_notice = true;
                }
            }
        }
    }

    pub(crate) fn apply_automatic_web_media_preference(
        &mut self,
        playlist_runtime: &mut PlaylistRuntime,
        renderer: &render_wgpu_shell::Renderer,
    ) -> bool {
        let Some(pending) = self.pending_automatic_web_media_switch.take() else {
            return false;
        };
        match self.start_automatic_web_media_switch(pending.clone(), playlist_runtime, renderer) {
            Ok(_) => true,
            Err(crate::state::same_item_candidate_switch::SameItemSwitchError::Busy) => {
                self.pending_automatic_web_media_switch = Some(pending);
                false
            }
            Err(error) => {
                tracing::warn!(error = %error, "Automatic remembered stream switch rejected");
                false
            }
        }
    }
}
