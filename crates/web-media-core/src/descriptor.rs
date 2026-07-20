use std::fmt;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};

use crate::{CandidateIdentity, NormalizedCodec, SemanticIdentity, StreamLayout, VideoHeight};

/// Максимальное число subtitle descriptors у одного candidate-а.
pub const MAX_SUBTITLE_DESCRIPTORS: usize = 256;

/// Верхняя граница neutral video width.
pub const MAX_VIDEO_WIDTH: u32 = 16_384;

/// Максимальная длина нормализованного language tag.
const MAX_LANGUAGE_TAG_UTF8_BYTES: usize = 64;

/// Максимальная длина subtitle format identity.
const MAX_SUBTITLE_FORMAT_IDENTITY_UTF8_BYTES: usize = 256;

/// Dynamic-range hint без codec/backend policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DynamicRange {
    /// Явный SDR.
    Sdr,
    /// Явный HDR.
    Hdr,
    /// Metadata недостаточно для статического вывода.
    Unknown,
}

/// Exact rational frame rate без `f64`/NaN ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameRate {
    /// Frames numerator.
    numerator: NonZeroU32,
    /// Time denominator.
    denominator: NonZeroU32,
}

impl FrameRate {
    /// Создаёт положительную rational frame rate.
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, FrameRateError> {
        let numerator = NonZeroU32::new(numerator).ok_or(FrameRateError::ZeroNumerator)?;
        let denominator = NonZeroU32::new(denominator).ok_or(FrameRateError::ZeroDenominator)?;
        Ok(Self {
            numerator,
            denominator,
        })
    }

    /// Возвращает numerator.
    pub const fn numerator(self) -> u32 {
        self.numerator.get()
    }

    /// Возвращает denominator.
    pub const fn denominator(self) -> u32 {
        self.denominator.get()
    }
}

/// Ошибка rational frame rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRateError {
    /// Zero numerator не описывает video stream.
    ZeroNumerator,
    /// Zero denominator запрещён математически.
    ZeroDenominator,
}

impl fmt::Display for FrameRateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroNumerator => {
                formatter.write_str("frame-rate numerator должен быть ненулевым")
            }
            Self::ZeroDenominator => {
                formatter.write_str("frame-rate denominator должен быть ненулевым")
            }
        }
    }
}

impl std::error::Error for FrameRateError {}

/// Проверенная sample rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SampleRate(NonZeroU32);

impl SampleRate {
    /// Создаёт положительную sample rate без policy о decoder capability.
    pub fn new(hertz: u32) -> Result<Self, SampleRateError> {
        NonZeroU32::new(hertz)
            .map(Self)
            .ok_or(SampleRateError::Zero)
    }

    /// Возвращает Hz.
    pub const fn hertz(self) -> u32 {
        self.0.get()
    }
}

/// Ошибка sample rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleRateError {
    /// Нулевая sample rate.
    Zero,
}

impl fmt::Display for SampleRateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("sample rate должна быть больше нуля")
    }
}

impl std::error::Error for SampleRateError {}

/// Проверенное число audio channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelCount(NonZeroU16);

impl ChannelCount {
    /// Создаёт положительное channel count.
    pub fn new(channels: u16) -> Result<Self, ChannelCountError> {
        NonZeroU16::new(channels)
            .map(Self)
            .ok_or(ChannelCountError::Zero)
    }

    /// Возвращает число channels.
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

/// Ошибка channel count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelCountError {
    /// Нулевое число channels.
    Zero,
}

impl fmt::Display for ChannelCountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("число audio channels должно быть больше нуля")
    }
}

impl std::error::Error for ChannelCountError {}

/// Проверенная ненулевая video width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VideoWidth(NonZeroU32);

impl VideoWidth {
    /// Проверяет диапазон `1..=MAX_VIDEO_WIDTH`.
    pub fn new(pixels: u32) -> Result<Self, VideoWidthError> {
        let width = NonZeroU32::new(pixels).ok_or(VideoWidthError::Zero)?;
        if pixels > MAX_VIDEO_WIDTH {
            return Err(VideoWidthError::TooLarge {
                provided_pixels: pixels,
                maximum_pixels: MAX_VIDEO_WIDTH,
            });
        }

        Ok(Self(width))
    }

