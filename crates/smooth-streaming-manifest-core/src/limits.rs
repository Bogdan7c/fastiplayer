//! Mandatory caller-owned budgets Smooth Streaming normalization.

use std::num::NonZeroUsize;

use thiserror::Error;

/// Именованный budget используется и в diagnostics, и в stable error taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothManifestLimitKind {
    Streams,
    QualitiesPerStream,
    TotalQualities,
    TimelineEntriesPerStream,
    TotalTimelineEntries,
    FragmentsPerStream,
    TotalFragments,
    TemplateBytes,
    StringBytes,
    CodecBytes,
    CustomAttributesPerQuality,
    TotalCustomAttributes,
    CustomAttributeNameBytes,
    CustomAttributeValueBytes,
}

/// Builder сообщает точное обязательное поле без hidden defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("не задан обязательный Smooth Streaming budget {field:?}")]
pub struct MissingSmoothManifestLimit {
    field: SmoothManifestLimitKind,
}

impl MissingSmoothManifestLimit {
    #[must_use]
    pub const fn field(self) -> SmoothManifestLimitKind {
        self.field
    }
}

/// Ошибка различает отсутствие, ноль и противоречивую per/total пару.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SmoothManifestLimitBuildError {
    #[error(transparent)]
    Missing(#[from] MissingSmoothManifestLimit),
    #[error("Smooth Streaming budget {field:?} обязан быть больше нуля")]
    Zero { field: SmoothManifestLimitKind },
    #[error("per-stream Smooth Streaming budget превышает total budget")]
    PerStreamExceedsTotal {
        per_stream: SmoothManifestLimitKind,
        total: SmoothManifestLimitKind,
    },
}

/// Полный immutable набор budgets; production default намеренно отсутствует.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmoothManifestLimits {
    maximum_streams: NonZeroUsize,
    maximum_qualities_per_stream: NonZeroUsize,
    maximum_total_qualities: NonZeroUsize,
    maximum_timeline_entries_per_stream: NonZeroUsize,
    maximum_total_timeline_entries: NonZeroUsize,
    maximum_fragments_per_stream: NonZeroUsize,
    maximum_total_fragments: NonZeroUsize,
    maximum_template_bytes: NonZeroUsize,
    maximum_string_bytes: NonZeroUsize,
    maximum_codec_bytes: NonZeroUsize,
    maximum_custom_attributes_per_quality: NonZeroUsize,
    maximum_total_custom_attributes: NonZeroUsize,
    maximum_custom_attribute_name_bytes: NonZeroUsize,
    maximum_custom_attribute_value_bytes: NonZeroUsize,
}

impl SmoothManifestLimits {
    #[must_use]
    pub fn builder() -> SmoothManifestLimitsBuilder {
        SmoothManifestLimitsBuilder::new()
    }

    #[must_use]
    pub const fn maximum_streams(&self) -> usize {
        self.maximum_streams.get()
    }

    #[must_use]
    pub const fn maximum_qualities_per_stream(&self) -> usize {
        self.maximum_qualities_per_stream.get()
    }

    #[must_use]
    pub const fn maximum_total_qualities(&self) -> usize {
        self.maximum_total_qualities.get()
    }

    #[must_use]
    pub const fn maximum_timeline_entries_per_stream(&self) -> usize {
        self.maximum_timeline_entries_per_stream.get()
    }

    #[must_use]
    pub const fn maximum_total_timeline_entries(&self) -> usize {
        self.maximum_total_timeline_entries.get()
    }

    #[must_use]
    pub const fn maximum_fragments_per_stream(&self) -> usize {
        self.maximum_fragments_per_stream.get()
    }

    #[must_use]
    pub const fn maximum_total_fragments(&self) -> usize {
        self.maximum_total_fragments.get()
    }

    #[must_use]
    pub const fn maximum_template_bytes(&self) -> usize {
        self.maximum_template_bytes.get()
    }

    #[must_use]
    pub const fn maximum_string_bytes(&self) -> usize {
        self.maximum_string_bytes.get()
    }

    #[must_use]
    pub const fn maximum_codec_bytes(&self) -> usize {
        self.maximum_codec_bytes.get()
    }

    #[must_use]
    pub const fn maximum_custom_attributes_per_quality(&self) -> usize {
        self.maximum_custom_attributes_per_quality.get()
    }

    #[must_use]
    pub const fn maximum_total_custom_attributes(&self) -> usize {
        self.maximum_total_custom_attributes.get()
    }

