use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use web_media_core::ExactSelectionIdentity;

use super::model::{WebMediaCatalogChoice, WebMediaSelectionTarget};

static NEXT_ATTACHMENT_ID: AtomicU64 = AtomicU64::new(1);

/// Provider composition job. Locator/request material остаётся внутри реализации.
pub(crate) trait WebMediaCatalogDiscovery: Send + Sync {
    fn discover(
        &self,
        cancellation: source_core::CancellationToken,
    ) -> anyhow::Result<DiscoveredWebMediaCatalog>;
}

/// Полный unpublished результат одного background pass-а.
pub(crate) struct DiscoveredWebMediaCatalog {
    pub(crate) choices: Vec<WebMediaCatalogChoice>,
    pub(crate) active: WebMediaSelectionTarget,
    pub(crate) rejected_siblings: usize,
}

/// Runtime-only attachment Installed source-а. Equality сравнивает только opaque instance.
#[derive(Clone)]
pub(crate) struct WebMediaCatalogAttachment {
    id: u64,
    parent: ExactSelectionIdentity,
    discovery: Arc<dyn WebMediaCatalogDiscovery>,
}

impl WebMediaCatalogAttachment {
    pub(crate) fn new(
        parent: ExactSelectionIdentity,
        discovery: Arc<dyn WebMediaCatalogDiscovery>,
    ) -> Self {
        Self {
            id: NEXT_ATTACHMENT_ID.fetch_add(1, Ordering::Relaxed),
            parent,
            discovery,
        }
    }

    pub(crate) const fn parent(&self) -> &ExactSelectionIdentity {
        &self.parent
    }

    pub(super) fn run(
        &self,
        cancellation: source_core::CancellationToken,
    ) -> anyhow::Result<DiscoveredWebMediaCatalog> {
        self.discovery.discover(cancellation)
    }
}

impl PartialEq for WebMediaCatalogAttachment {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for WebMediaCatalogAttachment {}

impl fmt::Debug for WebMediaCatalogAttachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebMediaCatalogAttachment")
            .field("id", &self.id)
            .field("parent", &self.parent)
            .field("discovery", &"<provider-private>")
            .finish()
    }
}
