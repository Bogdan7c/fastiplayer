//! Provider-neutral foundation segmented HTTP media.
//!
//! Crate владеет bounded manifest/segment lifecycle, generation fencing,
//! retry/backoff и explicit nonblocking readiness. Конкретные HLS/DASH policy,
//! container parsing, ABR, player/UI и durable queue state сюда не входят.

#![forbid(unsafe_code)]

mod adapter;
mod config;
mod fetch;
mod manifest;
mod range_source;
mod segment;
mod timeline;

pub use adapter::{
    ActivatableBlockingOrderedSegmentAdapter, BlockingOrderedSegmentAdapter,
    BlockingOrderedSegmentReadAheadHandle,
};
pub use config::{AdaptiveRetryPolicy, AdaptiveRetryPolicyError, AdaptiveTransportLimits};
pub use fetch::{
    AdaptiveFetchedResource, AdaptiveHttpContext, AdaptiveResourceFetchRequest,
    AdaptiveResourcePurpose, AdaptiveResourceQueryApplication, AdaptiveResourceSecretForwarding,
    AdaptiveTransportError,
};
pub use manifest::{
    AdaptiveManifestFetcher, ManifestBaseUri, ManifestFetchRequest, ManifestPoll, ManifestResource,
};
pub use range_source::{
    AdaptiveRangeByteSource, AdaptiveRangeSourceConfig, AdaptiveRangeSourceOpenError,
};
pub use segment::{
    AdaptiveOrderedSegmentSource, AdaptiveSegmentCompletion, AdaptiveSegmentDescriptor,
    AdaptiveSegmentSnapshot, AdaptiveSegmentSnapshotError, SegmentByteRange, SegmentPoll,
    SourceRangeError,
};
pub use timeline::{
    AdaptivePresentation, ComponentClockMetadata, DvrWindow, DvrWindowError, LiveEdge,
};

#[cfg(test)]
mod tests;
