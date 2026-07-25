//! Типизированные ошибки initialization-segment boundary.

use std::fmt;

/// Codec family узкого initialization profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentCodecKind {
    /// Four-byte length-prefixed H.264 в `avc1`.
    H264Avc1,
    /// AAC Low Complexity в `mp4a`.
    AacLowComplexity,
}

/// Конкретная проблема codec configuration без публикации raw bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentCodecConfigurationIssue {
    /// Codec configuration отсутствует.
    Empty,
    /// H.264 NAL содержит Annex-B start code вместо чистого NAL unit-а.
    AnnexBStartCode,
    /// H.264 forbidden_zero_bit нарушает NAL header contract.
    H264ForbiddenZeroBit,
    /// SPS слишком короток для profile/compatibility/level fields.
    TruncatedSequenceParameterSet,
    /// PPS слишком короток для минимального NAL payload.
    TruncatedPictureParameterSet,
    /// NAL unit имеет неправильный H.264 type.
    UnexpectedNalUnitType {
        /// Ожидаемый NAL unit type.
        expected: u8,
        /// Фактический NAL unit type.
        actual: u8,
    },
    /// Parameter set не помещается в 16-bit `avcC` length.
    ParameterSetTooLarge,
    /// AAC `AudioSpecificConfig` не имеет точного поддержанного размера.
    InvalidAudioSpecificConfigLength,
    /// AAC object type не является AAC-LC.
    UnsupportedAacObjectType {
        /// Фактический MPEG-4 Audio Object Type.
        actual: u8,
    },
    /// Explicit/escape sampling-frequency form не входит в узкий профиль.
    UnsupportedAacSamplingFrequency,
    /// AAC channel configuration отсутствует либо неизвестна.
    UnsupportedAacChannelConfiguration {
        /// Фактический indexed channel configuration.
        actual: u8,
    },
    /// Объявленная sample rate противоречит `AudioSpecificConfig`.
    AacSampleRateMismatch,
    /// Объявленное число каналов противоречит `AudioSpecificConfig`.
    AacChannelCountMismatch,
    /// Неподдерживаемые AAC-LC extension flags или trailing fields.
    UnsupportedAacExtension,
}

/// Поле с фиксированной ISO BMFF шириной.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentInitializationField {
    /// Ширина video sample entry.
    VideoWidth,
    /// Высота video sample entry.
    VideoHeight,
    /// Sample rate в 16.16 audio sample entry.
    AudioSampleRate,
    /// Число audio channels.
    AudioChannelCount,
    /// `mvhd.next_track_ID`.
    NextTrackId,
}

/// Box, размер которого обязан помещаться в 32-bit header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentBoxType {
    FileType,
    Movie,
    Track,
    Media,
    MediaInformation,
    SampleTable,
    SampleDescription,
    AvcSampleEntry,
    AvcConfiguration,
    AacSampleEntry,
    ElementaryStreamDescriptor,
    MovieExtends,
}

/// Обязательный write budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentInitializationLimitKind {
    /// Максимальный размер опубликованного `ftyp + moov`.
    OutputBytes,
    /// Максимальная сумма caller-provided codec configuration bytes.
    CodecConfigurationBytes,
}

/// Ошибка сборки набора обязательных write limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentInitializationLimitBuildError {
    /// Caller не задал обязательный budget.
    Missing {
        /// Отсутствующий budget.
        kind: FragmentInitializationLimitKind,
    },
    /// Нулевой budget не допускает осмысленной работы.
    Zero {
        /// Нулевой budget.
        kind: FragmentInitializationLimitKind,
    },
}

/// Полный public error boundary initialization builder-а.
#[derive(Debug)]
pub enum FragmentInitializationError {
    /// Caller отменил planning либо публикацию.
    Cancelled,
    /// Обязательный budget исчерпан до allocation/publication.
    LimitExceeded {
        /// Исчерпанный budget.
        kind: FragmentInitializationLimitKind,
        /// Настроенный предел.
        limit: u64,
        /// Проверенное требование.
        observed: u64,
    },
    /// Caller передал структурно недопустимую codec configuration.
    InvalidCodecConfiguration {
        /// Codec family.
        codec: FragmentCodecKind,
        /// Безопасная причина.
        issue: FragmentCodecConfigurationIssue,
    },
    /// Codec bytes противоречат typed metadata caller-а.
    IncompatibleCodecConfiguration {
        /// Codec family.
        codec: FragmentCodecKind,
        /// Безопасная причина.
        issue: FragmentCodecConfigurationIssue,
    },
    /// Нулевое значение невозможно выразить semantic typed field-ом.
    InvalidField {
        /// Недопустимое поле.
        field: FragmentInitializationField,
    },
    /// Значение не помещается в ISO BMFF field.
    FieldOverflow {
        /// Переполненное поле.
        field: FragmentInitializationField,
        /// Исходное caller-visible значение.
        value: u64,
    },
    /// Planned box не помещается в 32-bit box header.
    BoxSizeOverflow {
        /// Переполненный box.
        box_type: FragmentBoxType,
        /// Полный вычисленный размер.
        size: u64,
    },
    /// Checked сложение размеров переполнило адресное пространство.
    SizeArithmeticOverflow,
    /// Единственная planned allocation не была предоставлена allocator-ом.
    AllocationFailed,
    /// Внутренний writer не совпал с предварительно доказанным планом.
    SerializationInvariantViolated,
}

impl PartialEq for FragmentInitializationError {
    fn eq(&self, other: &Self) -> bool {
        use FragmentInitializationError as Error;

        match (self, other) {
            (Error::Cancelled, Error::Cancelled)
            | (Error::SizeArithmeticOverflow, Error::SizeArithmeticOverflow)
            | (Error::AllocationFailed, Error::AllocationFailed)
            | (Error::SerializationInvariantViolated, Error::SerializationInvariantViolated) => {
                true
            }
            (
                Error::LimitExceeded {
                    kind: left_kind,
                    limit: left_limit,
                    observed: left_observed,
                },
                Error::LimitExceeded {
                    kind: right_kind,
                    limit: right_limit,
                    observed: right_observed,
                },
            ) => {
                left_kind == right_kind
                    && left_limit == right_limit
                    && left_observed == right_observed
            }
            (
                Error::InvalidCodecConfiguration {
                    codec: left_codec,
                    issue: left_issue,
                },
                Error::InvalidCodecConfiguration {
                    codec: right_codec,
                    issue: right_issue,
                },
            )
            | (
                Error::IncompatibleCodecConfiguration {
                    codec: left_codec,
                    issue: left_issue,
                },
                Error::IncompatibleCodecConfiguration {
                    codec: right_codec,
                    issue: right_issue,
                },
            ) => left_codec == right_codec && left_issue == right_issue,
            (Error::InvalidField { field: left }, Error::InvalidField { field: right }) => {
                left == right
            }
            (
                Error::FieldOverflow {
                    field: left_field,
                    value: left_value,
                },
                Error::FieldOverflow {
                    field: right_field,
                    value: right_value,
                },
            ) => left_field == right_field && left_value == right_value,
            (
                Error::BoxSizeOverflow {
                    box_type: left_type,
                    size: left_size,
                },
                Error::BoxSizeOverflow {
                    box_type: right_type,
                    size: right_size,
                },
            ) => left_type == right_type && left_size == right_size,
            _ => false,
        }
    }
}

impl Eq for FragmentInitializationError {}

impl fmt::Display for FragmentInitializationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "fragmented MP4 initialization build failed: {self:?}"
        )
    }
}

impl std::error::Error for FragmentInitializationError {}
