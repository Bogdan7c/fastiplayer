//! Type-state handoff для уже загруженного direct-ingress F4M root.

use source_core::HttpRequestTarget;
use web_media_adaptive::{AdaptiveFetchedResource, AdaptiveHttpContext};

/// Bounded root response вместе с HTTP context-ом, который его получил.
///
/// Этот тип не публикует URL и не дублирует runtime: direct ingress передаёт
/// completed response существующему HDS resolver-у, а child manifests,
/// bootstrap и F4F fragments продолжают использовать тот же context.
pub struct HdsFetchedManifestInput {
    pub(crate) selected_target: HttpRequestTarget,
    pub(crate) http: AdaptiveHttpContext,
    pub(crate) fetched: AdaptiveFetchedResource,
}

impl HdsFetchedManifestInput {
    /// Связывает exact selected root с completed response и его HTTP context-ом.
    #[must_use]
    pub fn new(
        selected_target: HttpRequestTarget,
        http: AdaptiveHttpContext,
        fetched: AdaptiveFetchedResource,
    ) -> Self {
        Self {
            selected_target,
            http,
            fetched,
        }
    }
}
