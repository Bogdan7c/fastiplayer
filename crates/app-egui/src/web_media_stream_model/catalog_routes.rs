//! Временный N05A→N05B adapter neutral catalog targets к extractor open tokens.
//!
//! Catalog и sidebar видят только [`WebMediaSelectionTarget`]. Provider-owned
//! selections остаются в этой закрытой route table до их удаления в N05B.

use service_ytdlp::{YtDlpCandidateSelection, YtDlpComposedSelection};

use super::WebMediaStreamConfiguration;
use crate::web_media_catalog::WebMediaSelectionTarget;
use crate::web_media_open::YtDlpCandidateOpenIntent;

/// Extractor-private compatibility route; neutral catalog/UI его содержимое не видят.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum ExtractorCatalogSelectionRoute {
    /// Neutral atomic/muxed target маршрутизируется к exact extractor selection.
    Candidate {
        target: WebMediaSelectionTarget,
        selection: Box<YtDlpCandidateSelection>,
    },
    /// Neutral composed target маршрутизируется к уже проверенной A/V паре.
    SeparateComponents {
        target: WebMediaSelectionTarget,
        selection: Box<YtDlpComposedSelection>,
        parent_preference: Box<YtDlpCandidateSelection>,
    },
}

impl ExtractorCatalogSelectionRoute {
    /// Возвращает только neutral lookup key без provider payload-а.
    pub(crate) const fn target(&self) -> &WebMediaSelectionTarget {
        match self {
            Self::Candidate { target, .. } | Self::SeparateComponents { target, .. } => target,
        }
    }
}

impl WebMediaStreamConfiguration {
    /// Прикрепляет extractor-private route table после построения neutral catalog rows.
    pub(crate) fn with_catalog_selection_routes(
        mut self,
        routes: Vec<ExtractorCatalogSelectionRoute>,
    ) -> Self {
        self.catalog_selection_routes = routes.into();
        self
    }

    /// Разрешает neutral target обратно в прежний open intent без изменения switch lifecycle.
    pub(crate) fn selection_intent_for_catalog_target(
        &self,
        target: &WebMediaSelectionTarget,
    ) -> Option<YtDlpCandidateOpenIntent> {
        let route = self
            .catalog_selection_routes
            .iter()
            .find(|route| route.target() == target)?;
        Some(match route {
            ExtractorCatalogSelectionRoute::Candidate { selection, .. } => {
                YtDlpCandidateOpenIntent::exact_parent_provider_default(
                    selection.clone(),
                    self.preference,
                )
            }
            ExtractorCatalogSelectionRoute::SeparateComponents {
                selection,
                parent_preference,
                ..
            } => YtDlpCandidateOpenIntent::composed(
                selection.clone(),
                parent_preference.clone(),
                self.preference,
            ),
        })
    }
}
