//! Root и StreamIndex normalized Smooth Streaming VOD model.

use std::fmt;

use crate::error::{
    SmoothDeclaredCountKind, SmoothManifestError, SmoothProfileIncompatibility, SmoothSchemaField,
};
use crate::limits::{SmoothManifestLimitKind, SmoothManifestLimits};
use crate::quality::SmoothQualityLevel;
use crate::template::SmoothFragmentUrlTemplate;
use crate::time::{SmoothTime, SmoothTimescale};
use crate::timeline::SmoothChunkTimeline;

/// Bounded standard StreamIndex component name.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SmoothStreamName(Box<str>);

impl SmoothStreamName {
    /// Parser-only constructor принимает уже bounded schema string.
    pub(crate) fn from_validated(value: String) -> Self {
        Self(value.into_boxed_str())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SmoothStreamName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothStreamName")
            .field("bytes", &self.0.len())
            .finish()
    }
}

/// Bounded standard StreamIndex language metadata.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SmoothStreamLanguage(Box<str>);

impl SmoothStreamLanguage {
    /// Parser-only constructor принимает уже bounded schema string.
    pub(crate) fn from_validated(value: String) -> Self {
        Self(value.into_boxed_str())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SmoothStreamLanguage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothStreamLanguage")
            .field("bytes", &self.0.len())
            .finish()
    }
}

/// Именованный parser-to-model handoff исключает ambiguous positional Options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmoothStreamIdentityMetadata {
    name: Option<SmoothStreamName>,
    language: Option<SmoothStreamLanguage>,
}

impl SmoothStreamIdentityMetadata {
    /// Только parser формирует identity metadata из admitted attributes.
    pub(crate) const fn new(
        name: Option<SmoothStreamName>,
        language: Option<SmoothStreamLanguage>,
    ) -> Self {
        Self { name, language }
    }

    #[must_use]
    pub const fn name(&self) -> Option<&SmoothStreamName> {
        self.name.as_ref()
    }

    #[must_use]
    pub const fn language(&self) -> Option<&SmoothStreamLanguage> {
        self.language.as_ref()
    }
}

/// Поддерживаемая root vocabulary ограничена exact версиями 2.0 и 2.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothManifestVersion {
    V2_0,
    V2_2,
}

impl SmoothManifestVersion {
    /// Версия создаётся только parser-ом из root attributes.
    pub(crate) fn from_major_minor(major: u16, minor: u16) -> Result<Self, SmoothManifestError> {
        match (major, minor) {
            (2, 0) => Ok(Self::V2_0),
            (2, 2) => Ok(Self::V2_2),
            _ => Err(SmoothManifestError::UnsupportedVersion { major, minor }),
        }
    }
}

/// VOD profile допускает только independent Video и Audio stream axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothStreamKind {
    Video,
    Audio,
}

/// Declared root stream count не кодируется ambiguous `Option`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SmoothDeclaredStreamCount {
    Unspecified,
    Exact(u64),
}

/// Declared StreamIndex quality count не кодируется ambiguous `Option`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SmoothDeclaredQualityCount {
    #[cfg(test)]
    Unspecified,
    Exact(u64),
}

/// Один normalized StreamIndex без HTTP/runtime semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmoothStream {
    kind: SmoothStreamKind,
    identity_metadata: SmoothStreamIdentityMetadata,
    timescale: SmoothTimescale,
    url_template: SmoothFragmentUrlTemplate,
    qualities: Box<[SmoothQualityLevel]>,
    timeline: SmoothChunkTimeline,
}

/// Именованный parser-only construction handoff сохраняет читаемость boundary.
pub(crate) struct SmoothStreamConstruction {
    pub(crate) kind: SmoothStreamKind,
    pub(crate) identity_metadata: SmoothStreamIdentityMetadata,
    pub(crate) timescale: SmoothTimescale,
    pub(crate) url_template: SmoothFragmentUrlTemplate,
    pub(crate) qualities: Vec<SmoothQualityLevel>,
    pub(crate) timeline: SmoothChunkTimeline,
    pub(crate) declared_quality_count: SmoothDeclaredQualityCount,
}

