//! Background catalog composition поверх provider-owned discovery boundaries.

use std::sync::Arc;

use anyhow::{Context, Result};
use service_ytdlp::{YtDlpCandidateSelection, YtDlpCandidateSnapshot, YtDlpLiveIntent};
use web_media_core::{
    ComponentVariantCatalog, ComponentVariantCatalogGeneration, ComponentVariantCatalogIdentity,
    ComponentVariantSelection, ComponentVariantSelectionRequest, ExactSelectionIdentity,
    StreamLayout,
};
use web_media_playback_plan::{
    OpaqueAlternativeRank, PlanningCandidateSnapshot, PlaybackCapabilitySnapshot,
    PlaybackSelectionPolicy,
};

use crate::web_media_catalog::{
    DiscoveredWebMediaCatalog, WebMediaCatalogAttachment, WebMediaCatalogChoice,
    WebMediaCatalogDiscovery, WebMediaMode, WebMediaSelectionTarget,
};

use super::catalog_capabilities::AppCatalogCapabilityProbe;
use super::component_variants::YtDlpComponentSelectionOpenIntent;
use super::{AdaptiveEndpointRefreshPorts, WebCandidateOpenContext, WebOpenRuntime};

pub(crate) struct DiscoveredProviderCatalog {
    pub(crate) catalog: Arc<ComponentVariantCatalog>,
    pub(crate) provider_selection: ComponentVariantSelection,
    pub(crate) rejected_siblings: usize,
}

pub(super) struct CatalogAttachmentRequest {
    pub(super) candidate_snapshot: YtDlpCandidateSnapshot,
    pub(super) planning_snapshot: PlanningCandidateSnapshot,
    pub(super) active_selection: YtDlpCandidateSelection,
    pub(super) active_composed: Option<Box<service_ytdlp::YtDlpComposedSelection>>,
    pub(super) active_component: YtDlpComponentSelectionOpenIntent,
    pub(super) network_config: rustiplayer_config::NetworkConfig,
    pub(super) demux_config: rustiplayer_config::PlayerDemuxConfig,
    pub(super) system_capabilities: capability_core::SystemCapabilities,
    pub(super) audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    pub(super) policy: PlaybackSelectionPolicy,
    pub(super) preferred_height: web_media_core::PreferredHeightPolicy,
    pub(super) live_intent: YtDlpLiveIntent,
    pub(super) locator: service_ytdlp::YtDlpMediaLocator,
    pub(super) yt_dlp_config: rustiplayer_config::YtDlpConfig,
}

struct CatalogDiscoveryJob {
    request: CatalogAttachmentRequest,
}

pub(super) fn catalog_attachment(
    request: CatalogAttachmentRequest,
) -> Result<WebMediaCatalogAttachment> {
    let parent = ExactSelectionIdentity::new(
        request.active_selection.exact_identity().clone(),
        request.active_selection.semantic_identity().clone(),
    )
    .context("catalog attachment parent identity is invalid")?;
    Ok(WebMediaCatalogAttachment::new(
        parent,
        Arc::new(CatalogDiscoveryJob { request }),
    ))
}

