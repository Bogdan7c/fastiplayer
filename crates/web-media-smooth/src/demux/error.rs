//! Secret-safe P4 construction failures.

use std::fmt;

use demux_api::ProgressiveDemuxStartupError;
use smooth_streaming_manifest_core::SmoothManifestError;

/// Ошибка запуска nonblocking Smooth demux worker-а.
#[derive(thiserror::Error)]
pub enum SmoothVodDemuxBuildError {
    /// Progressive worker не удалось создать.
    #[error("Smooth progressive demux worker startup failed")]
    ProgressiveStartup(#[source] ProgressiveDemuxStartupError),
}

/// Ошибка pure manifest-owned Smooth VOD seek planning.
#[derive(thiserror::Error)]
pub enum SmoothVodSeekError {
    /// Target лежит после authoritative VOD duration.
    #[error("Smooth seek target находится за пределами VOD duration")]
    TargetOutsideDuration,
    /// Selected runtime stream исчез из sealed manifest.
    #[error("Smooth seek stream отсутствует в sealed manifest")]
    StreamMissing,
    /// Validated timeline не смог вернуть fragment.
    #[error("Smooth seek timeline lookup failed")]
    Timeline(#[source] SmoothManifestError),
    /// Duration нельзя точно свести к bounded manifest ticks.
    #[error("Smooth seek target не представим в manifest clock")]
    TargetTickOverflow,
}

impl fmt::Debug for SmoothVodSeekError {
    /// Debug не раскрывает manifest custom fields или target locator.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::TargetOutsideDuration => "target-outside-duration",
            Self::StreamMissing => "stream-missing",
            Self::Timeline(_) => "timeline",
            Self::TargetTickOverflow => "target-tick-overflow",
        };
        formatter
            .debug_struct("SmoothVodSeekError")
            .field("kind", &kind)
            .finish()
    }
}

impl fmt::Debug for SmoothVodDemuxBuildError {
    /// Debug намеренно не раскрывает catalog identities или transport state.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::ProgressiveStartup(_) => "progressive_startup",
        };
        formatter
            .debug_struct("SmoothVodDemuxBuildError")
            .field("kind", &kind)
            .finish()
    }
}
