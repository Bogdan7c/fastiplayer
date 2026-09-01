//! Public request boundary подготовки Smooth VOD.

use source_core::{HttpRequestTarget, SourceRuntimeConfig};
use web_media_adaptive::{AdaptiveFetchedResource, AdaptiveHttpContext};
use web_media_core::{ComponentVariantCatalogGeneration, PreferredHeightPolicy};
use web_media_transport_api::TransportOpenRequest;

use crate::SmoothPreparationPolicy;

/// Уже загруженный bounded root manifest вместе с исходным HTTP context-ом.
///
/// Direct ingress передаёт этот type-state существующему Smooth preparation
/// owner-у, поэтому parser/catalog/runtime используют тот же response и тот же
/// cookie/cancellation context без повторного `/Manifest` request-а.
pub struct SmoothFetchedManifestInput {
    pub(crate) selected_target: HttpRequestTarget,
    pub(crate) http: AdaptiveHttpContext,
    pub(crate) fetched: AdaptiveFetchedResource,
}

impl SmoothFetchedManifestInput {
    /// Связывает completed root response с context-ом, который его получил.
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

/// Полный request: transport intent, runtime reference и caller-owned policies.
pub struct SmoothPrepareRequest<'config> {
    pub(crate) transport: TransportOpenRequest,
    pub(crate) source_config: &'config SourceRuntimeConfig,
    pub(crate) catalog_generation: ComponentVariantCatalogGeneration,
    pub(crate) preferred_height: PreferredHeightPolicy,
    pub(crate) policy: SmoothPreparationPolicy,
    /// Optional direct-ingress handoff; `None` сохраняет extractor fetch path.
    pub(crate) fetched_manifest: Option<SmoothFetchedManifestInput>,
}

impl<'config> SmoothPrepareRequest<'config> {
    /// Создаёт подготовку без positional booleans и скрытых budget-ов.
    #[must_use]
    pub fn new(
        transport: TransportOpenRequest,
        source_config: &'config SourceRuntimeConfig,
        catalog_generation: ComponentVariantCatalogGeneration,
        preferred_height: PreferredHeightPolicy,
        policy: SmoothPreparationPolicy,
    ) -> Self {
        Self {
            transport,
            source_config,
            catalog_generation,
            preferred_height,
            policy,
            fetched_manifest: None,
        }
    }

    /// Передаёт уже fetched authoritative root body без второго network request-а.
    #[must_use]
    pub fn with_fetched_manifest(mut self, fetched_manifest: SmoothFetchedManifestInput) -> Self {
        self.fetched_manifest = Some(fetched_manifest);
        self
    }
}
