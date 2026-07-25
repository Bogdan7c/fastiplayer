//! Public intent model и обязательные write limits.

use std::fmt;
use std::num::{NonZeroU16, NonZeroU32, NonZeroUsize};

use super::error::{
    FragmentInitializationError, FragmentInitializationField,
    FragmentInitializationLimitBuildError, FragmentInitializationLimitKind,
};
use super::validate::{
    validate_aac_lc_configuration, validate_aac_specific_config, validate_h264_parameter_set,
};
use crate::fragment_reconstruction::FragmentTrackId;

/// Media timescale одного track-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentTimescale(NonZeroU32);

impl FragmentTimescale {
    /// Создаёт заранее проверенный ненулевой timescale.
    pub const fn new(ticks_per_second: NonZeroU32) -> Self {
        Self(ticks_per_second)
    }

    /// Возвращает ticks per second.
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// Ширина video sample entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentVideoWidth(NonZeroU16);

impl FragmentVideoWidth {
    /// Проверяет ненулевое значение и 16-bit ISO field.
    pub fn try_new(value: u32) -> Result<Self, FragmentInitializationError> {
        nonzero_u16(value, FragmentInitializationField::VideoWidth).map(Self)
    }

    /// Возвращает число coded pixels.
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

/// Высота video sample entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentVideoHeight(NonZeroU16);

impl FragmentVideoHeight {
    /// Проверяет ненулевое значение и 16-bit ISO field.
    pub fn try_new(value: u32) -> Result<Self, FragmentInitializationError> {
        nonzero_u16(value, FragmentInitializationField::VideoHeight).map(Self)
    }

    /// Возвращает число coded pixels.
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

/// Coded размеры H.264 track-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentVideoDimensions {
    width: FragmentVideoWidth,
    height: FragmentVideoHeight,
}

impl FragmentVideoDimensions {
    /// Группирует typed width/height без positional primitive-ов.
    pub const fn new(width: FragmentVideoWidth, height: FragmentVideoHeight) -> Self {
        Self { width, height }
    }

    /// Возвращает coded width.
    pub const fn width(self) -> FragmentVideoWidth {
        self.width
    }

    /// Возвращает coded height.
    pub const fn height(self) -> FragmentVideoHeight {
        self.height
    }
}

/// Проверенный чистый H.264 SPS NAL unit без Annex-B start code.
#[derive(Clone, Copy)]
pub struct FragmentH264SequenceParameterSet<'codec>(&'codec [u8]);

impl<'codec> FragmentH264SequenceParameterSet<'codec> {
    /// Валидирует ровно один SPS NAL unit.
    pub fn try_new(bytes: &'codec [u8]) -> Result<Self, FragmentInitializationError> {
        validate_h264_parameter_set(bytes, 7, true)?;
        Ok(Self(bytes))
    }

    /// Возвращает доказанные SPS bytes.
    pub const fn as_bytes(self) -> &'codec [u8] {
        self.0
    }
}

impl fmt::Debug for FragmentH264SequenceParameterSet<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FragmentH264SequenceParameterSet")
            .field("byte_count", &self.0.len())
            .finish()
    }
}

/// Проверенный чистый H.264 PPS NAL unit без Annex-B start code.
#[derive(Clone, Copy)]
pub struct FragmentH264PictureParameterSet<'codec>(&'codec [u8]);

impl<'codec> FragmentH264PictureParameterSet<'codec> {
    /// Валидирует ровно один PPS NAL unit.
    pub fn try_new(bytes: &'codec [u8]) -> Result<Self, FragmentInitializationError> {
        validate_h264_parameter_set(bytes, 8, false)?;
        Ok(Self(bytes))
    }

    /// Возвращает доказанные PPS bytes.
    pub const fn as_bytes(self) -> &'codec [u8] {
        self.0
    }
}

impl fmt::Debug for FragmentH264PictureParameterSet<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FragmentH264PictureParameterSet")
            .field("byte_count", &self.0.len())
            .finish()
    }
}

/// Полная H.264 `avc1` initialization configuration.
#[derive(Clone, Copy, Debug)]
pub struct FragmentH264Configuration<'codec> {
    dimensions: FragmentVideoDimensions,
    sequence_parameter_set: FragmentH264SequenceParameterSet<'codec>,
    picture_parameter_set: FragmentH264PictureParameterSet<'codec>,
}

impl<'codec> FragmentH264Configuration<'codec> {
    /// Связывает typed dimensions с точными одним SPS и одним PPS.
    pub const fn new(
        dimensions: FragmentVideoDimensions,
        sequence_parameter_set: FragmentH264SequenceParameterSet<'codec>,
        picture_parameter_set: FragmentH264PictureParameterSet<'codec>,
    ) -> Self {
        Self {
            dimensions,
            sequence_parameter_set,
            picture_parameter_set,
        }
    }

