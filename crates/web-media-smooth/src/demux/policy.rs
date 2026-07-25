//! Caller-owned P4 readiness, interleave и progressive queue budgets.

use demux_api::{
    CompositeComponentLeadPolicy, DemuxSniffBudget, ProgressiveAsyncSeekLimits,
    ProgressiveDemuxBufferLimits,
};
use media_core::DemuxRetryHint;

/// Полная P4 policy без hidden production defaults.
#[derive(Clone, Copy, Debug)]
pub struct SmoothVodDemuxPolicy {
    pub(crate) sniff_budget: DemuxSniffBudget,
    pub(crate) lead_policy: CompositeComponentLeadPolicy,
    pub(crate) progressive_limits: ProgressiveDemuxBufferLimits,
    pub(crate) retry_hint: DemuxRetryHint,
    pub(crate) asynchronous_seek_limits: ProgressiveAsyncSeekLimits,
}

impl SmoothVodDemuxPolicy {
    /// Собирает independently validated policies для demux worker-а.
    #[must_use]
    pub const fn new(
        sniff_budget: DemuxSniffBudget,
        lead_policy: CompositeComponentLeadPolicy,
        progressive_limits: ProgressiveDemuxBufferLimits,
        retry_hint: DemuxRetryHint,
        asynchronous_seek_limits: ProgressiveAsyncSeekLimits,
    ) -> Self {
        Self {
            sniff_budget,
            lead_policy,
            progressive_limits,
            retry_hint,
            asynchronous_seek_limits,
        }
    }
}
