//! Neutral same-item/settings actions поверх закрытого physical adapter state.

use super::*;

/// Provider-neutral selection intent, который same-item lifecycle передаёт source owner-у.
pub(crate) enum WebMediaSelectionSwitchIntent {
    CatalogTarget(crate::web_media_catalog::WebMediaSelectionTarget),
    ComponentSemantic(web_media_core::ComponentVariantSemanticSelectionRequest),
}

/// Source-owned resolution без provider checks в UI/lifecycle.
pub(crate) enum WebMediaSelectionSwitchResolution {
    NoChange,
    Ready(WebMediaOpenRequest),
    Unsupported,
    Stale,
}

/// Settings-owned выбор поведения только для stable direct resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectResourceSettingsAction {
    KeepInstalled,
    Rebuild,
}

/// Settings-owned selection policy extractor adapter-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebMediaSettingsSelectionPolicy {
    PreserveInstalled,
    ReselectBestPlayable,
}

/// Named settings policy не заставляет caller передавать неочевидные bool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WebMediaSettingsReconfigurePolicy {
    pub(crate) direct_resource: DirectResourceSettingsAction,
    pub(crate) selection: WebMediaSettingsSelectionPolicy,
}

/// Settings route либо inert, либо получает neutral request.
pub(crate) enum WebMediaSettingsReconfigureDecision {
    NoChange,
    Reopen(WebMediaOpenRequest),
}

impl WebMediaSourceIntent {
    /// Сообщает settings owner-у, нужен ли lifecycle transaction вообще.
    pub(crate) fn requires_settings_reconfigure(
        &self,
        policy: WebMediaSettingsReconfigurePolicy,
    ) -> bool {
        match &*self.adapter {
            WebMediaSourceAdapter::Direct { .. } => {
                policy.direct_resource == DirectResourceSettingsAction::Rebuild
            }
            WebMediaSourceAdapter::NativeHls { .. } | WebMediaSourceAdapter::Extractor { .. } => {
                true
            }
        }
    }

    /// Разрешает same-item action внутри owner-а physical extractor state.
    pub(crate) fn selection_switch_request(
        &self,
        intent: WebMediaSelectionSwitchIntent,
        settings: WebMediaOpenSettings,
    ) -> WebMediaSelectionSwitchResolution {
        let WebMediaSourceAdapter::Extractor {
            locator,
            source_state,
        } = &*self.adapter
        else {
            return match intent {
                WebMediaSelectionSwitchIntent::CatalogTarget(
                    crate::web_media_catalog::WebMediaSelectionTarget::InstalledOnly,
                ) => WebMediaSelectionSwitchResolution::NoChange,
                WebMediaSelectionSwitchIntent::CatalogTarget(_)
                | WebMediaSelectionSwitchIntent::ComponentSemantic(_) => {
                    WebMediaSelectionSwitchResolution::Unsupported
                }
            };
        };

        let selection_intent = match intent {
            WebMediaSelectionSwitchIntent::CatalogTarget(
                crate::web_media_catalog::WebMediaSelectionTarget::InstalledOnly,
            ) => return WebMediaSelectionSwitchResolution::NoChange,
            WebMediaSelectionSwitchIntent::CatalogTarget(target) => {
                let Some(selection_intent) = source_state.selection_intent_for_target(&target)
                else {
                    return WebMediaSelectionSwitchResolution::Stale;
                };
                selection_intent
            }
            WebMediaSelectionSwitchIntent::ComponentSemantic(selection) => {
                source_state.selection_intent_for_component(selection)
            }
        };

        WebMediaSelectionSwitchResolution::Ready(WebMediaOpenRequest::extractor(
            locator.clone(),
            selection_intent,
            settings,
        ))
    }

    /// Проецирует settings change в controlled reopen без adapter dispatch у caller-а.
    pub(crate) fn settings_reconfigure_request(
        &self,
        policy: WebMediaSettingsReconfigurePolicy,
        network_config: rustiplayer_config::NetworkConfig,
        demux_config: rustiplayer_config::PlayerDemuxConfig,
        adaptive_settings: WebMediaOpenSettings,
    ) -> WebMediaSettingsReconfigureDecision {
        let request = match &*self.adapter {
            WebMediaSourceAdapter::Direct { locator } => {
                if policy.direct_resource == DirectResourceSettingsAction::KeepInstalled {
                    return WebMediaSettingsReconfigureDecision::NoChange;
                }
                WebMediaOpenRequest::direct(locator.clone(), network_config, demux_config)
            }
            WebMediaSourceAdapter::NativeHls { source, selection } => {
                WebMediaOpenRequest::native_hls(
                    source.clone(),
                    NativeHlsOpenIntent::ExactSelection(selection.clone()),
                    adaptive_settings,
                )
            }
            WebMediaSourceAdapter::Extractor {
                locator,
                source_state,
            } => {
                let selection_intent = match policy.selection {
                    WebMediaSettingsSelectionPolicy::PreserveInstalled => {
                        source_state.installed_reopen_intent()
                    }
                    WebMediaSettingsSelectionPolicy::ReselectBestPlayable => {
                        crate::web_media_open::YtDlpCandidateOpenIntent::BestPlayable
                    }
                };
                WebMediaOpenRequest::extractor(locator.clone(), selection_intent, adaptive_settings)
            }
        };
        WebMediaSettingsReconfigureDecision::Reopen(request)
    }
}
