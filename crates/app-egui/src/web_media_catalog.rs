//! Process-lifetime orchestration verified web-media catalogs.

mod coordinator;
mod discovery;
mod model;

pub(crate) use coordinator::{
    WebMediaCatalogCoordinator, WebMediaCatalogCorrelation, WebMediaCatalogScope,
};
pub(crate) use discovery::{
    DiscoveredWebMediaCatalog, WebMediaCatalogAttachment, WebMediaCatalogDiscovery,
};
pub(crate) use model::{
    WebMediaCatalogChoice, WebMediaCatalogState, WebMediaFacetAction, WebMediaFacetOption,
    WebMediaMode, WebMediaRememberedPreference, WebMediaSelectionTarget,
};

#[cfg(test)]
mod tests;
