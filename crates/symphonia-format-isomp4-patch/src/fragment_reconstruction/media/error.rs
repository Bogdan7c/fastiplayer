//! Public typed errors canonical media reconstruction-а.

use std::fmt;

use super::super::error::FragmentInspectionError;

/// Ошибка обязательного write budget-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentWriteLimitBuildError {
    /// Нулевой output budget не разрешает безопасную публикацию.
    ZeroMaximumOutputBytes,
}

impl fmt::Display for FragmentWriteLimitBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid fragment write limits")
    }
}

impl std::error::Error for FragmentWriteLimitBuildError {}

/// Фаза writer-а, на которой caller отменил работу.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentWriteCancellationPhase {
    /// До вычисления canonical layout.
    Planning,
    /// Во время bounded sample-table work.
    SampleTable,
    /// До первого копирования media payload.
    BeforeMediaPayload,
    /// После полной проверки bytes, но до публикации результата.
    BeforePublication,
}

/// Canonical box, размер которого нельзя представить.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentMediaBoxType {
    /// Верхний `moof`.
    MovieFragment,
    /// Единственный `traf`.
    TrackFragment,
    /// Единственный `trun`.
    TrackFragmentRun,
    /// Единственный `mdat`.
    MediaData,
}

/// Checked arithmetic operation canonical writer-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentWriteArithmeticOperation {
    /// Число samples.
    SampleCount,
    /// Размер canonical sample table.
    SampleTableSize,
    /// Суммарный media payload.
    MediaPayloadSize,
    /// Сложение размеров boxes.
    BoxSize,
    /// Выбор и запись `tfdt.baseMediaDecodeTime`.
    DecodeTime,
    /// Signed `trun.data_offset`.
    DataOffset,
    /// Полный output buffer.
    OutputSize,
}

/// Ошибка canonical media writer-а после успешного inspection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FragmentWriteError {
    /// Caller отменил write phase.
    Cancelled {
        /// Точная фаза без раскрытия media bytes.
        phase: FragmentWriteCancellationPhase,
    },
    /// Canonical output не помещается в mandatory budget.
    OutputLimitExceeded {
        /// Настроенный предел.
        limit: u64,
        /// Полный заранее вычисленный размер.
        required: u64,
    },
    /// Allocator не смог заранее зарезервировать уже проверенный output.
    AllocationFailed {
        /// Размер requested capacity после output-limit проверки.
        requested: u64,
    },
    /// Video sample не имеет effective flags, которые writer обязан сохранить.
    MissingVideoSampleFlags {
        /// Индекс sample-а во fragment-е.
        sample_index: u32,
    },
    /// Audio flags нельзя сохранить одним canonical `trun`, когда presence смешана.
    AudioSampleFlagsNotUniform {
        /// Индекс первого sample-а с отличающейся presence.
        sample_index: u32,
    },
    /// Один canonical `trun` не может без потери представить CTO.
    CompositionOffsetUnrepresentable {
        /// Индекс первого непредставимого sample-а.
        sample_index: u32,
        /// Exact signed offset из F1A plan.
        offset: i64,
    },
    /// Checked arithmetic обнаружила переполнение.
    ArithmeticOverflow {
        /// Операция без raw payload.
        operation: FragmentWriteArithmeticOperation,
    },
    /// Размер canonical box не помещается в 32-bit header profile.
    BoxSizeUnrepresentable {
        /// Box, чей размер не представим.
        box_type: FragmentMediaBoxType,
        /// Вычисленный размер.
        size: u64,
    },
    /// `trun.data_offset` не помещается в signed 32-bit поле.
    DataOffsetUnrepresentable {
        /// Exact положительный offset от начала `moof`.
        offset: u64,
    },
    /// Verified sample ranges неожиданно не совпали с `mdat` payload.
    MediaPayloadLengthMismatch {
        /// Размер, доказанный F1A для `mdat`.
        expected: u64,
        /// Сумма normalized sample ranges.
        actual: u64,
    },
}

/// Верхнеуровневая ошибка сохраняет границу inspection/write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FragmentReconstructionError {
    /// Недоверенный input не прошёл F1A inspection.
    Inspection(FragmentInspectionError),
    /// Verified plan нельзя безопасно сериализовать в canonical profile.
    Writing(FragmentWriteError),
}

impl fmt::Display for FragmentWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllocationFailed { .. } => {
                write!(formatter, "canonical fragment media allocation failed")
            }
            _ => write!(formatter, "canonical fragment media write failed"),
        }
    }
}

impl std::error::Error for FragmentWriteError {}

impl fmt::Display for FragmentReconstructionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inspection(_) => write!(formatter, "fragment inspection failed"),
            Self::Writing(_) => write!(formatter, "fragment media writing failed"),
        }
    }
}

impl std::error::Error for FragmentReconstructionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Inspection(error) => Some(error),
            Self::Writing(error) => Some(error),
        }
    }
}

impl From<FragmentInspectionError> for FragmentReconstructionError {
    fn from(error: FragmentInspectionError) -> Self {
        Self::Inspection(error)
    }
}

impl From<FragmentWriteError> for FragmentReconstructionError {
    fn from(error: FragmentWriteError) -> Self {
        Self::Writing(error)
    }
}
