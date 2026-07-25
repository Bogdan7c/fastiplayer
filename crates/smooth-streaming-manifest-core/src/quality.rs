//! Shape-typed codec-proven Smooth Streaming quality rows.

use std::fmt;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};

use crate::custom_attributes::SmoothCustomAttributeSet;
use crate::error::{SmoothManifestError, SmoothSchemaField};
use crate::limits::SmoothManifestLimits;
use crate::model::SmoothStreamKind;

/// Stable manifest-declared QualityLevel identity без positional fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SmoothQualityIndex(u64);

impl SmoothQualityIndex {
    /// Parser-only constructor сохраняет весь declared `u64` domain.
    #[must_use]
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Bounded FourCC публикуется только после profile-specific alias admission.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SmoothCodecFourCc(Box<str>);

impl SmoothCodecFourCc {
    /// Codec parser передаёт сюда уже разрешённое exact spelling.
    pub(crate) fn new_validated(
        four_cc: &str,
        limits: &SmoothManifestLimits,
    ) -> Result<Self, SmoothManifestError> {
        if four_cc.is_empty()
            || four_cc.chars().any(char::is_control)
            || four_cc.len() > limits.maximum_string_bytes()
            || four_cc.len() != 4
            || !four_cc.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(SmoothManifestError::MalformedSchema {
                field: SmoothSchemaField::QualityLevel,
            });
        }
        Ok(Self(four_cc.to_owned().into_boxed_str()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SmoothCodecFourCc {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothCodecFourCc")
            .field("bytes", &self.0.len())
            .finish()
    }
}

/// Provenance codec bytes отделяет manifest ASC от явно выведенного AAC-LC ASC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmoothCodecConfigurationOrigin {
    H264SequenceAndPictureParameterSets,
    AacAudioSpecificConfig,
    AacDerivedFromQualityFields,
}

/// Codec-private bytes публикуются только после codec-specific proof.
#[derive(Clone, PartialEq, Eq)]
pub struct SmoothCodecConfiguration {
    bytes: Box<[u8]>,
    origin: SmoothCodecConfigurationOrigin,
}

impl SmoothCodecConfiguration {
    /// Единственная construction hatch вызывается codec proof owner-ом.
    pub(crate) const fn from_validated(
        bytes: Box<[u8]>,
        origin: SmoothCodecConfigurationOrigin,
    ) -> Self {
        Self { bytes, origin }
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn origin(&self) -> SmoothCodecConfigurationOrigin {
        self.origin
    }
}

impl fmt::Debug for SmoothCodecConfiguration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothCodecConfiguration")
            .field("bytes", &self.bytes.len())
            .field("origin", &self.origin)
            .finish()
    }
}

/// Нормализованная video quality row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmoothVideoQuality {
    index: SmoothQualityIndex,
    bitrate: NonZeroU64,
    width: NonZeroU32,
    height: NonZeroU32,
    codec: SmoothCodecFourCc,
    codec_configuration: SmoothCodecConfiguration,
    custom_attributes: SmoothCustomAttributeSet,
}

impl SmoothVideoQuality {
    /// Parser-only construction сохраняет nonzero numeric invariants.
    pub(crate) fn new(
        index: SmoothQualityIndex,
        bitrate: u64,
        width: u32,
        height: u32,
        codec: SmoothCodecFourCc,
        codec_configuration: SmoothCodecConfiguration,
        custom_attributes: SmoothCustomAttributeSet,
    ) -> Result<Self, SmoothManifestError> {
        Ok(Self {
            index,
            bitrate: required_u64(bitrate)?,
            width: required_u32(width)?,
            height: required_u32(height)?,
            codec,
            codec_configuration,
            custom_attributes,
        })
    }

    #[must_use]
    pub const fn index(&self) -> SmoothQualityIndex {
        self.index
    }

    #[must_use]
    pub const fn bitrate(&self) -> NonZeroU64 {
        self.bitrate
    }

    #[must_use]
    pub const fn width(&self) -> NonZeroU32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> NonZeroU32 {
        self.height
    }

    #[must_use]
    pub const fn codec(&self) -> &SmoothCodecFourCc {
        &self.codec
    }

    #[must_use]
    pub const fn codec_configuration(&self) -> &SmoothCodecConfiguration {
        &self.codec_configuration
    }

    #[must_use]
    pub const fn custom_attributes(&self) -> &SmoothCustomAttributeSet {
        &self.custom_attributes
    }
}