    /// Возвращает width в pixels.
    pub const fn pixels(self) -> u32 {
        self.0.get()
    }
}

/// Ошибка checked video width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoWidthError {
    /// Нулевая width не описывает video representation.
    Zero,
    /// Width выше named compatibility bound.
    TooLarge {
        /// Полученное значение.
        provided_pixels: u32,
        /// Разрешённое значение.
        maximum_pixels: u32,
    },
}

impl fmt::Display for VideoWidthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("ширина видео должна быть больше нуля"),
            Self::TooLarge {
                provided_pixels,
                maximum_pixels,
            } => write!(
                formatter,
                "ширина {provided_pixels}px превышает лимит {maximum_pixels}px"
            ),
        }
    }
}

impl std::error::Error for VideoWidthError {}

/// Проверенный положительный bitrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bitrate(NonZeroU64);

impl Bitrate {
    /// Создаёт bitrate в bits per second.
    pub fn new(bits_per_second: u64) -> Result<Self, BitrateError> {
        NonZeroU64::new(bits_per_second)
            .map(Self)
            .ok_or(BitrateError::Zero)
    }

    /// Возвращает bits per second.
    pub const fn bits_per_second(self) -> u64 {
        self.0.get()
    }
}

/// Ошибка checked bitrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitrateError {
    /// Нулевой bitrate не несёт полезного quality hint.
    Zero,
}

impl fmt::Display for BitrateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bitrate должен быть больше нуля")
    }
}

impl std::error::Error for BitrateError {}

/// Bounded language tag без locale parsing policy.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LanguageTag(String);

impl LanguageTag {
    /// Проверяет обязательность и named UTF-8 bound.
    pub fn new(value: impl Into<String>) -> Result<Self, CandidateDescriptorError> {
        let value = value.into();
        validate_descriptor_text(
            &value,
            DescriptorTextField::Language,
            MAX_LANGUAGE_TAG_UTF8_BYTES,
        )?;
        Ok(Self(value))
    }

    /// Возвращает exact language tag.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for LanguageTag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("LanguageTag").field(&self.0).finish()
    }
}

/// Bounded subtitle format identity.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubtitleFormatIdentity(String);

impl SubtitleFormatIdentity {
    /// Проверяет обязательность и named UTF-8 bound.
    pub fn new(value: impl Into<String>) -> Result<Self, CandidateDescriptorError> {
        let value = value.into();
        validate_descriptor_text(
            &value,
            DescriptorTextField::SubtitleFormat,
            MAX_SUBTITLE_FORMAT_IDENTITY_UTF8_BYTES,
        )?;
        Ok(Self(value))
    }

    /// Возвращает exact format identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SubtitleFormatIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubtitleFormatIdentity")
            .field("utf8_bytes", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// Нормализованный video track descriptor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VideoTrackDescriptor {
    /// Raw+parsed codec.
    codec: NormalizedCodec,
    /// Optional width; zero отбрасывается owner-ом mapping-а.
    width: Option<VideoWidth>,
    /// Checked height.
    height: Option<VideoHeight>,
    /// Exact rational frame rate.
    frame_rate: Option<FrameRate>,
    /// Optional positive bitrate.
    bitrate: Option<Bitrate>,
    /// Static dynamic-range hint.
    dynamic_range: DynamicRange,
}

impl VideoTrackDescriptor {
    /// Создаёт video descriptor из уже проверенных значений.
    pub const fn new(
        codec: NormalizedCodec,
        width: Option<VideoWidth>,
        height: Option<VideoHeight>,
        frame_rate: Option<FrameRate>,
        bitrate: Option<Bitrate>,
        dynamic_range: DynamicRange,
    ) -> Self {
        Self {
            codec,
            width,
            height,
            frame_rate,
            bitrate,
            dynamic_range,
        }
    }

    /// Возвращает codec identity.
    pub const fn codec(&self) -> &NormalizedCodec {
        &self.codec
    }

    /// Возвращает width.
    pub const fn width_pixels(&self) -> Option<u32> {
        match self.width {
            Some(width) => Some(width.pixels()),
            None => None,
        }
    }

    /// Возвращает height.
    pub const fn height(&self) -> Option<VideoHeight> {
        self.height
    }

