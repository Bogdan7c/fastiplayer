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
            WebMediaSourceAdapter::NativeHls { .. }
            | WebMediaSourceAdapter::NativeDash { .. }
            | WebMediaSourceAdapter::NativeHds { .. }
            | WebMediaSourceAdapter::NativeSmooth { .. }
            | WebMediaSourceAdapter::Extractor { .. } => true,
        }
    }

    /// Разрешает same-item action внутри owner-а physical adapter state.
    pub(crate) fn selection_switch_request(
        &self,
        intent: WebMediaSelectionSwitchIntent,
        settings: WebMediaOpenSettings,
    ) -> WebMediaSelectionSwitchResolution {
        match &*self.adapter {
            WebMediaSourceAdapter::Direct { .. } => match intent {
                WebMediaSelectionSwitchIntent::CatalogTarget(
                    crate::web_media_catalog::WebMediaSelectionTarget::InstalledOnly,
                ) => WebMediaSelectionSwitchResolution::NoChange,
                WebMediaSelectionSwitchIntent::CatalogTarget(_)
                | WebMediaSelectionSwitchIntent::ComponentSemantic(_) => {
                    WebMediaSelectionSwitchResolution::Unsupported
                }
            },
            WebMediaSourceAdapter::NativeHls {
                source,
                source_state,
            } => match intent {
                WebMediaSelectionSwitchIntent::CatalogTarget(
                    crate::web_media_catalog::WebMediaSelectionTarget::InstalledOnly,
                ) => WebMediaSelectionSwitchResolution::NoChange,
                WebMediaSelectionSwitchIntent::CatalogTarget(_) => {
                    WebMediaSelectionSwitchResolution::Unsupported
                }
                WebMediaSelectionSwitchIntent::ComponentSemantic(selection) => {
                    let Some(intent) = source_state.switch_intent_for_component(selection) else {
                        return WebMediaSelectionSwitchResolution::Stale;
                    };
                    WebMediaSelectionSwitchResolution::Ready(WebMediaOpenRequest::native_hls(
                        source.clone(),
                        intent,
                        settings,
                    ))
                }
            },
            WebMediaSourceAdapter::NativeDash {
                source,
                source_state,
            } => match intent {
                WebMediaSelectionSwitchIntent::CatalogTarget(
                    crate::web_media_catalog::WebMediaSelectionTarget::InstalledOnly,
                ) => WebMediaSelectionSwitchResolution::NoChange,
                WebMediaSelectionSwitchIntent::CatalogTarget(_) => {
                    WebMediaSelectionSwitchResolution::Unsupported
                }
                WebMediaSelectionSwitchIntent::ComponentSemantic(selection) => {
                    let Some(intent) = source_state.switch_intent_for_component(selection) else {
                        return WebMediaSelectionSwitchResolution::Stale;
                    };
                    WebMediaSelectionSwitchResolution::Ready(WebMediaOpenRequest::native_dash(
                        source.clone(),
                        intent,
                        settings,
                    ))
                }
            },
            WebMediaSourceAdapter::NativeHds {
                source,
                source_state,
            } => match intent {
                WebMediaSelectionSwitchIntent::CatalogTarget(
                    crate::web_media_catalog::WebMediaSelectionTarget::InstalledOnly,
                ) => WebMediaSelectionSwitchResolution::NoChange,
                WebMediaSelectionSwitchIntent::CatalogTarget(_) => {
                    WebMediaSelectionSwitchResolution::Unsupported
                }
                WebMediaSelectionSwitchIntent::ComponentSemantic(selection) => {
                    let Some(intent) = source_state.switch_intent_for_component(selection) else {
                        return WebMediaSelectionSwitchResolution::Stale;
                    };
                    WebMediaSelectionSwitchResolution::Ready(WebMediaOpenRequest::native_hds(
                        source.clone(),
                        intent,
                        settings,
                    ))
                }
            },
            WebMediaSourceAdapter::NativeSmooth {
                source,
                source_state,
            } => match intent {
                WebMediaSelectionSwitchIntent::CatalogTarget(
                    crate::web_media_catalog::WebMediaSelectionTarget::InstalledOnly,
                ) => WebMediaSelectionSwitchResolution::NoChange,
                WebMediaSelectionSwitchIntent::CatalogTarget(_) => {
                    WebMediaSelectionSwitchResolution::Unsupported
                }
                WebMediaSelectionSwitchIntent::ComponentSemantic(selection) => {
                    let Some(intent) = source_state.switch_intent_for_component(selection) else {
                        return WebMediaSelectionSwitchResolution::Stale;
                    };
                    WebMediaSelectionSwitchResolution::Ready(WebMediaOpenRequest::native_smooth(
                        source.clone(),
                        intent,
                        settings,
                    ))
                }
            },
            WebMediaSourceAdapter::Extractor {
                locator,
                source_state,
            } => {
                let selection_intent = match intent {
                    WebMediaSelectionSwitchIntent::CatalogTarget(
                        crate::web_media_catalog::WebMediaSelectionTarget::InstalledOnly,
                    ) => return WebMediaSelectionSwitchResolution::NoChange,
                    WebMediaSelectionSwitchIntent::CatalogTarget(target) => {
                        let Some(selection_intent) =
                            source_state.selection_intent_for_target(&target)
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
                    web_media_core::ExtractorInvocationReason::ExtractorBackedRecovery,
                    settings,
                ))
            }
        }
    }

    /// Проецирует settings change в controlled reopen без adapter dispatch у caller-а.
    pub(crate) fn settings_reconfigure_request(
        &self,
        policy: WebMediaSettingsReconfigurePolicy,
        network_config: fastiplayer_config::NetworkConfig,
        demux_config: fastiplayer_config::PlayerDemuxConfig,
        adaptive_settings: WebMediaOpenSettings,
    ) -> WebMediaSettingsReconfigureDecision {
        let request = match &*self.adapter {
            WebMediaSourceAdapter::Direct { locator } => {
                if policy.direct_resource == DirectResourceSettingsAction::KeepInstalled {
                    return WebMediaSettingsReconfigureDecision::NoChange;
                }
                WebMediaOpenRequest::direct(locator.clone(), network_config, demux_config)
            }
            WebMediaSourceAdapter::NativeHls {
                source,
                source_state,
            } => WebMediaOpenRequest::native_hls(
                source.clone(),
                source_state.installed_reopen_intent(),
                adaptive_settings,
            ),
            WebMediaSourceAdapter::NativeDash {
                source,
                source_state,
            } => WebMediaOpenRequest::native_dash(
                source.clone(),
                source_state.installed_reopen_intent(),
                adaptive_settings,
            ),
            WebMediaSourceAdapter::NativeHds {
                source,
                source_state,
            } => WebMediaOpenRequest::native_hds(
                source.clone(),
                source_state.installed_reopen_intent(),
                adaptive_settings,
            ),
            WebMediaSourceAdapter::NativeSmooth {
                source,
                source_state,
            } => WebMediaOpenRequest::native_smooth(
                source.clone(),
                source_state.installed_reopen_intent(),
                adaptive_settings,
            ),
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
                WebMediaOpenRequest::extractor(
                    locator.clone(),
                    selection_intent,
                    web_media_core::ExtractorInvocationReason::ExtractorBackedRecovery,
                    adaptive_settings,
                )
            }
        };
        WebMediaSettingsReconfigureDecision::Reopen(request)
    }
}

