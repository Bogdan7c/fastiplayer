//! Общая app-owned policy для adaptive HTTP runtimes.

use std::num::NonZeroUsize;

use anyhow::{Result, anyhow};
use rustiplayer_config::NetworkConfig;
use web_media_adaptive::AdaptiveTransportLimits;
use web_media_transport_api::SourceGeneration;

/// Первая runtime generation одного независимого adaptive open attempt-а.
const INITIAL_ADAPTIVE_SOURCE_GENERATION: u64 = 1;

/// Возвращает named initial generation без HLS/DASH coupling-а.
#[must_use]
pub(crate) const fn initial_adaptive_source_generation() -> SourceGeneration {
    SourceGeneration::new(INITIAL_ADAPTIVE_SOURCE_GENERATION)
}

/// Проецирует общий network memory budget в bounded adaptive transport policy.
pub(crate) fn adaptive_transport_limits(
    network: &NetworkConfig,
) -> Result<AdaptiveTransportLimits> {
    let maximum_resource_bytes = usize::try_from(network.memory_cache_mb)
        .ok()
        .and_then(|megabytes| megabytes.checked_mul(1_024 * 1_024))
        .and_then(NonZeroUsize::new)
        .ok_or_else(|| {
            anyhow!("network.memory_cache_mb нельзя выразить как adaptive resource byte budget")
        })?;
    Ok(AdaptiveTransportLimits::new(
        NonZeroUsize::new(2 * 1_024 * 1_024).expect("adaptive manifest budget"),
        maximum_resource_bytes,
        NonZeroUsize::new(8_192).expect("adaptive descriptor budget"),
    ))
}
