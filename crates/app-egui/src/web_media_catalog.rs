//! Process-lifetime orchestration declared web-media catalogs.

mod attachment;
mod coordinator;
mod model;

pub(crate) use attachment::WebMediaCatalogAttachment;
pub(crate) use coordinator::{
    WebMediaCatalogCoordinator, WebMediaCatalogCorrelation, WebMediaCatalogScope,
};
pub(crate) use model::{
    WebMediaAutomaticQualityDirection, WebMediaCatalog, WebMediaCatalogChoice,
    WebMediaCatalogState, WebMediaFacetAction, WebMediaFacetOption, WebMediaMode,
    WebMediaRememberedPreference, WebMediaSelectionTarget,
};

/// Собирает functional fixture через те же installed-only attachment/model boundaries.
#[cfg(test)]
pub(crate) fn installed_only_catalog_state_for_test() -> WebMediaCatalogState {
    let attachment = WebMediaCatalogAttachment::installed_only();
    WebMediaCatalogState::Ready(std::sync::Arc::new(
        model::WebMediaCatalog::new(1, None, attachment.choices(), attachment.active())
            .expect("installed-only fixture must be internally consistent"),
    ))
}

#[cfg(test)]
mod tests;
