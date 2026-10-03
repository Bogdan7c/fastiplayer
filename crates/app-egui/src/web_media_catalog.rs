//! Process-lifetime orchestration declared web-media catalogs.
//!
//! Доменная модель catalog-а живёт в `media_source_open::web_media_catalog` и
//! re-export-ится здесь под прежними путями; app-egui владеет только
//! coordinator-ом, который связывает catalog с queue binding-ами плейлиста.

mod coordinator;

pub(crate) use coordinator::{
    WebMediaCatalogCoordinator, WebMediaCatalogCorrelation, WebMediaCatalogScope,
};
pub(crate) use media_source_open::web_media_catalog::{
    WebMediaAutomaticQualityDirection, WebMediaCatalog, WebMediaCatalogAttachment,
    WebMediaCatalogState, WebMediaFacetAction, WebMediaFacetOption, WebMediaMode,
    WebMediaRememberedPreference, WebMediaSelectionTarget,
};
// Выбор каталога в production-коде app собирал только `web_media_open`, который
// переехал в `media-source-open` (session-05); здесь он нужен лишь тестам coordinator-а.
#[cfg(test)]
pub(crate) use media_source_open::web_media_catalog::WebMediaCatalogChoice;
// Ошибка catalog-а нужна только coordinator-у; наружу модуля не re-export-ится.
use media_source_open::web_media_catalog::WebMediaCatalogSafeError;

/// Собирает functional fixture через те же installed-only attachment/model boundaries.
#[cfg(test)]
pub(crate) fn installed_only_catalog_state_for_test() -> WebMediaCatalogState {
    let attachment = WebMediaCatalogAttachment::installed_only();
    WebMediaCatalogState::Ready(std::sync::Arc::new(
        WebMediaCatalog::from_attachment(1, None, &attachment)
            .expect("installed-only fixture must be internally consistent"),
    ))
}
