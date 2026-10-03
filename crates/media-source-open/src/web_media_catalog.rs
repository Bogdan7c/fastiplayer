//! Доменная модель declared web-media catalog-а: choices, facet picker,
//! automatic quality и runtime attachment Installed source-а.
//!
//! Process-lifetime оркестрация catalog-ов (coordinator, привязанный к queue
//! binding-ам плейлиста) остаётся в `app-egui` и строит catalog только через
//! `WebMediaCatalog::from_attachment`; внутреннее устройство attachment-а
//! наружу не выходит. Тестовый target `WebMediaSelectionTarget::Fixture`
//! существует только при `cfg(any(test, feature = "test-fixtures"))`.

mod attachment;
mod model;

pub use attachment::WebMediaCatalogAttachment;
pub use model::{
    WebMediaAutomaticQualityDirection, WebMediaCatalog, WebMediaCatalogChoice,
    WebMediaCatalogSafeError, WebMediaCatalogState, WebMediaFacetAction, WebMediaFacetOption,
    WebMediaMode, WebMediaRememberedPreference, WebMediaSelectionTarget,
};

#[cfg(test)]
mod tests;