    /// Возвращает frame rate.
    pub const fn frame_rate(&self) -> Option<FrameRate> {
        self.frame_rate
    }

    /// Возвращает bitrate.
    pub const fn bitrate(&self) -> Option<Bitrate> {
        match self.bitrate {
            Some(bitrate) => Some(bitrate),
            None => None,
        }
    }

    /// Возвращает dynamic-range hint.
    pub const fn dynamic_range(&self) -> DynamicRange {
        self.dynamic_range
    }
}

/// Нормализованный audio track descriptor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioTrackDescriptor {
    /// Raw+parsed codec.
    codec: NormalizedCodec,
    /// Optional sample rate.
    sample_rate: Option<SampleRate>,
    /// Optional channel count.
    channels: Option<ChannelCount>,
    /// Optional positive bitrate.
    bitrate: Option<Bitrate>,
    /// Optional language metadata.
    language: Option<LanguageTag>,
}

impl AudioTrackDescriptor {
    /// Создаёт audio descriptor из уже проверенных значений.
    pub const fn new(
        codec: NormalizedCodec,
        sample_rate: Option<SampleRate>,
        channels: Option<ChannelCount>,
        bitrate: Option<Bitrate>,
        language: Option<LanguageTag>,
    ) -> Self {
        Self {
            codec,
            sample_rate,
            channels,
            bitrate,
            language,
        }
    }

    /// Возвращает codec identity.
    pub const fn codec(&self) -> &NormalizedCodec {
        &self.codec
    }

    /// Возвращает sample rate.
    pub const fn sample_rate(&self) -> Option<SampleRate> {
        self.sample_rate
    }

    /// Возвращает channel count.
    pub const fn channels(&self) -> Option<ChannelCount> {
        self.channels
    }

    /// Возвращает bitrate.
    pub const fn bitrate(&self) -> Option<Bitrate> {
        match self.bitrate {
            Some(bitrate) => Some(bitrate),
            None => None,
        }
    }

    /// Возвращает language metadata.
    pub const fn language(&self) -> Option<&LanguageTag> {
        self.language.as_ref()
    }
}

/// Subtitle groundwork descriptor без playback/provider semantics.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubtitleDescriptor {
    /// Semantic identity дорожки.
    semantic_identity: SemanticIdentity,
    /// Bounded format identity.
    format: SubtitleFormatIdentity,
    /// Optional language.
    language: Option<LanguageTag>,
}

impl SubtitleDescriptor {
    /// Создаёт static subtitle descriptor.
    pub const fn new(
        semantic_identity: SemanticIdentity,
        format: SubtitleFormatIdentity,
        language: Option<LanguageTag>,
    ) -> Self {
        Self {
            semantic_identity,
            format,
            language,
        }
    }

    /// Возвращает semantic identity.
    pub const fn semantic_identity(&self) -> &SemanticIdentity {
        &self.semantic_identity
    }

    /// Возвращает format identity.
    pub const fn format(&self) -> &SubtitleFormatIdentity {
        &self.format
    }

    /// Возвращает language.
    pub const fn language(&self) -> Option<&LanguageTag> {
        self.language.as_ref()
    }
}

/// Полный immutable candidate descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateDescriptor {
    /// Snapshot-local exact identity.
    identity: CandidateIdentity,
    /// Refresh-stable semantic identity.
    semantic_identity: SemanticIdentity,
    /// Resource layout.
    layout: StreamLayout,
    /// Bounded subtitle groundwork.
    subtitles: Box<[SubtitleDescriptor]>,
}

