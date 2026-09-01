//! App-owned identity и reopen intent для доказанного native HLS VOD/live источника.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use web_media_core::{
    ComponentVariantCatalog, ComponentVariantSemanticSelectionRequest, SourceIdentity,
    WebMediaSelection, WebMediaSemanticSelectionRequest,
};

use super::types::SafeMediaLabel;

/// Process-local lineage не выводится из secret-bearing root URL.
static NEXT_NATIVE_HLS_SOURCE_ID: AtomicU64 = AtomicU64::new(1);

/// Reconstructible native HLS top-level identity без URL в diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NativeHlsUrl {
    target: source_core::HttpRequestTarget,
    safe_label: SafeMediaLabel,
    source_identity: SourceIdentity,
}

impl NativeHlsUrl {
    /// Сохраняет exact request target и отдельно уже redacted UI label.
    #[must_use]
    pub(crate) fn new(target: source_core::HttpRequestTarget, safe_label: SafeMediaLabel) -> Self {
        let source_identity = SourceIdentity::new(
            NEXT_NATIVE_HLS_SOURCE_ID
                .fetch_add(1, Ordering::Relaxed)
                .max(1),
        );
        Self {
            target,
            safe_label,
            source_identity,
        }
    }

    /// Exact locator раскрывается только app-owned HTTP composition owner-у.
    #[must_use]
    pub(crate) const fn target(&self) -> &source_core::HttpRequestTarget {
        &self.target
    }

    /// Возвращает bounded label без URL/query material.
    #[must_use]
    pub(crate) const fn safe_label(&self) -> &SafeMediaLabel {
        &self.safe_label
    }

    /// Возвращает opaque lineage для fresh catalog generations и recovery fences.
    #[must_use]
    pub(crate) const fn source_identity(&self) -> SourceIdentity {
        self.source_identity
    }
}

impl fmt::Debug for NativeHlsUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeHlsUrl")
            .field("target", &"<redacted>")
            .field("safe_label", &self.safe_label)
            .finish()
    }
}

/// Initial admission может ровно один раз перейти в unchanged extractor path;
/// последующие open-ы несут только refresh-stable neutral selection.
#[derive(Clone)]
pub(crate) enum NativeHlsOpenIntent {
    InitialWithYtDlpFallback {
        fallback_locator: service_ytdlp::YtDlpMediaLocator,
    },
    SemanticSelection(WebMediaSemanticSelectionRequest),
}

impl fmt::Debug for NativeHlsOpenIntent {
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

/// Native HLS adapter владеет neutral catalog projection и semantic reopen intent.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NativeHlsSourceState {
    neutral_selection: WebMediaSelection,
    stream_configuration: crate::web_media_stream_model::WebMediaStreamConfiguration,
    catalog_attachment: crate::web_media_catalog::WebMediaCatalogAttachment,
}

impl NativeHlsSourceState {
    /// Собирает single-parent projection; master catalog остаётся canonical component catalog-ом.
    pub(crate) fn new(
        neutral_selection: WebMediaSelection,
        component_catalog: Option<Arc<ComponentVariantCatalog>>,
        preference: crate::web_media_stream_model::WebMediaSelectionPreference,
    ) -> anyhow::Result<Self> {
        let mut stream_configuration =
            crate::web_media_stream_model::WebMediaStreamConfiguration::from_native_manifest(
                neutral_selection.parent().clone(),
                preference,
            );
        if let Some(component_catalog) = component_catalog {
            let web_media_core::WebMediaSelectionShape::Components(component_selection) =
                neutral_selection.shape()
            else {
                anyhow::bail!("native HLS master catalog потерял component selection");
            };
            stream_configuration = stream_configuration
                .with_component_variants(component_catalog, component_selection.clone())
                .map_err(anyhow::Error::new)?;
        }
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

    /// Возвращает full provider-neutral stream/component catalog projection.
    pub(crate) const fn stream_configuration(
        &self,
    ) -> &crate::web_media_stream_model::WebMediaStreamConfiguration {
        &self.stream_configuration
    }

    /// Parent catalog честно содержит один root item; variants живут в component catalog-е.
    pub(crate) const fn catalog_attachment(
        &self,
    ) -> &crate::web_media_catalog::WebMediaCatalogAttachment {
        &self.catalog_attachment
    }

    /// Controlled reopen всегда refresh-ит root и semantic-rematch-ит selection.
    pub(crate) fn installed_reopen_intent(&self) -> NativeHlsOpenIntent {
        NativeHlsOpenIntent::SemanticSelection(self.neutral_selection.semantic_rematch_request())
    }

    /// Проверяет component action против установленного catalog-а до запуска strong reopen.
    pub(crate) fn switch_intent_for_component(
        &self,
        selection: ComponentVariantSemanticSelectionRequest,
    ) -> Option<NativeHlsOpenIntent> {
        self.stream_configuration
            .semantic_selection_request_for_component(selection)
            .map(NativeHlsOpenIntent::SemanticSelection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_reveals_exact_target_query() {
        let source = NativeHlsUrl::new(
            source_core::HttpRequestTarget::parse_exact(
                "https://media.example.test/master.m3u8?access_token=top-secret",
            )
            .expect("valid target"),
            SafeMediaLabel::from_service_safe_label("media.example.test/master.m3u8"),
        );

        let debug = format!("{source:?}");
        assert!(!debug.contains("top-secret"));
        assert!(!debug.contains("access_token"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn semantic_reopen_intent_cannot_retain_or_invoke_extractor_fallback() {
        let source = NativeHlsUrl::new(
            source_core::HttpRequestTarget::parse_exact("https://media.example.test/master.m3u8")
                .expect("valid target"),
            SafeMediaLabel::from_service_safe_label("media.example.test/master.m3u8"),
        );
        let source_identity = source.source_identity();
        let parent = web_media_core::ExactSelectionIdentity::new(
            web_media_core::CandidateIdentity::new(
                source_identity,
                web_media_core::ExtractionGeneration::new(1),
                web_media_core::CandidateFormatIdentity::new("native-hls-vod")
                    .expect("valid format identity"),
            ),
            web_media_core::SemanticIdentity::new(source_identity, "native-hls-vod")
                .expect("valid semantic identity"),
        )
        .expect("matching source lineage");
        let intent = NativeHlsOpenIntent::SemanticSelection(
            web_media_core::WebMediaSelection::candidate(parent).semantic_rematch_request(),
        );

        match intent {
            NativeHlsOpenIntent::SemanticSelection(_) => {}
            NativeHlsOpenIntent::InitialWithYtDlpFallback { .. } => {
                panic!("semantic reopen не должен содержать fallback locator")
            }
        }
    }
}