impl WebMediaCatalogDiscovery for CatalogDiscoveryJob {
    fn discover(
        &self,
        cancellation: source_core::CancellationToken,
    ) -> Result<DiscoveredWebMediaCatalog> {
        let runtime =
            WebOpenRuntime::new(&self.request.network_config, &self.request.demux_config)?;
        let capabilities = PlaybackCapabilitySnapshot::new(
            &runtime.transport_capabilities,
            &runtime.demux_capabilities,
            &self.request.system_capabilities,
            self.request.audio_capabilities,
        );
        let choices = parent_choices(
            &self.request.candidate_snapshot,
            &self.request.planning_snapshot,
            capabilities,
            &self.request.policy,
        )?;
        let mut active = match &self.request.active_composed {
            Some(selection) => WebMediaSelectionTarget::Composed {
                selection: selection.clone(),
                parent_preference: Box::new(self.request.active_selection.clone()),
            },
            None => WebMediaSelectionTarget::Parent {
                selection: Box::new(self.request.active_selection.clone()),
            },
        };
        let mut rejected_siblings = 0usize;
        let mut capability_probe = AppCatalogCapabilityProbe::new(
            self.request.system_capabilities.clone(),
            self.request.audio_capabilities,
        );
        let mut proven_choices = Vec::with_capacity(choices.len());
        for choice in choices {
            if cancellation.is_cancelled() {
                anyhow::bail!("web-media catalog discovery cancelled");
            }
            let candidate = match &choice.target {
                #[cfg(test)]
                WebMediaSelectionTarget::Fixture(_) => continue,
                WebMediaSelectionTarget::Parent { selection } => self
                    .request
                    .candidate_snapshot
                    .rematch_exact(selection)?
                    .candidate()
                    .clone(),
                WebMediaSelectionTarget::Composed { selection, .. } => {
                    self.request
                        .candidate_snapshot
                        .rematch_composed(selection)?
                        .1
                }
                WebMediaSelectionTarget::Provider { .. } => continue,
            };
            let parent_selection = match &choice.target {
                #[cfg(test)]
                WebMediaSelectionTarget::Fixture(_) => None,
                WebMediaSelectionTarget::Parent { selection } => Some(selection.as_ref()),
                WebMediaSelectionTarget::Composed { .. }
                | WebMediaSelectionTarget::Provider { .. } => None,
            };
            let endpoint_refresh_ports = self.endpoint_refresh_ports(
                &runtime,
                &candidate,
                parent_selection,
                cancellation.clone(),
            );
            let catalog_identity = ComponentVariantCatalogIdentity::new(
                ExactSelectionIdentity::new(
                    candidate.descriptor().identity().clone(),
                    candidate.descriptor().semantic_identity().clone(),
                )?,
                ComponentVariantCatalogGeneration::new(1),
            );
            let opened = runtime.open_candidate(
                &candidate,
                WebCandidateOpenContext {
                    live_intent: self.request.live_intent,
                    endpoint_refresh_ports,
                    timeline_port_generation:
                        super::preparation::next_dynamic_timeline_port_generation()?,
                    component_selection_intent: YtDlpComponentSelectionOpenIntent::ProviderDefault,
                    preferred_height: self.request.preferred_height,
                    catalog_identity,
                    cancellation: cancellation.clone(),
                },
                &|| cancellation.is_cancelled(),
                &mut capability_probe,
            );
            if opened.is_ok() {
                proven_choices.push(choice);
            } else {
                rejected_siblings = rejected_siblings.saturating_add(1);
            }
        }
        let mut choices = proven_choices;
        let active_candidate = self
            .request
            .candidate_snapshot
            .rematch_exact(&self.request.active_selection)?
            .candidate();
        let catalog_identity = ComponentVariantCatalogIdentity::new(
            ExactSelectionIdentity::new(
                self.request.active_selection.exact_identity().clone(),
                self.request.active_selection.semantic_identity().clone(),
            )?,
            ComponentVariantCatalogGeneration::new(1),
        );
        let provider = if crate::web_media_hls_open::candidate_is_hls(active_candidate) {
            crate::web_media_hls_open::discover_hls_candidate_catalog(
                active_candidate,
                runtime.provider_id.clone(),
                &runtime.source_config,
                &runtime.network_config,
                Arc::clone(&runtime.hls_demux_registry),
                cancellation,
                self.request.live_intent,
                catalog_identity,
                &mut capability_probe,
            )?
        } else if super::smooth::candidate_is_smooth(active_candidate) {
            Some(super::smooth::discover_smooth_candidate_catalog(
                active_candidate,
                runtime.provider_id.clone(),
                &runtime.source_config,
                &runtime.network_config,
                Arc::clone(&runtime.demux_registry),
                cancellation,
                self.request.preferred_height,
                catalog_identity,
                &capability_probe,
            )?)
        } else if super::hds::candidate_is_hds(active_candidate) {
            Some(super::hds::discover_hds_candidate_catalog(
                active_candidate,
                runtime.provider_id.clone(),
                &runtime.source_config,
                &runtime.network_config,
                Arc::clone(&runtime.demux_registry),
                cancellation,
                self.request.preferred_height,
                catalog_identity,
                &capability_probe,
            )?)
        } else if crate::web_media_dash_open::candidate_is_dash(active_candidate) {
            let endpoint_refresh: Option<Arc<dyn web_media_dash::DashEndpointRefreshPort>> =
                (self.request.live_intent == YtDlpLiveIntent::Live).then(|| {
                    Arc::new(
                        crate::web_media_dash_refresh::AppDashEndpointRefreshPort::new(
                            self.request.locator.clone(),
                            self.request.yt_dlp_config.clone(),
                            self.request.network_config.clone(),
                            runtime.source_config.clone(),
                            runtime.provider_id.clone(),
                            self.request.active_selection.clone(),
                            cancellation.clone(),
                        ),
                    ) as Arc<dyn web_media_dash::DashEndpointRefreshPort>
                });
            crate::web_media_dash_open::discover_dash_candidate_catalog(
                active_candidate,
                runtime.provider_id.clone(),
                &runtime.source_config,
                &runtime.network_config,
                Arc::clone(&runtime.demux_registry),
                cancellation,
                self.request.live_intent,
                endpoint_refresh,
                super::preparation::next_dynamic_timeline_port_generation()?,
                catalog_identity,
                &capability_probe,
            )?
        } else {
            None
        };
        if let Some(provider) = provider {
            rejected_siblings = rejected_siblings.saturating_add(provider.rejected_siblings);
            let selected = match &self.request.active_component {
                YtDlpComponentSelectionOpenIntent::ProviderDefault => {
                    provider.provider_selection.clone()
                }
                YtDlpComponentSelectionOpenIntent::Semantic(semantic) => {
                    provider.catalog.rematch_semantic(semantic.clone())?
                }
            };
            active = WebMediaSelectionTarget::Provider {
                parent: Box::new(self.request.active_selection.clone()),
                selection: selected.semantic_rematch_request(),
            };
            let parent_rank = choices
                .iter()
                .find_map(|choice| match &choice.target {
                    WebMediaSelectionTarget::Parent { selection }
                        if selection.as_ref() == &self.request.active_selection =>
                    {
                        Some(choice.rank)
                    }
                    _ => None,
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("active provider parent отсутствует в playable ranking")
                })?;
            append_provider_choices(
                &mut choices,
                &provider.catalog,
                &self.request.active_selection,
                parent_rank,
            )?;
        }
        Ok(DiscoveredWebMediaCatalog {
            choices,
            active,
            rejected_siblings,
        })
    }
}

