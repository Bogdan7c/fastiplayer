//! Caller-owned S38 budgets. Здесь нет hidden network defaults.

use std::num::NonZeroUsize;

use bounded_xml_reader::XmlBudgets;
use demux_api::{DemuxSniffBudget, ProgressiveAsyncSeekLimits, ProgressiveDemuxBufferLimits};
use hds_manifest_core::{F4mManifestLimits, HdsBootstrapLimits};
use media_core::DemuxRetryHint;
use web_media_adaptive::{AdaptiveRetryPolicy, AdaptiveTransportLimits};

/// Полный bounded policy одного HDS VOD open-а.
#[derive(Debug, Clone, Copy)]
pub struct HdsVodOpenPolicy {
    /// S04X XML budgets для root и child F4M документов.
    pub xml_budgets: XmlBudgets,
    /// Domain limits F4M hierarchy/media/bootstrap rows.
    pub manifest_limits: F4mManifestLimits,
    /// Binary limits для abst/asrt/afrt expansion.
    pub bootstrap_limits: HdsBootstrapLimits,
    /// Общий S31 resource/descriptor budget.
    pub adaptive_limits: AdaptiveTransportLimits,
    /// Общий S31 retry policy.
    pub adaptive_retry: AdaptiveRetryPolicy,
    /// Bounded F4F registry sniff.
    pub demux_sniff_budget: DemuxSniffBudget,
    /// Player-facing worker queue limits.
    pub demux_buffer_limits: ProgressiveDemuxBufferLimits,
    /// Retry hint для nonblocking demux events.
    pub demux_retry_hint: DemuxRetryHint,
    /// Максимум outstanding player seek receipts.
    pub async_seek_limits: ProgressiveAsyncSeekLimits,
    /// Максимальная глубина set-level F4M hierarchy.
    pub maximum_hierarchy_depth: usize,
    /// Максимум fetched manifest documents одного open-а.
    pub maximum_manifest_documents: usize,
    /// Максимум flattened rendition rows одного open-а.
    pub maximum_renditions: usize,
    /// Максимум одновременно выполняемых content/capability probe-ов rendition-ов.
    pub maximum_parallel_rendition_probes: NonZeroUsize,
    /// Максимум готовых selected F4F fragments поверх текущего demux fragment-а.
    pub maximum_buffered_fragments: NonZeroUsize,
    /// Максимум параллельных selected F4F HTTP fetch-ов с in-order delivery.
    pub maximum_concurrent_fragment_fetches: NonZeroUsize,
}
