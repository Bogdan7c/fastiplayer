use std::time::Instant;

use crate::playlist_runtime::PlaylistRuntime;
use crate::web_media_catalog::{
    WebMediaAutomaticQualityDirection, WebMediaCatalogCorrelation, WebMediaCatalogScope,
    WebMediaCatalogState, WebMediaSelectionTarget,
};
use crate::web_media_stream_model::WebMediaSelectionPreference;

use super::AppState;
use super::automatic_web_media_quality::{
    AutomaticWebMediaQualityDecision, AutomaticWebMediaQualityObservation,
};

/// Причина автоматического switch-а определяет, можно ли сохранять target как user preference.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AutomaticWebMediaSwitchPurpose {
    /// Восстановление уже сохранённого пользователем выбора обязано оставить preference.
    RememberedPreference,
    /// Runtime adaptation не имеет права превращаться в ручной item override.
    AdaptiveQuality,
}

#[derive(Clone)]
pub(super) struct PendingAutomaticWebMediaSwitch {
    pub(super) parent_generation: crate::web_media_stream_model::WebMediaStreamGeneration,
    pub(super) catalog_generation: u64,
    pub(super) target: WebMediaSelectionTarget,
    pub(super) purpose: AutomaticWebMediaSwitchPurpose,
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
            self.automatic_web_media_quality.reset();
            self.web_media_fallback_notice = false;
            return;
        };
        let Some(web_intent) = source.web_intent() else {
            playlist_runtime.clear_web_media_catalog();
            self.web_media_catalog_state = WebMediaCatalogState::Inactive;
            self.pending_automatic_web_media_switch = None;
            self.automatic_web_media_quality.reset();
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
            self.automatic_web_media_quality.reset();
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
        let Some(stream_configuration) = stream_configuration else {
            self.pending_automatic_web_media_switch = None;
            self.automatic_web_media_quality.reset();
            self.web_media_fallback_notice = false;
            return;
        };
        let WebMediaCatalogScope::Item(item_id) = scope else {
            self.pending_automatic_web_media_switch = None;
            self.automatic_web_media_quality.reset();
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
        let catalog = std::sync::Arc::clone(catalog);
        let remembered_preference = playlist_runtime.remembered_web_media_preference(item_id);
        if stream_configuration.preference() == WebMediaSelectionPreference::GlobalBestPlayable
            && remembered_preference.is_none()
        {
            self.web_media_fallback_notice = false;
            if self
                .pending_automatic_web_media_switch
                .as_ref()
                .is_some_and(|pending| {
                    pending.purpose == AutomaticWebMediaSwitchPurpose::AdaptiveQuality
                })
            {
                return;
            }
            if self.same_item_switch.is_none() {
                self.pending_automatic_web_media_switch =
                    self.automatic_quality_switch_for(item_id, catalog.as_ref(), Instant::now());
            }
            return;
        }
        self.automatic_web_media_quality.reset();
        let Some(preference) = remembered_preference else {
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
                                purpose: AutomaticWebMediaSwitchPurpose::RememberedPreference,
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

    /// Преобразует player evidence в exact adjacent catalog target без fake combination logic.
    fn automatic_quality_switch_for(
        &mut self,
        item_id: playlist_core::PlaylistItemId,
        catalog: &crate::web_media_catalog::WebMediaCatalog,
        now: Instant,
    ) -> Option<PendingAutomaticWebMediaSwitch> {
        let active_height = catalog.active_choice().video.as_ref()?.height()?.pixels();
        let lower = catalog.automatic_quality_target(WebMediaAutomaticQualityDirection::Lower);
        let higher = catalog.automatic_quality_target(WebMediaAutomaticQualityDirection::Higher);
        let media_instance_id = self.last_player_snapshot.media_instance_id?;
        let observation = AutomaticWebMediaQualityObservation::from_snapshot(
            item_id,
            media_instance_id,
            &self.last_player_snapshot,
            active_height,
            lower.is_some(),
            higher.as_ref().map(|target| target.height),
        );
        let selected = match self.automatic_web_media_quality.observe(observation, now)? {
            AutomaticWebMediaQualityDecision::Lower => lower?,
            AutomaticWebMediaQualityDecision::Higher => higher?,
        };
        Some(PendingAutomaticWebMediaSwitch {
            parent_generation: catalog.parent_generation()?,
            catalog_generation: catalog.generation(),
            target: selected.target,
            purpose: AutomaticWebMediaSwitchPurpose::AdaptiveQuality,
        })
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
                tracing::warn!(error = %error, "Automatic web-media stream switch rejected");
                false
            }
        }
    }
}