    pub(super) const fn dimensions(self) -> FragmentVideoDimensions {
        self.dimensions
    }

    pub(super) const fn sequence_parameter_set(self) -> FragmentH264SequenceParameterSet<'codec> {
        self.sequence_parameter_set
    }

    pub(super) const fn picture_parameter_set(self) -> FragmentH264PictureParameterSet<'codec> {
        self.picture_parameter_set
    }
}

/// Sample rate, представимая одновременно в `mdhd` и audio sample entry 16.16.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentAacSampleRate(NonZeroU16);

impl FragmentAacSampleRate {
    /// Проверяет ненулевое значение и ширину 16.16 integer part.
    pub fn try_new(value: u32) -> Result<Self, FragmentInitializationError> {
        nonzero_u16(value, FragmentInitializationField::AudioSampleRate).map(Self)
    }

    /// Возвращает Hz.
    pub const fn get(self) -> u32 {
        self.0.get() as u32
    }
}

/// Число AAC output channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentAacChannelCount(NonZeroU16);

impl FragmentAacChannelCount {
    /// Проверяет ненулевое число и ширину MP4 audio sample entry.
    pub fn try_new(value: u32) -> Result<Self, FragmentInitializationError> {
        nonzero_u16(value, FragmentInitializationField::AudioChannelCount).map(Self)
    }

    /// Возвращает число channels.
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

/// Точный проверенный AAC-LC `AudioSpecificConfig`.
#[derive(Clone, Copy)]
pub struct FragmentAacAudioSpecificConfig<'codec>(&'codec [u8]);

impl<'codec> FragmentAacAudioSpecificConfig<'codec> {
    /// Принимает только узкий двухбайтовый AAC-LC ASC без HE-AAC extensions.
    pub fn try_new(bytes: &'codec [u8]) -> Result<Self, FragmentInitializationError> {
        validate_aac_specific_config(bytes)?;
        Ok(Self(bytes))
    }

    /// Возвращает точные ASC bytes.
    pub const fn as_bytes(self) -> &'codec [u8] {
        self.0
    }
}

impl fmt::Debug for FragmentAacAudioSpecificConfig<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FragmentAacAudioSpecificConfig")
            .field("byte_count", &self.0.len())
            .finish()
    }
}

/// Полная AAC-LC `mp4a` initialization configuration.
#[derive(Clone, Copy, Debug)]
pub struct FragmentAacLcConfiguration<'codec> {
    sample_rate: FragmentAacSampleRate,
    channel_count: FragmentAacChannelCount,
    audio_specific_config: FragmentAacAudioSpecificConfig<'codec>,
}

impl<'codec> FragmentAacLcConfiguration<'codec> {
    /// Проверяет согласованность typed metadata с ASC.
    pub fn try_new(
        sample_rate: FragmentAacSampleRate,
        channel_count: FragmentAacChannelCount,
        audio_specific_config: FragmentAacAudioSpecificConfig<'codec>,
    ) -> Result<Self, FragmentInitializationError> {
        validate_aac_lc_configuration(sample_rate, channel_count, audio_specific_config)?;
        Ok(Self {
            sample_rate,
            channel_count,
            audio_specific_config,
        })
    }

    pub(super) const fn sample_rate(self) -> FragmentAacSampleRate {
        self.sample_rate
    }

    pub(super) const fn channel_count(self) -> FragmentAacChannelCount {
        self.channel_count
    }

    pub(super) const fn audio_specific_config(self) -> FragmentAacAudioSpecificConfig<'codec> {
        self.audio_specific_config
    }
}

/// Взаимоисключающие codec-specific initialization intents.
#[derive(Clone, Copy, Debug)]
pub enum FragmentInitializationCodec<'codec> {
    /// H.264 `avc1` с четырёхбайтовыми length prefixes.
    H264Avc1(FragmentH264Configuration<'codec>),
    /// AAC Low Complexity `mp4a.40.2`.
    AacLowComplexity(FragmentAacLcConfiguration<'codec>),
}

/// Обязательные write limits без скрытых defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FragmentInitializationLimits {
    maximum_output_bytes: NonZeroUsize,
    maximum_codec_configuration_bytes: NonZeroUsize,
}

impl FragmentInitializationLimits {
    /// Начинает builder без скрытых budget-ов.
    pub const fn builder() -> FragmentInitializationLimitsBuilder {
        FragmentInitializationLimitsBuilder::new()
    }

    /// Возвращает предел готового `ftyp + moov`.
    pub const fn maximum_output_bytes(&self) -> usize {
        self.maximum_output_bytes.get()
    }