impl SmoothStream {
    /// Parser-only constructor закрепляет axis, clock и count invariants.
    pub(crate) fn new(
        input: SmoothStreamConstruction,
        limits: &SmoothManifestLimits,
    ) -> Result<Self, SmoothManifestError> {
        let SmoothStreamConstruction {
            kind,
            identity_metadata,
            timescale,
            url_template,
            qualities,
            timeline,
            declared_quality_count,
        } = input;
        if qualities.is_empty() {
            return Err(SmoothManifestError::MalformedSchema {
                field: SmoothSchemaField::QualityLevel,
            });
        }
        enforce_limit(
            qualities.len(),
            limits.maximum_qualities_per_stream(),
            SmoothManifestLimitKind::QualitiesPerStream,
        )?;
        if qualities.iter().any(|quality| quality.kind() != kind) {
            return Err(SmoothManifestError::ProfileIncompatible {
                reason: SmoothProfileIncompatibility::MixedQualityKinds,
            });
        }
        validate_quality_identities(&qualities, &url_template)?;
        if timeline.timescale() != timescale {
            return Err(SmoothManifestError::MalformedSchema {
                field: SmoothSchemaField::Timeline,
            });
        }
        validate_declared_quality_count(declared_quality_count, qualities.len())?;
        Ok(Self {
            kind,
            identity_metadata,
            timescale,
            url_template,
            qualities: qualities.into_boxed_slice(),
            timeline,
        })
    }

    #[must_use]
    pub const fn kind(&self) -> SmoothStreamKind {
        self.kind
    }

    #[must_use]
    pub const fn identity_metadata(&self) -> &SmoothStreamIdentityMetadata {
        &self.identity_metadata
    }

    #[must_use]
    pub const fn name(&self) -> Option<&SmoothStreamName> {
        self.identity_metadata.name()
    }

    #[must_use]
    pub const fn language(&self) -> Option<&SmoothStreamLanguage> {
        self.identity_metadata.language()
    }

    #[must_use]
    pub const fn timescale(&self) -> SmoothTimescale {
        self.timescale
    }

    #[must_use]
    pub const fn url_template(&self) -> &SmoothFragmentUrlTemplate {
        &self.url_template
    }

    #[must_use]
    pub fn qualities(&self) -> &[SmoothQualityLevel] {
        &self.qualities
    }

    #[must_use]
    pub const fn timeline(&self) -> &SmoothChunkTimeline {
        &self.timeline
    }
}

/// Root normalized VOD manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmoothManifest {
    version: SmoothManifestVersion,
    duration: SmoothTime,
    streams: Box<[SmoothStream]>,
}

impl SmoothManifest {
    /// Единственная construction path вызывается после полного parser validation.
    pub(crate) fn new_vod(
        version: SmoothManifestVersion,
        duration: SmoothTime,
        streams: Vec<SmoothStream>,
        declared_stream_count: SmoothDeclaredStreamCount,
        limits: &SmoothManifestLimits,
    ) -> Result<Self, SmoothManifestError> {
        if streams.is_empty() {
            return Err(SmoothManifestError::ProfileIncompatible {
                reason: SmoothProfileIncompatibility::MissingRequiredStream,
            });
        }
        enforce_limit(
            streams.len(),
            limits.maximum_streams(),
            SmoothManifestLimitKind::Streams,
        )?;
        let total_qualities = streams.iter().try_fold(0usize, |count, stream| {
            count
                .checked_add(stream.qualities.len())
                .ok_or(SmoothManifestError::LimitExceeded {
                    limit: SmoothManifestLimitKind::TotalQualities,
                    maximum: limits.maximum_total_qualities(),
                })
        })?;
        enforce_limit(
            total_qualities,
            limits.maximum_total_qualities(),
            SmoothManifestLimitKind::TotalQualities,
        )?;
        validate_declared_stream_count(declared_stream_count, streams.len())?;
        validate_presentation_intervals(duration, &streams)?;
        Ok(Self {
            version,
            duration,
            streams: streams.into_boxed_slice(),
        })
    }

