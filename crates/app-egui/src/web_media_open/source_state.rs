//! Extractor-private reconstructible selection state за neutral app boundary.

use std::sync::Arc;

use service_ytdlp::YtDlpCandidateSelection;

use super::{YtDlpCandidateOpenIntent, catalog};

/// Provider DTO и reverse routes никогда не пересекают этот adapter owner.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ExtractorMediaSourceState {
    pub(super) neutral_selection: web_media_core::WebMediaSelection,
    pub(super) candidate_selection: YtDlpCandidateSelection,
    pub(super) composed_selection: Option<Box<service_ytdlp::YtDlpComposedSelection>>,
    pub(super) stream_configuration: crate::web_media_stream_model::WebMediaStreamConfiguration,
    pub(super) catalog_attachment: crate::web_media_catalog::WebMediaCatalogAttachment,
    pub(super) catalog_selection_routes: Arc<[catalog::ExtractorCatalogSelectionRoute]>,
}

impl ExtractorMediaSourceState {
    /// Возвращает canonical N01 selection без provider token-а.
    pub(crate) const fn neutral_selection(&self) -> &web_media_core::WebMediaSelection {
        &self.neutral_selection
    }

    /// Возвращает secret-safe installed stream projection для UI model owner-а.
    pub(crate) const fn stream_configuration(
        &self,
    ) -> &crate::web_media_stream_model::WebMediaStreamConfiguration {
        &self.stream_configuration
    }

    /// Возвращает neutral catalog attachment без extractor route payload-а.
    pub(crate) const fn catalog_attachment(
        &self,
    ) -> &crate::web_media_catalog::WebMediaCatalogAttachment {
        &self.catalog_attachment
    }

    /// Разрешает neutral catalog target внутри extractor adapter-а.
    pub(crate) fn selection_intent_for_target(
        &self,
        target: &crate::web_media_catalog::WebMediaSelectionTarget,
    ) -> Option<YtDlpCandidateOpenIntent> {
        self.catalog_selection_routes
            .iter()
            .find(|route| route.target() == target)
            .map(|route| route.selection_intent(self.stream_configuration.preference()))
    }

    /// Строит component reopen, не раскрывая provider parent token lifecycle-у.
    pub(crate) fn selection_intent_for_component(
        &self,
        semantic_selection: web_media_core::ComponentVariantSemanticSelectionRequest,
    ) -> YtDlpCandidateOpenIntent {
        YtDlpCandidateOpenIntent::exact_with_component_semantic_selection(
            Box::new(self.candidate_selection.clone()),
            &self.stream_configuration,
            semantic_selection,
        )
    }

    /// Сохраняет установленный parent/component выбор при suspend/reopen.
    pub(crate) fn installed_reopen_intent(&self) -> YtDlpCandidateOpenIntent {
        match &self.composed_selection {
            Some(selection) => YtDlpCandidateOpenIntent::composed(
                selection.clone(),
                Box::new(self.candidate_selection.clone()),
                self.stream_configuration.preference(),
            ),
            None => YtDlpCandidateOpenIntent::exact_preserving_installed_stream_configuration(
                Box::new(self.candidate_selection.clone()),
                &self.stream_configuration,
            ),
        }
    }

    /// Extractor-focused tests проверяют exact rematch token только внутри adapter module.
    #[cfg(test)]
    pub(crate) const fn candidate_selection(&self) -> &YtDlpCandidateSelection {
        &self.candidate_selection
    }
}