    /// Возвращает предел caller-provided codec bytes.
    pub const fn maximum_codec_configuration_bytes(&self) -> usize {
        self.maximum_codec_configuration_bytes.get()
    }
}

/// Builder обязательных initialization limits.
#[derive(Clone, Debug, Default)]
pub struct FragmentInitializationLimitsBuilder {
    maximum_output_bytes: Option<usize>,
    maximum_codec_configuration_bytes: Option<usize>,
}

impl FragmentInitializationLimitsBuilder {
    /// Создаёт пустой builder.
    pub const fn new() -> Self {
        Self {
            maximum_output_bytes: None,
            maximum_codec_configuration_bytes: None,
        }
    }

    /// Задаёт максимальный размер готового segment-а.
    pub const fn maximum_output_bytes(mut self, maximum: usize) -> Self {
        self.maximum_output_bytes = Some(maximum);
        self
    }

    /// Задаёт максимальную сумму SPS+PPS либо ASC.
    pub const fn maximum_codec_configuration_bytes(mut self, maximum: usize) -> Self {
        self.maximum_codec_configuration_bytes = Some(maximum);
        self
    }

    /// Проверяет полноту и ненулевую семантику limits.
    pub fn build(
        self,
    ) -> Result<FragmentInitializationLimits, FragmentInitializationLimitBuildError> {
        Ok(FragmentInitializationLimits {
            maximum_output_bytes: required_limit(
                self.maximum_output_bytes,
                FragmentInitializationLimitKind::OutputBytes,
            )?,
            maximum_codec_configuration_bytes: required_limit(
                self.maximum_codec_configuration_bytes,
                FragmentInitializationLimitKind::CodecConfigurationBytes,
            )?,
        })
    }
}

/// Полный immutable запрос на один initialization segment.
pub struct FragmentInitializationRequest<'codec, 'policy> {
    track_id: FragmentTrackId,
    timescale: FragmentTimescale,
    codec: FragmentInitializationCodec<'codec>,
    limits: &'policy FragmentInitializationLimits,
    cancellation: &'policy dyn Fn() -> bool,
}

impl<'codec, 'policy> FragmentInitializationRequest<'codec, 'policy> {
    /// Создаёт запрос без manifest/runtime state и без скрытых defaults.
    pub const fn new(
        track_id: FragmentTrackId,
        timescale: FragmentTimescale,
        codec: FragmentInitializationCodec<'codec>,
        limits: &'policy FragmentInitializationLimits,
        cancellation: &'policy dyn Fn() -> bool,
    ) -> Self {
        Self {
            track_id,
            timescale,
            codec,
            limits,
            cancellation,
        }
    }

    pub(super) const fn track_id(&self) -> FragmentTrackId {
        self.track_id
    }

    pub(super) const fn timescale(&self) -> FragmentTimescale {
        self.timescale
    }

    pub(super) const fn codec(&self) -> FragmentInitializationCodec<'codec> {
        self.codec
    }

    pub(super) const fn limits(&self) -> &FragmentInitializationLimits {
        self.limits
    }

    pub(super) fn is_cancelled(&self) -> bool {
        (self.cancellation)()
    }
}

/// Отдельный готовый `ftyp + moov`, не содержащий media bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct FragmentInitializationSegment {
    bytes: Vec<u8>,
}

impl FragmentInitializationSegment {
    pub(super) const fn verified(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Заимствует готовые initialization bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Передаёт владение готовыми initialization bytes caller-у.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl fmt::Debug for FragmentInitializationSegment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FragmentInitializationSegment")
            .field("byte_count", &self.bytes.len())
            .finish()
    }
}

fn nonzero_u16(
    value: u32,
    field: FragmentInitializationField,
) -> Result<NonZeroU16, FragmentInitializationError> {
    if value == 0 {
        return Err(FragmentInitializationError::InvalidField { field });
    }
    let narrowed =
        u16::try_from(value).map_err(|_| FragmentInitializationError::FieldOverflow {
            field,
            value: u64::from(value),
        })?;
    NonZeroU16::new(narrowed).ok_or(FragmentInitializationError::InvalidField { field })
}

fn required_limit(
    value: Option<usize>,
    kind: FragmentInitializationLimitKind,
) -> Result<NonZeroUsize, FragmentInitializationLimitBuildError> {
    match value {
        None => Err(FragmentInitializationLimitBuildError::Missing { kind }),
        Some(0) => Err(FragmentInitializationLimitBuildError::Zero { kind }),
        Some(value) => {
            NonZeroUsize::new(value).ok_or(FragmentInitializationLimitBuildError::Zero { kind })
        }
    }
}
