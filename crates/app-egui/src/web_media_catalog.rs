//! Process-lifetime orchestration declared web-media catalogs.

mod attachment;
mod coordinator;
mod model;

pub(crate) use attachment::WebMediaCatalogAttachment;
pub(crate) use coordinator::{
    WebMediaCatalogCoordinator, WebMediaCatalogCorrelation, WebMediaCatalogScope,
};
pub(crate) use model::{
    WebMediaCatalogChoice, WebMediaCatalogState, WebMediaFacetAction, WebMediaFacetOption,
    WebMediaMode, WebMediaRememberedPreference, WebMediaSelectionTarget,
};

#[cfg(test)]
mod tests;