impl CandidateDescriptor {
    /// Проверяет subtitle count и публикует immutable candidate.
    pub fn new(
        identity: CandidateIdentity,
        semantic_identity: SemanticIdentity,
        layout: StreamLayout,
        subtitles: Vec<SubtitleDescriptor>,
    ) -> Result<Self, CandidateDescriptorError> {
        if identity.source() != semantic_identity.source() {
            return Err(CandidateDescriptorError::SemanticSourceMismatch {
                exact_source: identity.source(),
                semantic_source: semantic_identity.source(),
            });
        }

        if subtitles.len() > MAX_SUBTITLE_DESCRIPTORS {
            return Err(CandidateDescriptorError::TooManySubtitles {
                provided: subtitles.len(),
                maximum: MAX_SUBTITLE_DESCRIPTORS,
            });
        }

        if let Some((ordinal, subtitle)) = subtitles
            .iter()
            .enumerate()
            .find(|(_, subtitle)| subtitle.semantic_identity().source() != identity.source())
        {
            return Err(CandidateDescriptorError::SubtitleSourceMismatch {
                ordinal,
                candidate_source: identity.source(),
                subtitle_source: subtitle.semantic_identity().source(),
            });
        }

        Ok(Self {
            identity,
            semantic_identity,
            layout,
            subtitles: subtitles.into_boxed_slice(),
        })
    }

    /// Возвращает snapshot-local identity.
    pub const fn identity(&self) -> &CandidateIdentity {
        &self.identity
    }

    /// Возвращает refresh-stable identity.
    pub const fn semantic_identity(&self) -> &SemanticIdentity {
        &self.semantic_identity
    }

    /// Возвращает layout.
    pub const fn layout(&self) -> &StreamLayout {
        &self.layout
    }

    /// Возвращает subtitles.
    pub const fn subtitles(&self) -> &[SubtitleDescriptor] {
        &self.subtitles
    }
}

/// Текстовое поле descriptor-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptorTextField {
    /// Language tag.
    Language,
    /// Subtitle format identity.
    SubtitleFormat,
}

/// Ошибка построения static candidate descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateDescriptorError {
    /// Exact и semantic identities принадлежат разным source lineages.
    SemanticSourceMismatch {
        /// Source snapshot-local identity.
        exact_source: crate::SourceIdentity,
        /// Source semantic identity.
        semantic_source: crate::SourceIdentity,
    },
    /// Subtitle descriptor принадлежит другой source lineage.
    SubtitleSourceMismatch {
        /// Нулевая позиция subtitle в candidate descriptor.
        ordinal: usize,
        /// Source candidate-а.
        candidate_source: crate::SourceIdentity,
        /// Source subtitle identity.
        subtitle_source: crate::SourceIdentity,
    },
    /// Обязательный текст пуст.
    EmptyText {
        /// Безопасное имя поля.
        field: DescriptorTextField,
    },
    /// Текст превысил named UTF-8 byte bound.
    TextTooLong {
        /// Безопасное имя поля.
        field: DescriptorTextField,
        /// Фактическая длина.
        provided_bytes: usize,
        /// Разрешённая длина.
        maximum_bytes: usize,
    },
    /// Subtitle list превысил candidate-level bound.
    TooManySubtitles {
        /// Фактическое число descriptors.
        provided: usize,
        /// Разрешённое число descriptors.
        maximum: usize,
    },
}

impl fmt::Display for CandidateDescriptorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SemanticSourceMismatch {
                exact_source,
                semantic_source,
            } => write!(
                formatter,
                "exact source {exact_source:?} не совпадает с semantic source {semantic_source:?}"
            ),
            Self::SubtitleSourceMismatch {
                ordinal,
                candidate_source,
                subtitle_source,
            } => write!(
                formatter,
                "subtitle #{ordinal} source {subtitle_source:?} не совпадает с candidate source {candidate_source:?}"
            ),
            Self::EmptyText { field } => write!(formatter, "{field:?} не может быть пустым"),
            Self::TextTooLong {
                field,
                provided_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "{field:?} занимает {provided_bytes} bytes при лимите {maximum_bytes}"
            ),
            Self::TooManySubtitles { provided, maximum } => write!(
                formatter,
                "candidate содержит {provided} subtitles при лимите {maximum}"
            ),
        }
    }
}

impl std::error::Error for CandidateDescriptorError {}

/// Проверяет обязательность и UTF-8 byte bound descriptor text.
fn validate_descriptor_text(
    value: &str,
    field: DescriptorTextField,
    maximum_bytes: usize,
) -> Result<(), CandidateDescriptorError> {
    if value.is_empty() {
        return Err(CandidateDescriptorError::EmptyText { field });
    }

    if value.len() > maximum_bytes {
        return Err(CandidateDescriptorError::TextTooLong {
            field,
            provided_bytes: value.len(),
            maximum_bytes,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests;