/// Нормализованная audio quality row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmoothAudioQuality {
    index: SmoothQualityIndex,
    bitrate: NonZeroU64,
    sampling_rate: NonZeroU32,
    channels: NonZeroU16,
    bits_per_sample: NonZeroU16,
    packet_size: NonZeroU16,
    audio_tag: u16,
    codec: SmoothCodecFourCc,
    codec_configuration: SmoothCodecConfiguration,
    custom_attributes: SmoothCustomAttributeSet,
}

impl SmoothAudioQuality {
    /// Parser-only construction сохраняет nonzero numeric invariants.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        index: SmoothQualityIndex,
        bitrate: u64,
        sampling_rate: u32,
        channels: u16,
        bits_per_sample: u16,
        packet_size: u16,
        audio_tag: u16,
        codec: SmoothCodecFourCc,
        codec_configuration: SmoothCodecConfiguration,
        custom_attributes: SmoothCustomAttributeSet,
    ) -> Result<Self, SmoothManifestError> {
        Ok(Self {
            index,
            bitrate: required_u64(bitrate)?,
            sampling_rate: required_u32(sampling_rate)?,
            channels: required_u16(channels)?,
            bits_per_sample: required_u16(bits_per_sample)?,
            packet_size: required_u16(packet_size)?,
            audio_tag,
            codec,
            codec_configuration,
            custom_attributes,
        })
    }

    #[must_use]
    pub const fn index(&self) -> SmoothQualityIndex {
        self.index
    }

    #[must_use]
    pub const fn bitrate(&self) -> NonZeroU64 {
        self.bitrate
    }

    #[must_use]
    pub const fn sampling_rate(&self) -> NonZeroU32 {
        self.sampling_rate
    }

    #[must_use]
    pub const fn channels(&self) -> NonZeroU16 {
        self.channels
    }

    #[must_use]
    pub const fn bits_per_sample(&self) -> NonZeroU16 {
        self.bits_per_sample
    }

    #[must_use]
    pub const fn packet_size(&self) -> NonZeroU16 {
        self.packet_size
    }

    #[must_use]
    pub const fn audio_tag(&self) -> u16 {
        self.audio_tag
    }

    #[must_use]
    pub const fn codec(&self) -> &SmoothCodecFourCc {
        &self.codec
    }

    #[must_use]
    pub const fn codec_configuration(&self) -> &SmoothCodecConfiguration {
        &self.codec_configuration
    }

    #[must_use]
    pub const fn custom_attributes(&self) -> &SmoothCustomAttributeSet {
        &self.custom_attributes
    }
}

/// Shape-typed quality предотвращает video/audio field mixing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmoothQualityLevel {
    Video(SmoothVideoQuality),
    Audio(SmoothAudioQuality),
}

impl SmoothQualityLevel {
    #[must_use]
    pub const fn index(&self) -> SmoothQualityIndex {
        match self {
            Self::Video(quality) => quality.index(),
            Self::Audio(quality) => quality.index(),
        }
    }

    #[must_use]
    pub(crate) const fn bitrate_value(&self) -> u64 {
        match self {
            Self::Video(quality) => quality.bitrate().get(),
            Self::Audio(quality) => quality.bitrate().get(),
        }
    }

    #[must_use]
    pub(crate) const fn custom_attributes(&self) -> &SmoothCustomAttributeSet {
        match self {
            Self::Video(quality) => quality.custom_attributes(),
            Self::Audio(quality) => quality.custom_attributes(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> SmoothStreamKind {
        match self {
            Self::Video(_) => SmoothStreamKind::Video,
            Self::Audio(_) => SmoothStreamKind::Audio,
        }
    }
}

/// Преобразует обязательный positive integer в nonzero storage.
fn required_u64(value: u64) -> Result<NonZeroU64, SmoothManifestError> {
    NonZeroU64::new(value).ok_or(SmoothManifestError::MalformedSchema {
        field: SmoothSchemaField::QualityLevel,
    })
}

/// Преобразует обязательный positive integer в nonzero storage.
fn required_u32(value: u32) -> Result<NonZeroU32, SmoothManifestError> {
    NonZeroU32::new(value).ok_or(SmoothManifestError::MalformedSchema {
        field: SmoothSchemaField::QualityLevel,
    })
}

/// Преобразует обязательный positive integer в nonzero storage.
fn required_u16(value: u16) -> Result<NonZeroU16, SmoothManifestError> {
    NonZeroU16::new(value).ok_or(SmoothManifestError::MalformedSchema {
        field: SmoothSchemaField::QualityLevel,
    })
}
