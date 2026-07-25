//! Typed и redacted ошибки отдельных стадий Smooth fMP4 adapter-а.

use smooth_streaming_manifest_core::SmoothManifestError;
use symphonia_format_isomp4::{
    FragmentInitializationError, FragmentInspectionError, FragmentReconstructionError,
    FragmentWriteError,
};
use thiserror::Error;

/// Ошибка выбора stream/quality или точного codec mapping-а.
#[derive(Debug, Error)]
pub enum SmoothTrackMappingError {
    /// Caller отменил mapping до публикации mapped track-а.
    #[error("Smooth track mapping отменён")]
    Cancelled,
    /// Stream ordinal отсутствует в sealed manifest.
    #[error("выбранный Smooth stream отсутствует")]
    StreamNotFound,
    /// Quality index отсутствует в выбранном stream.
    #[error("выбранный Smooth quality отсутствует")]
    QualityNotFound,
    /// Stream timescale не помещается в обязательное F1 поле.
    #[error("Smooth stream timescale не помещается в fMP4 timescale")]
    TimescaleOutOfRange,
    /// H.264 private data не является canonical парой SPS/PPS.
    #[error("Smooth H.264 codec configuration не является canonical SPS/PPS парой")]
    InvalidH264Configuration,
    /// Manifest codec fields не проходят точную F1 init-валидацию.
    #[error("Smooth codec fields несовместимы с fMP4 init contract")]
    InitializationContract(#[source] FragmentInitializationError),
}

/// Ошибка сборки Smooth initialization segment.
#[derive(Debug, Error)]
pub enum SmoothInitializationError {
    /// Caller отменил стадию до или внутри F1 boundary.
    #[error("сборка Smooth initialization segment отменена")]
    Cancelled,
    /// F1 отверг mapped init request.
    #[error("не удалось собрать Smooth initialization segment")]
    Contract(#[source] FragmentInitializationError),
}

/// Ошибка sealed fragment planning-а.
#[derive(Debug, Error)]
pub enum SmoothFragmentPlanError {
    /// Caller отменил planning до публикации plan-а.
    #[error("Smooth fragment planning отменён")]
    Cancelled,
    /// Fragment index отсутствует в compact manifest timeline.
    #[error("выбранный Smooth fragment отсутствует")]
    FragmentNotFound,
    /// Manifest interval переполняет `u64`.
    #[error("Smooth manifest fragment interval переполнен")]
    WindowOverflow,
    /// Validated compact timeline не смог материализовать fragment.
    #[error("не удалось материализовать Smooth manifest fragment")]
    Timeline(#[source] SmoothManifestError),
    /// Sealed manifest template не смог построить относительный путь.
    #[error("не удалось отобразить Smooth fragment path")]
    PathRendering(#[source] SmoothManifestError),
}

/// Ошибка reconstruction или строгой admission-классификации.
#[derive(Debug, Error)]
pub enum SmoothFragmentReconstructionError {
    /// Caller отменил работу до публикации результата.
    #[error("Smooth fragment reconstruction отменён")]
    Cancelled,
    /// F1 inspection обнаружил start mismatch; наружу не протекает F1 vocabulary.
    #[error("coded fragment начинается не в manifest start")]
    StartMismatch {
        /// Авторитетный manifest start.
        expected_start: u64,
        /// Фактический coded start.
        actual_start: u64,
    },
    /// F1 inspection завершился typed structural ошибкой.
    #[error("Smooth fragment inspection отклонён")]
    Inspection(#[source] FragmentInspectionError),
    /// F1 writer завершился typed ошибкой.
    #[error("Smooth fragment canonical reconstruction не выполнен")]
    Writing(#[source] FragmentWriteError),
    /// Coded interval короче manifest window.
    #[error("coded fragment не покрывает manifest window")]
    Underrun {
        /// Число отсутствующих ticks в одном stream clock.
        missing_ticks: core::num::NonZeroU64,
    },
    /// Video overhang нельзя маскировать audio clipping policy.
    #[error("video fragment выходит за manifest window")]
    VideoOverhang {
        /// Число лишних ticks в одном stream clock.
        excess_ticks: core::num::NonZeroU64,
    },
}

impl SmoothFragmentReconstructionError {
    /// Нормализует F1 error, сохраняя typed inspection/write различие.
    pub(crate) fn from_f1(error: FragmentReconstructionError) -> Self {
        match error {
            FragmentReconstructionError::Inspection(FragmentInspectionError::TimingConflict {
                expected_base_decode_time,
                actual_base_decode_time,
            }) => Self::StartMismatch {
                expected_start: expected_base_decode_time,
                actual_start: actual_base_decode_time,
            },
            FragmentReconstructionError::Inspection(FragmentInspectionError::Cancelled)
            | FragmentReconstructionError::Writing(FragmentWriteError::Cancelled { .. }) => {
                Self::Cancelled
            }
            FragmentReconstructionError::Inspection(error) => Self::Inspection(error),
            FragmentReconstructionError::Writing(error) => Self::Writing(error),
        }
    }
}
