//! Pure bounded parser/model Microsoft Smooth Streaming VOD client manifest-а.
//!
//! Crate владеет только schema-level значениями, exact clocks, относительными
//! fragment URL templates и compact timeline normalization. HTTP, quality
//! selection, fMP4, demux, playback и seek принадлежат следующим слоям и
//! намеренно отсутствуют в этой dependency boundary.
//!
//! Downstream может получить validated model только через parser:
//! ```compile_fail
//! use smooth_streaming_manifest_core::{SmoothManifest, SmoothManifestVersion};
//! let forged = SmoothManifest {
//!     version: SmoothManifestVersion::V2_0,
//!     duration: panic!("private construction запрещена"),
//!     streams: Box::new([]),
//! };
//! ```
//!
//! Parser-only quality constructors также не являются public API:
//! ```compile_fail
//! use smooth_streaming_manifest_core::SmoothVideoQuality;
//! let forged = SmoothVideoQuality::new(
//!     1, 1, 1, panic!(), panic!(), panic!(),
//! );
//! ```

mod codec;
mod custom_attributes;
mod error;
mod limits;
mod model;
mod parser;
mod parser_quality;
mod parser_values;
mod quality;
mod template;
mod time;
mod timeline;
mod timeline_input;

#[cfg(test)]
mod tests_limits;
#[cfg(test)]
mod tests_model;
#[cfg(test)]
mod tests_support;
#[cfg(test)]
mod tests_template;
#[cfg(test)]
mod tests_time;
#[cfg(test)]
mod tests_timeline;

pub use custom_attributes::{
    SmoothCustomAttribute, SmoothCustomAttributeName, SmoothCustomAttributeSet,
    SmoothCustomAttributeValue,
};
pub use error::{
    SmoothCodecConfigurationError, SmoothDeclaredCountKind, SmoothManifestError,
    SmoothProfileIncompatibility, SmoothSchemaField, SmoothTimelineError,
    SmoothUnsupportedConstruct, SmoothUrlTemplateError,
};
pub use limits::{
    MissingSmoothManifestLimit, SmoothManifestLimitBuildError, SmoothManifestLimitKind,
    SmoothManifestLimits, SmoothManifestLimitsBuilder,
};
#[cfg(test)]
pub(crate) use model::{
    SmoothDeclaredQualityCount, SmoothDeclaredStreamCount, SmoothStreamConstruction,
};
pub use model::{
    SmoothManifest, SmoothManifestVersion, SmoothStream, SmoothStreamIdentityMetadata,
    SmoothStreamKind, SmoothStreamLanguage, SmoothStreamName,
};
pub use parser::{
    SMOOTH_STREAMING_DEFAULT_TIMESCALE_TICKS_PER_SECOND, SmoothManifestParseRequest,
    parse_vod_client_manifest, parse_vod_client_manifest_cancellable,
};
pub use quality::{
    SmoothAudioQuality, SmoothCodecConfiguration, SmoothCodecConfigurationOrigin,
    SmoothCodecFourCc, SmoothQualityIndex, SmoothQualityLevel, SmoothVideoQuality,
};
pub use template::{
    SmoothCustomAttributesRender, SmoothFragmentUrlRenderContext, SmoothFragmentUrlTemplate,
};
pub use time::{SmoothTime, SmoothTimeError, SmoothTimescale};
#[cfg(test)]
pub(crate) use timeline::SmoothManifestTimelineBudget;
pub use timeline::{
    SmoothChunkFragment, SmoothChunkFragmentIter, SmoothChunkRun, SmoothChunkTimeline,
};
#[cfg(test)]
pub(crate) use timeline_input::{
    SmoothChunkDuration, SmoothChunkEntry, SmoothChunkRepeat, SmoothChunkStart,
    SmoothDeclaredFragmentCount,
};
