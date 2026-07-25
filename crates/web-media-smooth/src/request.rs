//! Public request boundary подготовки Smooth VOD.

use source_core::SourceRuntimeConfig;
use web_media_core::{ComponentVariantCatalogGeneration, PreferredHeightPolicy};
use web_media_transport_api::TransportOpenRequest;

use crate::SmoothPreparationPolicy;

/// Полный request: transport intent, runtime reference и caller-owned policies.
pub struct SmoothPrepareRequest<'config> {
    pub(crate) transport: TransportOpenRequest,
    pub(crate) source_config: &'config SourceRuntimeConfig,
    pub(crate) catalog_generation: ComponentVariantCatalogGeneration,
    pub(crate) preferred_height: PreferredHeightPolicy,
    pub(crate) policy: SmoothPreparationPolicy,
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
        }
    }
}
