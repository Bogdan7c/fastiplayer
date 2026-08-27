//! Immutable HLS runtime profile: transport admission, open budgets и seek accounting.
//!
//! Модуль не владеет I/O или lifecycle; он только строит checked policy values для parent owner-а.

use std::num::NonZeroUsize;
use std::time::Duration;

use anyhow::Result;
use demux_api::{
    CompositeComponentLeadPolicy, DemuxInputCapability, DemuxSniffBudget,
    ProgressiveAsyncSeekLimits, ProgressiveDemuxBufferLimits,
};
use hls_playlist_core::HlsParserLimits;
use media_core::DemuxRetryHint;
use web_media_adaptive::AdaptiveTransportLimits;
use web_media_hls::{HlsCatalogBuildPolicy, HlsVodOpenPolicy, HlsVodSeekLandingPolicy};

/// Ограничивает число outstanding seek receipts одного HLS runtime-а.
pub(super) fn hls_async_seek_limits() -> ProgressiveAsyncSeekLimits {
    ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(16).expect("HLS outstanding seek receipts"))
}

/// Собирает единый VOD policy для native и yt-dlp HLS preparation paths.
pub(crate) fn hls_policy(limits: AdaptiveTransportLimits) -> Result<HlsVodOpenPolicy> {
    Ok(HlsVodOpenPolicy {
        seek_landing_policy: HlsVodSeekLandingPolicy::DecodeFromOrBeforeTarget,
        parser_limits: HlsParserLimits::default(),
        demux_sniff_budget: DemuxSniffBudget::new(
            NonZeroUsize::new(64 * 1_024).expect("HLS sniff bytes"),
            NonZeroUsize::new(8).expect("HLS sniff segments"),
            Duration::from_secs(2),
        )?,
        progressive_limits: ProgressiveDemuxBufferLimits::new(
            NonZeroUsize::new(256).expect("HLS event queue"),
            NonZeroUsize::new(16 * 1_024 * 1_024).expect("HLS encoded queue"),
        ),
        retry_hint: DemuxRetryHint::new(Duration::from_millis(10))?,
        composite_lead_policy: CompositeComponentLeadPolicy::single_pending_packet(
            Duration::from_secs(3),
            NonZeroUsize::new(4 * 1_024 * 1_024).expect("HLS composite packet"),
        )?,
        maximum_key_resource_bytes: NonZeroUsize::new(64).expect("HLS key response"),
        maximum_seek_index_entries: NonZeroUsize::new(4_096).expect("HLS seek anchors"),
        maximum_seek_replay_events: NonZeroUsize::new(65_536).expect("HLS seek replay events"),
        maximum_seek_replay_bytes: limits.maximum_segment_bytes,
    })
}

/// Задаёт bounded catalog discovery budgets без provider/runtime ownership.
pub(super) fn hls_catalog_policy() -> Result<HlsCatalogBuildPolicy> {
    Ok(HlsCatalogBuildPolicy {
        catalog_limit: web_media_core::ComponentVariantCatalogLimit::new(256)?,
        compatibility_edge_limit: web_media_core::ComponentVariantEdgeLimit::new(4_096)?,
        maximum_unique_children: NonZeroUsize::new(256)
            .expect("HLS catalog child limit is non-zero"),
    })
}

/// Planner HLS transport output не делает TS playable для progressive HTTP rows.
pub(crate) fn hls_transport_input() -> DemuxInputCapability {
    DemuxInputCapability::OrderedSegments
}
