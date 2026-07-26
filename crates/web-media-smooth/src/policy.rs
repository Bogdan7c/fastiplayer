//! Обязательная caller-owned policy без production defaults.

use std::num::NonZeroUsize;

use bounded_xml_reader::XmlBudgets;
use smooth_streaming_manifest_core::SmoothManifestLimits;
use symphonia_format_isomp4::FragmentInitializationLimits;
use web_media_adaptive::{AdaptiveRetryPolicy, AdaptiveTransportLimits};
use web_media_core::{ComponentVariantCatalogLimit, ComponentVariantEdgeLimit};

/// Общий предел bytes всех построенных initialization segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggregateInitializationByteLimit(NonZeroUsize);

impl AggregateInitializationByteLimit {
    /// Создаёт явный ненулевой aggregate budget.
    #[must_use]
    pub const fn new(maximum_bytes: NonZeroUsize) -> Self {
        Self(maximum_bytes)
    }

    /// Возвращает maximum aggregate bytes.
    #[must_use]
    pub const fn maximum_bytes(self) -> usize {
        self.0.get()
    }
}

/// Полная policy одной атомарной подготовки Smooth VOD.
///
/// Конструктор намеренно требует все budgets: незаметного fallback на
/// библиотечные или production значения здесь нет.
#[derive(Debug, Clone)]
pub struct SmoothPreparationPolicy {
    pub(crate) adaptive_limits: AdaptiveTransportLimits,
    pub(crate) adaptive_retry: AdaptiveRetryPolicy,
    pub(crate) xml_budgets: XmlBudgets,
    pub(crate) manifest_limits: SmoothManifestLimits,
    pub(crate) initialization_limits: FragmentInitializationLimits,
    pub(crate) aggregate_initialization_limit: AggregateInitializationByteLimit,
    pub(crate) catalog_limit: ComponentVariantCatalogLimit,
    pub(crate) compatibility_edge_limit: ComponentVariantEdgeLimit,
}

impl SmoothPreparationPolicy {
    /// Собирает policy из восьми независимо именованных budget groups.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        adaptive_limits: AdaptiveTransportLimits,
        adaptive_retry: AdaptiveRetryPolicy,
        xml_budgets: XmlBudgets,
        manifest_limits: SmoothManifestLimits,
        initialization_limits: FragmentInitializationLimits,
        aggregate_initialization_limit: AggregateInitializationByteLimit,
        catalog_limit: ComponentVariantCatalogLimit,
        compatibility_edge_limit: ComponentVariantEdgeLimit,
    ) -> Self {
        Self {
            adaptive_limits,
            adaptive_retry,
            xml_budgets,
            manifest_limits,
            initialization_limits,
            aggregate_initialization_limit,
            catalog_limit,
            compatibility_edge_limit,
        }
    }
}
