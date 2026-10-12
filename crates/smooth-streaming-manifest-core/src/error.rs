//! Stable secret-safe taxonomy ошибок Smooth Streaming manifest boundary.

use std::fmt;

use thiserror::Error;

use crate::limits::SmoothManifestLimitKind;

/// Поле schema, которое нарушило обязательный structural контракт.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothSchemaField {
    Root,
    MajorVersion,
    MinorVersion,
    TimeScale,
    Duration,
    StreamIndex,
    QualityLevel,
    CustomAttributes,
    Timeline,
    Url,
}

/// Причина несовместимости с узким VOD profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothProfileIncompatibility {
    LiveManifest,
    UnsupportedStreamKind,
    UnsupportedVideoCodec,
    UnsupportedAudioCodec,
    UnsupportedCodecProfile,
    UnsupportedAudioTag,
    TextStream,
    SparseStream,
    EmbeddedStream,
    CompositeStream,
    TrickModeStream,
    VendorExtension,
    DuplicateQualityIndex,
    AmbiguousQualityRenderIdentity,
    NoCommonPlaybackInterval,
    MissingRequiredStream,
    MixedQualityKinds,
}

/// Явно неподдерживаемая стандартная конструкция vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothUnsupportedConstruct {
    UnknownElement,
    UnknownAttribute,
    SparseTimeline,
    MultipleTimelines,
    LookAheadFragments,
    DvrWindow,
    NonVodChunking,
}

/// Typed причина rejection относительного fragment URL template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothUrlTemplateError {
    Empty,
    TooLong,
    AbsoluteReference,
    QueryOrFragment,
    Backslash,
    Traversal,
    ControlCharacter,
    UnterminatedPlaceholder,
    UnknownPlaceholder,
    MissingBitratePlaceholder,
    MissingStartTimePlaceholder,
    DuplicatePlaceholder,
    CustomAttributesUnavailable,
    RenderedPathTooLong,
}

/// Typed причина invalid compact chunk timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothTimelineError {
    Empty,
    ZeroDuration,
    ZeroRepeat,
    NegativeRepeat,
    RepeatRequiresVersion22,
    MissingAdjacentExplicitStart,
    NonDivisibleInferredDuration,
    BackwardStart,
    Overlap,
    Discontinuity,
    ArithmeticOverflow,
    FragmentIndexOutOfRange,
    OutsidePresentationDuration,
}

/// Причина отказа strict codec-private proof boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothCodecConfigurationError {
    Empty,
    OddHexLength,
    InvalidHexDigit,
    MissingH264SequenceParameterSet,
    MissingH264PictureParameterSet,
    DuplicateH264SequenceParameterSet,
    DuplicateH264PictureParameterSet,
    UnexpectedH264NalUnit,
    InvalidAacAudioSpecificConfig,
    AacObjectTypeMismatch,
    AacSamplingRateMismatch,
    AacChannelCountMismatch,
}

/// Вид declared count для точного typed mismatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothDeclaredCountKind {
    StreamCount,
    QualityCount,
    FragmentCount,
}

/// Верхнеуровневая taxonomy готова принять D2 XML parser без string matching.
#[derive(Error, PartialEq, Eq)]
pub enum SmoothManifestError {
    #[error("Smooth Streaming manifest preparation canceled")]
    Cancelled,
    #[error("hardened XML boundary flinch manifest")]
    Xml {
        #[source]
        source: bounded_xml_reader::XmlReadError,
    },
    /// Well-formed XML document не является Smooth client manifest.
    #[error("document root is not SmoothStreamingMedia")]
    InvalidRoot,
    #[error("Smooth Streaming manifest exceeds the mandatory budget")]
    LimitExceeded {
        limit: SmoothManifestLimitKind,
        maximum: usize,
    },
    #[error("Smooth Streaming manifest violates mandatory schema")]
    MalformedSchema { field: SmoothSchemaField },
    #[error("Smooth Streaming version {major}.{minor} is not supported")]
    UnsupportedVersion { major: u16, minor: u16 },
    #[error("Smooth Streaming manifest is incompatible with VOD profile")]
    ProfileIncompatible {
        reason: SmoothProfileIncompatibility,
    },
    #[error("DRM-protected Smooth Streaming manifest is not supported")]
    DrmProtected,
    #[error("private Smooth Streaming extension unsupported")]
    PrivateExtension,
    #[error("Smooth Streaming construct is not included in the supported profile")]
    UnsupportedConstruct {
        construct: SmoothUnsupportedConstruct,
    },
    #[error("The fragment URL template did not pass the secure grammar")]
    InvalidUrlTemplate { reason: SmoothUrlTemplateError },
    #[error("chunk timeline doesn't pass exact normalization")]
    InvalidTimeline { reason: SmoothTimelineError },
    #[error("codec configuration failed bounded opaque boundary")]
    InvalidCodecConfiguration {
        reason: SmoothCodecConfigurationError,
    },
    #[error("The declared manifest count does not match the normalized count.")]
    DeclaredCountMismatch {
        kind: SmoothDeclaredCountKind,
        declared: u64,
        actual: u64,
    },
}

impl fmt::Debug for SmoothManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("Cancelled"),
            Self::Xml { .. } => formatter.write_str("Xml"),
            Self::InvalidRoot => formatter.write_str("InvalidRoot"),
            Self::LimitExceeded { limit, maximum } => formatter
                .debug_struct("LimitExceeded")
                .field("limit", limit)
                .field("maximum", maximum)
                .finish(),
            Self::MalformedSchema { field } => formatter
                .debug_struct("MalformedSchema")
                .field("field", field)
                .finish(),
            Self::UnsupportedVersion { major, minor } => formatter
                .debug_struct("UnsupportedVersion")
                .field("major", major)
                .field("minor", minor)
                .finish(),
            Self::ProfileIncompatible { reason } => formatter
                .debug_struct("ProfileIncompatible")
                .field("reason", reason)
                .finish(),
            Self::DrmProtected => formatter.write_str("DrmProtected"),
            Self::PrivateExtension => formatter.write_str("PrivateExtension"),
            Self::UnsupportedConstruct { construct } => formatter
                .debug_struct("UnsupportedConstruct")
                .field("construct", construct)
                .finish(),
            Self::InvalidUrlTemplate { reason } => formatter
                .debug_struct("InvalidUrlTemplate")
                .field("reason", reason)
                .finish(),
            Self::InvalidTimeline { reason } => formatter
                .debug_struct("InvalidTimeline")
                .field("reason", reason)
                .finish(),
            Self::InvalidCodecConfiguration { reason } => formatter
                .debug_struct("InvalidCodecConfiguration")
                .field("reason", reason)
                .finish(),
            Self::DeclaredCountMismatch {
                kind,
                declared,
                actual,
            } => formatter
                .debug_struct("DeclaredCountMismatch")
                .field("kind", kind)
                .field("declared", declared)
                .field("actual", actual)
                .finish(),
        }
    }
}