    #[must_use]
    pub const fn maximum_custom_attribute_name_bytes(&self) -> usize {
        self.maximum_custom_attribute_name_bytes.get()
    }

    #[must_use]
    pub const fn maximum_custom_attribute_value_bytes(&self) -> usize {
        self.maximum_custom_attribute_value_bytes.get()
    }
}

/// Complete named builder предотвращает неаудируемые production defaults.
#[derive(Debug, Clone, Default)]
pub struct SmoothManifestLimitsBuilder {
    maximum_streams: Option<usize>,
    maximum_qualities_per_stream: Option<usize>,
    maximum_total_qualities: Option<usize>,
    maximum_timeline_entries_per_stream: Option<usize>,
    maximum_total_timeline_entries: Option<usize>,
    maximum_fragments_per_stream: Option<usize>,
    maximum_total_fragments: Option<usize>,
    maximum_template_bytes: Option<usize>,
    maximum_string_bytes: Option<usize>,
    maximum_codec_bytes: Option<usize>,
    maximum_custom_attributes_per_quality: Option<usize>,
    maximum_total_custom_attributes: Option<usize>,
    maximum_custom_attribute_name_bytes: Option<usize>,
    maximum_custom_attribute_value_bytes: Option<usize>,
}

impl SmoothManifestLimitsBuilder {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            maximum_streams: None,
            maximum_qualities_per_stream: None,
            maximum_total_qualities: None,
            maximum_timeline_entries_per_stream: None,
            maximum_total_timeline_entries: None,
            maximum_fragments_per_stream: None,
            maximum_total_fragments: None,
            maximum_template_bytes: None,
            maximum_string_bytes: None,
            maximum_codec_bytes: None,
            maximum_custom_attributes_per_quality: None,
            maximum_total_custom_attributes: None,
            maximum_custom_attribute_name_bytes: None,
            maximum_custom_attribute_value_bytes: None,
        }
    }

    #[must_use]
    pub const fn maximum_streams(mut self, maximum: usize) -> Self {
        self.maximum_streams = Some(maximum);
        self
    }

    #[must_use]
    pub const fn maximum_qualities_per_stream(mut self, maximum: usize) -> Self {
        self.maximum_qualities_per_stream = Some(maximum);
        self
    }

    #[must_use]
    pub const fn maximum_total_qualities(mut self, maximum: usize) -> Self {
        self.maximum_total_qualities = Some(maximum);
        self
    }

    #[must_use]
    pub const fn maximum_timeline_entries_per_stream(mut self, maximum: usize) -> Self {
        self.maximum_timeline_entries_per_stream = Some(maximum);
        self
    }

    #[must_use]
    pub const fn maximum_total_timeline_entries(mut self, maximum: usize) -> Self {
        self.maximum_total_timeline_entries = Some(maximum);
        self
    }

    #[must_use]
    pub const fn maximum_fragments_per_stream(mut self, maximum: usize) -> Self {
        self.maximum_fragments_per_stream = Some(maximum);
        self
    }

    #[must_use]
    pub const fn maximum_total_fragments(mut self, maximum: usize) -> Self {
        self.maximum_total_fragments = Some(maximum);
        self
    }

    #[must_use]
    pub const fn maximum_template_bytes(mut self, maximum: usize) -> Self {
        self.maximum_template_bytes = Some(maximum);
        self
    }

    #[must_use]
    pub const fn maximum_string_bytes(mut self, maximum: usize) -> Self {
        self.maximum_string_bytes = Some(maximum);
        self
    }

    #[must_use]
    pub const fn maximum_codec_bytes(mut self, maximum: usize) -> Self {
        self.maximum_codec_bytes = Some(maximum);
        self
    }

    #[must_use]
    pub const fn maximum_custom_attributes_per_quality(mut self, maximum: usize) -> Self {
        self.maximum_custom_attributes_per_quality = Some(maximum);
        self
    }

    #[must_use]
    pub const fn maximum_total_custom_attributes(mut self, maximum: usize) -> Self {
        self.maximum_total_custom_attributes = Some(maximum);
        self
    }

    #[must_use]
    pub const fn maximum_custom_attribute_name_bytes(mut self, maximum: usize) -> Self {
        self.maximum_custom_attribute_name_bytes = Some(maximum);
        self
    }

    #[must_use]
    pub const fn maximum_custom_attribute_value_bytes(mut self, maximum: usize) -> Self {
        self.maximum_custom_attribute_value_bytes = Some(maximum);
        self
    }

    pub fn build(self) -> Result<SmoothManifestLimits, SmoothManifestLimitBuildError> {
        let limits = SmoothManifestLimits {
            maximum_streams: required_nonzero(
                self.maximum_streams,
                SmoothManifestLimitKind::Streams,
            )?,
            maximum_qualities_per_stream: required_nonzero(
                self.maximum_qualities_per_stream,
                SmoothManifestLimitKind::QualitiesPerStream,
            )?,
            maximum_total_qualities: required_nonzero(
                self.maximum_total_qualities,
                SmoothManifestLimitKind::TotalQualities,
            )?,
            maximum_timeline_entries_per_stream: required_nonzero(
                self.maximum_timeline_entries_per_stream,
                SmoothManifestLimitKind::TimelineEntriesPerStream,
            )?,
            maximum_total_timeline_entries: required_nonzero(
                self.maximum_total_timeline_entries,
                SmoothManifestLimitKind::TotalTimelineEntries,
            )?,
            maximum_fragments_per_stream: required_nonzero(
                self.maximum_fragments_per_stream,
                SmoothManifestLimitKind::FragmentsPerStream,
            )?,
            maximum_total_fragments: required_nonzero(
                self.maximum_total_fragments,
                SmoothManifestLimitKind::TotalFragments,
            )?,
            maximum_template_bytes: required_nonzero(
                self.maximum_template_bytes,
                SmoothManifestLimitKind::TemplateBytes,
            )?,
            maximum_string_bytes: required_nonzero(
                self.maximum_string_bytes,
                SmoothManifestLimitKind::StringBytes,
            )?,
            maximum_codec_bytes: required_nonzero(
                self.maximum_codec_bytes,
                SmoothManifestLimitKind::CodecBytes,
            )?,
            maximum_custom_attributes_per_quality: required_nonzero(
                self.maximum_custom_attributes_per_quality,
                SmoothManifestLimitKind::CustomAttributesPerQuality,
            )?,
            maximum_total_custom_attributes: required_nonzero(
                self.maximum_total_custom_attributes,
                SmoothManifestLimitKind::TotalCustomAttributes,
            )?,
            maximum_custom_attribute_name_bytes: required_nonzero(
                self.maximum_custom_attribute_name_bytes,
                SmoothManifestLimitKind::CustomAttributeNameBytes,
            )?,
            maximum_custom_attribute_value_bytes: required_nonzero(
                self.maximum_custom_attribute_value_bytes,
                SmoothManifestLimitKind::CustomAttributeValueBytes,
            )?,
        };
        validate_per_total(
            limits.maximum_qualities_per_stream,
            limits.maximum_total_qualities,
            SmoothManifestLimitKind::QualitiesPerStream,
            SmoothManifestLimitKind::TotalQualities,
        )?;
        validate_per_total(
            limits.maximum_timeline_entries_per_stream,
            limits.maximum_total_timeline_entries,
            SmoothManifestLimitKind::TimelineEntriesPerStream,
            SmoothManifestLimitKind::TotalTimelineEntries,
        )?;
        validate_per_total(
            limits.maximum_fragments_per_stream,
            limits.maximum_total_fragments,
            SmoothManifestLimitKind::FragmentsPerStream,
            SmoothManifestLimitKind::TotalFragments,
        )?;
        validate_per_total(
            limits.maximum_custom_attributes_per_quality,
            limits.maximum_total_custom_attributes,
            SmoothManifestLimitKind::CustomAttributesPerQuality,
            SmoothManifestLimitKind::TotalCustomAttributes,
        )?;
        Ok(limits)
    }
}

fn required_nonzero(
    value: Option<usize>,
    field: SmoothManifestLimitKind,
) -> Result<NonZeroUsize, SmoothManifestLimitBuildError> {
    let value = value.ok_or(MissingSmoothManifestLimit { field })?;
    NonZeroUsize::new(value).ok_or(SmoothManifestLimitBuildError::Zero { field })
}

fn validate_per_total(
    per_stream: NonZeroUsize,
    total: NonZeroUsize,
    per_stream_kind: SmoothManifestLimitKind,
    total_kind: SmoothManifestLimitKind,
) -> Result<(), SmoothManifestLimitBuildError> {
    if per_stream > total {
        return Err(SmoothManifestLimitBuildError::PerStreamExceedsTotal {
            per_stream: per_stream_kind,
            total: total_kind,
        });
    }
    Ok(())
}
