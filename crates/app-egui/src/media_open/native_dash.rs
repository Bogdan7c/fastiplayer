//! App-owned identity и reopen intent для доказанного native static DASH источника.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use web_media_core::{
    ComponentVariantCatalog, ComponentVariantSemanticSelectionRequest, SourceIdentity,
    WebMediaSelection, WebMediaSemanticSelectionRequest,
};

use super::types::SafeMediaLabel;

/// Process-local lineage не выводится из secret-bearing MPD URL.
static NEXT_NATIVE_DASH_SOURCE_ID: AtomicU64 = AtomicU64::new(1);

/// Reconstructible native DASH root identity без URL в diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NativeDashUrl {
    /// Exact HTTP target доступен только native DASH composition owner-у.
    target: source_core::HttpRequestTarget,
    /// Уже redacted bounded label для UI и diagnostics.
    safe_label: SafeMediaLabel,
    /// Opaque source lineage связывает fresh catalog generations.
    source_identity: SourceIdentity,
}

impl NativeDashUrl {
    /// Сохраняет exact request target отдельно от публичного safe label.
    #[must_use]
    pub(crate) fn new(target: source_core::HttpRequestTarget, safe_label: SafeMediaLabel) -> Self {
        let source_identity = SourceIdentity::new(
            NEXT_NATIVE_DASH_SOURCE_ID
                .fetch_add(1, Ordering::Relaxed)
                .max(1),
        );
        Self {
            target,
            safe_label,
            source_identity,
        }
    }

    /// Раскрывает root только app-owned HTTP composition boundary.
    #[must_use]
    pub(crate) const fn target(&self) -> &source_core::HttpRequestTarget {
        &self.target
    }

    /// Возвращает bounded label без query/credential material.
    #[must_use]
    pub(crate) const fn safe_label(&self) -> &SafeMediaLabel {
        &self.safe_label
    }

    /// Возвращает opaque lineage для fresh catalog/rematch identity.
    #[must_use]
    pub(crate) const fn source_identity(&self) -> SourceIdentity {
        self.source_identity
    }
}

impl fmt::Debug for NativeDashUrl {
    /// Не раскрывает root path/query.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeDashUrl")
            .field("target", &"<redacted>")
            .field("safe_label", &self.safe_label)
            .finish()
    }
}

/// Initial content admission может один раз перейти в extractor fallback;
/// installed reopen хранит только native semantic rematch intent.
#[derive(Clone)]
pub(crate) enum NativeDashOpenIntent {
    /// Единственный pre-Installed fallback сохраняет исходный page locator.
    InitialWithYtDlpFallback {
        /// Locator не попадает в установленный native source state.
        fallback_locator: service_ytdlp::YtDlpMediaLocator,
    },
    /// Fresh root fetch обязан semantic-rematch-ить установленный выбор.
    SemanticSelection(WebMediaSemanticSelectionRequest),
}

impl fmt::Debug for NativeDashOpenIntent {
    /// Не раскрывает fallback locator либо exact catalog identity.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitialWithYtDlpFallback { .. } => {
                formatter.write_str("InitialWithYtDlpFallback(<redacted>)")
            }
            Self::SemanticSelection(selection) => formatter
                .debug_struct("SemanticSelection")
                .field("shape", &selection.shape_kind())
                .field("identity", &"<redacted>")
                .finish(),
        }
    }
}

/// Native DASH adapter владеет neutral catalog projection и semantic reopen intent.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NativeDashSourceState {
    /// Canonical selection установленного fresh snapshot-а.
    neutral_selection: WebMediaSelection,
    /// Provider-neutral stream/component projection для sidebar/actions.
    stream_configuration: crate::web_media_stream_model::WebMediaStreamConfiguration,
    /// Parent catalog содержит один stable root item.
    catalog_attachment: crate::web_media_catalog::WebMediaCatalogAttachment,
}

impl NativeDashSourceState {
    /// Собирает single-parent projection поверх canonical DASH component catalog-а.
    pub(crate) fn new(
        neutral_selection: WebMediaSelection,
        component_catalog: Arc<ComponentVariantCatalog>,
        preference: crate::web_media_stream_model::WebMediaSelectionPreference,
    ) -> anyhow::Result<Self> {
        let web_media_core::WebMediaSelectionShape::Components(component_selection) =
            neutral_selection.shape()
        else {
            anyhow::bail!("native DASH catalog потерял component selection");
        };
        let stream_configuration =
            crate::web_media_stream_model::WebMediaStreamConfiguration::from_native_manifest(
                neutral_selection.parent().clone(),
                preference,
            )
            .with_component_variants(component_catalog, component_selection.clone())
            .map_err(anyhow::Error::new)?;
        Ok(Self {
            neutral_selection,
            stream_configuration,
            catalog_attachment: crate::web_media_catalog::WebMediaCatalogAttachment::installed_only(
            ),
        })
    }

    /// Возвращает exact neutral selection свежего установленного snapshot-а.
    pub(crate) const fn neutral_selection(&self) -> &WebMediaSelection {
        &self.neutral_selection
    }

    /// Возвращает full provider-neutral component catalog projection.
    pub(crate) const fn stream_configuration(
        &self,
    ) -> &crate::web_media_stream_model::WebMediaStreamConfiguration {
        &self.stream_configuration
    }

    /// Возвращает inert parent attachment; actions живут в component catalog-е.
    pub(crate) const fn catalog_attachment(
        &self,
    ) -> &crate::web_media_catalog::WebMediaCatalogAttachment {
        &self.catalog_attachment
    }

    /// Controlled reopen всегда refresh-ит root и semantic-rematch-ит selection.
    pub(crate) fn installed_reopen_intent(&self) -> NativeDashOpenIntent {
        NativeDashOpenIntent::SemanticSelection(self.neutral_selection.semantic_rematch_request())
    }

    /// Проверяет component action против установленного catalog generation.
    pub(crate) fn switch_intent_for_component(
        &self,
        selection: ComponentVariantSemanticSelectionRequest,
    ) -> Option<NativeDashOpenIntent> {
        self.stream_configuration
            .semantic_selection_request_for_component(selection)
            .map(NativeDashOpenIntent::SemanticSelection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_reveals_exact_target_query() {
        let source = NativeDashUrl::new(
            source_core::HttpRequestTarget::parse_exact(
                "https://media.example.test/video.mpd?access_token=top-secret",
            )
            .expect("valid target"),
            SafeMediaLabel::from_service_safe_label("media.example.test/video.mpd"),
        );

        let debug = format!("{source:?}");
        assert!(!debug.contains("top-secret"));
        assert!(!debug.contains("access_token"));
        assert!(debug.contains("<redacted>"));
    }
}