impl CatalogDiscoveryJob {
    fn endpoint_refresh_ports(
        &self,
        runtime: &WebOpenRuntime,
        candidate: &service_ytdlp::YtDlpNormalizedCandidate,
        selection: Option<&YtDlpCandidateSelection>,
        cancellation: source_core::CancellationToken,
    ) -> AdaptiveEndpointRefreshPorts {
        let hls = (self.request.live_intent == YtDlpLiveIntent::Live
            && crate::web_media_hls_open::candidate_is_hls(candidate))
        .then_some(selection)
        .flatten()
        .map(|selection| {
            Arc::new(
                crate::web_media_hls_refresh::AppHlsEndpointRefreshPort::new(
                    self.request.locator.clone(),
                    self.request.yt_dlp_config.clone(),
                    self.request.network_config.clone(),
                    runtime.source_config.clone(),
                    runtime.provider_id.clone(),
                    selection.clone(),
                    cancellation.clone(),
                ),
            ) as Arc<dyn web_media_hls::HlsEndpointRefreshPort>
        });
        let dash = (self.request.live_intent == YtDlpLiveIntent::Live
            && crate::web_media_dash_open::candidate_is_dash(candidate))
        .then_some(selection)
        .flatten()
        .map(|selection| {
            Arc::new(
                crate::web_media_dash_refresh::AppDashEndpointRefreshPort::new(
                    self.request.locator.clone(),
                    self.request.yt_dlp_config.clone(),
                    self.request.network_config.clone(),
                    runtime.source_config.clone(),
                    runtime.provider_id.clone(),
                    selection.clone(),
                    cancellation,
                ),
            ) as Arc<dyn web_media_dash::DashEndpointRefreshPort>
        });
        AdaptiveEndpointRefreshPorts { hls, dash }
    }
}

