use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use web_media_core::ExactSelectionIdentity;

use super::model::{WebMediaCatalogChoice, WebMediaSelectionTarget};

static NEXT_ATTACHMENT_ID: AtomicU64 = AtomicU64::new(1);

/// Runtime-only attachment Installed source-а. Equality сравнивает только opaque instance.
#[derive(Clone)]
pub(crate) struct WebMediaCatalogAttachment {
    id: u64,
    parent: ExactSelectionIdentity,
    choices: Arc<[WebMediaCatalogChoice]>,
    active: WebMediaSelectionTarget,
}

impl WebMediaCatalogAttachment {
    pub(crate) fn new(
        parent: ExactSelectionIdentity,
        choices: Vec<WebMediaCatalogChoice>,
        active: WebMediaSelectionTarget,
    ) -> anyhow::Result<Self> {
        if !choices.iter().any(|choice| choice.target == active) {
            anyhow::bail!("active choice отсутствует в declared web-media catalog");
        }
        if !active_matches_parent(&active, &parent) {
            anyhow::bail!("active choice не принадлежит parent identity declared catalog-а");
        }
        Ok(Self {
            id: NEXT_ATTACHMENT_ID.fetch_add(1, Ordering::Relaxed),
            parent,
            choices: choices.into(),
            active,
        })
    }

    pub(crate) const fn parent(&self) -> &ExactSelectionIdentity {
        &self.parent
    }

    pub(super) fn choices(&self) -> Arc<[WebMediaCatalogChoice]> {
        Arc::clone(&self.choices)
    }

    pub(super) const fn active(&self) -> &WebMediaSelectionTarget {
        &self.active
    }
}

fn active_matches_parent(
    active: &WebMediaSelectionTarget,
    parent: &ExactSelectionIdentity,
) -> bool {
    let selection = match active {
        #[cfg(test)]
        WebMediaSelectionTarget::Fixture(_) => return true,
        WebMediaSelectionTarget::Parent { selection } => selection.as_ref(),
        WebMediaSelectionTarget::Composed {
            parent_preference, ..
        } => parent_preference.as_ref(),
    };
    selection.exact_identity() == parent.exact()
        && selection.semantic_identity() == parent.semantic()
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
            .field("choice_count", &self.choices.len())
            .field("active", &self.active)
            .finish()
    }
}