impl WebMediaSourceIntent {
    pub(crate) fn controlled_reopen_request(
        &self,
        network_config: fastiplayer_config::NetworkConfig,
        demux_config: fastiplayer_config::PlayerDemuxConfig,
        adaptive_settings: Option<WebMediaOpenSettings>,
    ) -> Option<WebMediaOpenRequest> {
        let adapter = match &*self.adapter {
            WebMediaSourceAdapter::Direct { locator } => WebMediaOpenAdapter::Direct {
                locator: locator.clone(),
                network_config,
                demux_config,
            },
            WebMediaSourceAdapter::NativeHls {
                source,
                source_state,
            } => {
                let settings = adaptive_settings?;
                WebMediaOpenAdapter::NativeHls {
                    source: source.clone(),
                    intent: source_state.installed_reopen_intent(),
                    settings,
                }
            }
            WebMediaSourceAdapter::NativeDash {
                source,
                source_state,
            } => {
                let settings = adaptive_settings?;
                WebMediaOpenAdapter::NativeDash {
                    source: source.clone(),
                    intent: source_state.installed_reopen_intent(),
                    settings,
                }
            }
            WebMediaSourceAdapter::NativeHds {
                source,
                source_state,
            } => {
                let settings = adaptive_settings?;
                WebMediaOpenAdapter::NativeHds {
                    source: source.clone(),
                    intent: source_state.installed_reopen_intent(),
                    settings,
                }
            }
            WebMediaSourceAdapter::NativeSmooth {
                source,
                source_state,
            } => {
                let settings = adaptive_settings?;
                WebMediaOpenAdapter::NativeSmooth {
                    source: source.clone(),
                    intent: source_state.installed_reopen_intent(),
                    settings,
                }
            }
            WebMediaSourceAdapter::Extractor {
                locator,
                source_state,
                ..
            } => {
                let settings = adaptive_settings?;
                WebMediaOpenAdapter::Extractor {
                    locator: locator.clone(),
                    selection_intent: source_state.installed_reopen_intent(),
                    invocation_reason: ExtractorInvocationReason::ExtractorBackedRecovery,
                    settings,
                }
            }
        };
        Some(WebMediaOpenRequest {
            adapter: Box::new(adapter),
        })
    }
}