fn parent_choices(
    snapshot: &YtDlpCandidateSnapshot,
    planning: &PlanningCandidateSnapshot,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    policy: &PlaybackSelectionPolicy,
) -> Result<Vec<WebMediaCatalogChoice>> {
    let ranking =
        web_media_playback_plan::rank_playable_opaque_alternatives(planning, capabilities, policy)?;
    let rejected = ranking
        .rejected_candidates()
        .iter()
        .map(|candidate| candidate.exact_identity())
        .collect::<std::collections::HashSet<_>>();
    let mut choices = Vec::new();
    for candidate in snapshot.accepted_candidates() {
        if rejected.contains(candidate.descriptor().identity()) {
            continue;
        }
        let (mode, video) = layout_facets(candidate.descriptor().layout());
        let selection = snapshot.selection_for(candidate)?;
        let parent_rank = ranking
            .rank_of_candidate(selection.exact_identity(), selection.semantic_identity())
            .ok_or_else(|| {
                anyhow::anyhow!("playable candidate отсутствует в opaque planner ranking")
            })?;
        choices.push(WebMediaCatalogChoice {
            mode,
            video: video.cloned(),
            rank: OpaqueAlternativeRank::parent(parent_rank),
            target: WebMediaSelectionTarget::Parent {
                selection: Box::new(selection),
            },
        });
    }
    let current_audio = None;
    let playable_audio = snapshot
        .accepted_candidates()
        .filter(|candidate| {
            matches!(candidate.descriptor().layout(), StreamLayout::AudioOnly(_))
                && !rejected.contains(candidate.descriptor().identity())
        })
        .collect::<Vec<_>>();
    for video in snapshot.accepted_candidates().filter(|candidate| {
        matches!(candidate.descriptor().layout(), StreamLayout::VideoOnly(_))
            && !rejected.contains(candidate.descriptor().identity())
    }) {
        let Some(audio) = playable_audio.iter().copied().min_by(|left, right| {
            web_media_playback_plan::compare_audio_fallback(
                current_audio,
                left.descriptor().semantic_identity(),
                left.audio_fallback_rank()
                    .expect("audio-only candidate has audio rank"),
                right.descriptor().semantic_identity(),
                right
                    .audio_fallback_rank()
                    .expect("audio-only candidate has audio rank"),
            )
        }) else {
            continue;
        };
        let video_selection = snapshot.selection_for(video)?;
        let audio_selection = snapshot.selection_for(audio)?;
        let parent_rank = ranking
            .rank_of_candidate(
                video_selection.exact_identity(),
                video_selection.semantic_identity(),
            )
            .ok_or_else(|| {
                anyhow::anyhow!("composed video отсутствует в opaque planner ranking")
            })?;
        let composed = snapshot.compose_inventory_av(&video_selection, &audio_selection)?;
        let StreamLayout::Separate {
            video: component, ..
        } = composed.descriptor().layout()
        else {
            continue;
        };
        choices.push(WebMediaCatalogChoice {
            mode: WebMediaMode::VideoAndAudio,
            video: Some(component.video().clone()),
            rank: OpaqueAlternativeRank::parent(parent_rank),
            target: WebMediaSelectionTarget::Composed {
                selection: Box::new(composed),
                parent_preference: Box::new(video_selection),
            },
        });
    }
    Ok(choices)
}