    #[must_use]
    pub const fn version(&self) -> SmoothManifestVersion {
        self.version
    }

    #[must_use]
    pub const fn duration(&self) -> SmoothTime {
        self.duration
    }

    #[must_use]
    pub fn streams(&self) -> &[SmoothStream] {
        &self.streams
    }
}

/// Duplicate declared index и rendered URL identity не публикуются.
fn validate_quality_identities(
    qualities: &[SmoothQualityLevel],
    url_template: &SmoothFragmentUrlTemplate,
) -> Result<(), SmoothManifestError> {
    for (quality_index, quality) in qualities.iter().enumerate() {
        for previous in &qualities[..quality_index] {
            if previous.index() == quality.index() {
                return Err(SmoothManifestError::ProfileIncompatible {
                    reason: SmoothProfileIncompatibility::DuplicateQualityIndex,
                });
            }
            if previous.bitrate_value() == quality.bitrate_value()
                && (!url_template.uses_custom_attributes()
                    || previous.custom_attributes() == quality.custom_attributes())
            {
                return Err(SmoothManifestError::ProfileIncompatible {
                    reason: SmoothProfileIncompatibility::AmbiguousQualityRenderIdentity,
                });
            }
        }
    }
    Ok(())
}

/// Каждая timeline лежит в duration, а общий exact playback interval непуст.
fn validate_presentation_intervals(
    duration: SmoothTime,
    streams: &[SmoothStream],
) -> Result<(), SmoothManifestError> {
    let mut common_start = streams[0].timeline.first_start();
    let mut common_end = streams[0].timeline.last_end();
    for stream in streams {
        let first_start = stream.timeline.first_start();
        let last_end = stream.timeline.last_end();
        if last_end > duration {
            return Err(SmoothManifestError::InvalidTimeline {
                reason: crate::error::SmoothTimelineError::OutsidePresentationDuration,
            });
        }
        common_start = common_start.max(first_start);
        common_end = common_end.min(last_end);
    }
    if common_start >= common_end {
        return Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::NoCommonPlaybackInterval,
        });
    }
    Ok(())
}

/// Сверяет optional declared QualityLevels с фактическим числом rows.
fn validate_declared_quality_count(
    declared_count: SmoothDeclaredQualityCount,
    actual: usize,
) -> Result<(), SmoothManifestError> {
    match declared_count {
        SmoothDeclaredQualityCount::Exact(declared) => {
            validate_declared_count(SmoothDeclaredCountKind::QualityCount, declared, actual)
        }
        #[cfg(test)]
        SmoothDeclaredQualityCount::Unspecified => Ok(()),
    }
}

/// Сверяет optional declared StreamIndexCount с фактическим числом streams.
fn validate_declared_stream_count(
    declared_count: SmoothDeclaredStreamCount,
    actual: usize,
) -> Result<(), SmoothManifestError> {
    let SmoothDeclaredStreamCount::Exact(declared) = declared_count else {
        return Ok(());
    };
    validate_declared_count(SmoothDeclaredCountKind::StreamCount, declared, actual)
}

/// Сравнивает declared u64 только после checked conversion actual count.
fn validate_declared_count(
    kind: SmoothDeclaredCountKind,
    declared: u64,
    actual: usize,
) -> Result<(), SmoothManifestError> {
    let actual = u64::try_from(actual).map_err(|_| SmoothManifestError::LimitExceeded {
        limit: SmoothManifestLimitKind::TotalQualities,
        maximum: usize::MAX,
    })?;
    if declared != actual {
        return Err(SmoothManifestError::DeclaredCountMismatch {
            kind,
            declared,
            actual,
        });
    }
    Ok(())
}

/// Локальный guard не скрывает limit identity.
fn enforce_limit(
    observed: usize,
    maximum: usize,
    limit: SmoothManifestLimitKind,
) -> Result<(), SmoothManifestError> {
    if observed > maximum {
        Err(SmoothManifestError::LimitExceeded { limit, maximum })
    } else {
        Ok(())
    }
}