fn append_provider_choices(
    choices: &mut Vec<WebMediaCatalogChoice>,
    catalog: &ComponentVariantCatalog,
    parent: &YtDlpCandidateSelection,
    parent_rank: OpaqueAlternativeRank,
) -> Result<()> {
    let mut canonical_provider_rank = 0usize;
    visit_provider_selections(catalog, |mode, video, selection| {
        push_provider_choice(
            choices,
            parent,
            mode,
            video,
            selection,
            OpaqueAlternativeRank::provider(
                parent_rank.parent_playable_rank(),
                canonical_provider_rank,
            ),
        );
        canonical_provider_rank = canonical_provider_rank
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("provider rank space исчерпан"))?;
        Ok(())
    })
}

fn visit_provider_selections(
    catalog: &ComponentVariantCatalog,
    mut visitor: impl FnMut(
        WebMediaMode,
        Option<web_media_core::VideoTrackDescriptor>,
        ComponentVariantSelection,
    ) -> Result<()>,
) -> Result<()> {
    for presentation in catalog.coupled_presentations() {
        let selection = catalog.select_exact(ComponentVariantSelectionRequest::Coupled {
            presentation: presentation.exact_identity().clone(),
        })?;
        visitor(
            WebMediaMode::VideoAndAudio,
            Some(presentation.video().clone()),
            selection,
        )?;
    }
    let (videos, audios): (&[_], &[_]) = match catalog {
        ComponentVariantCatalog::Topology { video, audio, .. }
        | ComponentVariantCatalog::VideoAndAudio { video, audio, .. } => (video, audio),
        ComponentVariantCatalog::VideoOnly { video, .. } => (video, &[]),
        ComponentVariantCatalog::AudioOnly { audio, .. } => (&[], audio),
    };
    for video in videos {
        for audio in audios.iter().filter(|audio| {
            catalog.compatibility().is_none_or(|compatibility| {
                compatibility.allows(video.exact_identity(), audio.exact_identity())
            })
        }) {
            let selection =
                catalog.select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
                    video: video.exact_identity().clone(),
                    audio: audio.exact_identity().clone(),
                })?;
            visitor(
                WebMediaMode::VideoAndAudio,
                Some(video.track().clone()),
                selection,
            )?;
        }
        if catalog.is_video_only_selectable(video.exact_identity()) {
            let selection = catalog.select_exact(ComponentVariantSelectionRequest::VideoOnly {
                video: video.exact_identity().clone(),
            })?;
            visitor(
                WebMediaMode::VideoOnly,
                Some(video.track().clone()),
                selection,
            )?;
        }
    }
    for audio in audios {
        if catalog.is_audio_only_selectable(audio.exact_identity()) {
            let selection = catalog.select_exact(ComponentVariantSelectionRequest::AudioOnly {
                audio: audio.exact_identity().clone(),
            })?;
            visitor(WebMediaMode::AudioOnly, None, selection)?;
        }
    }
    Ok(())
}

fn push_provider_choice(
    choices: &mut Vec<WebMediaCatalogChoice>,
    parent: &YtDlpCandidateSelection,
    mode: WebMediaMode,
    video: Option<web_media_core::VideoTrackDescriptor>,
    selection: ComponentVariantSelection,
    rank: OpaqueAlternativeRank,
) {
    choices.push(WebMediaCatalogChoice {
        mode,
        video,
        rank,
        target: WebMediaSelectionTarget::Provider {
            parent: Box::new(parent.clone()),
            selection: selection.semantic_rematch_request(),
        },
    });
}

fn layout_facets(
    layout: &StreamLayout,
) -> (WebMediaMode, Option<&web_media_core::VideoTrackDescriptor>) {
    match layout {
        StreamLayout::Muxed(component) => (WebMediaMode::VideoAndAudio, Some(component.video())),
        StreamLayout::Separate { video, .. } => (WebMediaMode::VideoAndAudio, Some(video.video())),
        StreamLayout::VideoOnly(video) => (WebMediaMode::VideoOnly, Some(video.video())),
        StreamLayout::AudioOnly(_) => (WebMediaMode::AudioOnly, None),
    }
}

#[cfg(test)]
mod tests;
