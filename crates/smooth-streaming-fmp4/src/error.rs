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
    #[error("Smooth track mapping cancelled")]
    Cancelled,
    /// Stream ordinal отсутствует в sealed manifest.
    #[error("The selected Smooth stream is missing")]
    StreamNotFound,
    /// Quality index отсутствует в выбранном stream.
    #[error("Smooth quality is not selected")]
    QualityNotFound,
    /// Stream timescale не помещается в обязательное F1 поле.
    #[error("Smooth stream timescale does not fit in fMP4 timescale")]
    TimescaleOutOfRange,
    /// H.264 private data не является canonical парой SPS/PPS.
    #[error("Smooth H.264 codec configuration is not a canonical SPS/PPS pair")]
    InvalidH264Configuration,
    /// Manifest codec fields не проходят точную F1 init-валидацию.
    #[error("Smooth codec fields are not compatible with fMP4 init contract")]
    InitializationContract(#[source] FragmentInitializationError),
}

/// Ошибка сборки Smooth initialization segment.
#[derive(Debug, Error)]
pub enum SmoothInitializationError {
    /// Caller отменил стадию до или внутри F1 boundary.
    #[error("Smooth initialization segment is cancelled")]
    Cancelled,
    /// F1 отверг mapped init request.
    #[error("failed to assemble Smooth initialization segment")]
    Contract(#[source] FragmentInitializationError),
}

/// Ошибка sealed fragment planning-а.
#[derive(Debug, Error)]
pub enum SmoothFragmentPlanError {
    /// Caller отменил planning до публикации plan-а.
    #[error("Smooth fragment planning cancelled")]
    Cancelled,
    /// Fragment index отсутствует в compact manifest timeline.
    #[error("The selected Smooth fragment is missing.")]
    FragmentNotFound,
    /// Manifest interval переполняет `u64`.
    #[error("Smooth manifest fragment interval is full")]
    WindowOverflow,
    /// Validated compact timeline не смог материализовать fragment.
    #[error("Smooth manifest fragment failed to materialize")]
    Timeline(#[source] SmoothManifestError),
    /// Sealed manifest template не смог построить относительный путь.
    #[error("The Smooth fragment path was not displayed.")]
    PathRendering(#[source] SmoothManifestError),
}

/// Ошибка reconstruction или строгой admission-классификации.
#[derive(Debug, Error)]
pub enum SmoothFragmentReconstructionError {
    /// Caller отменил работу до публикации результата.
    #[error("Smooth fragment reconstruction cancelled")]
    Cancelled,
    /// F1 inspection обнаружил start mismatch; наружу не протекает F1 vocabulary.
    #[error("The coded fragment does not start manifestly.")]
    StartMismatch {
        /// Авторитетный manifest start.
        expected_start: u64,
        /// Фактический coded start.
        actual_start: u64,
    },
    /// F1 inspection завершился typed structural ошибкой.
    #[error("Smooth fragment inspection rejected")]
    Inspection(#[source] FragmentInspectionError),
    /// F1 writer завершился typed ошибкой.
    #[error("Smooth fragment canonical reconstruction not completed")]
    Writing(#[source] FragmentWriteError),
    /// Coded interval короче manifest window.
    #[error("coded fragment does not cover the manifest window")]
    Underrun {
        /// Число отсутствующих ticks в одном stream clock.
        missing_ticks: core::num::NonZeroU64,
    },
    /// Video overhang нельзя маскировать audio clipping policy.
    #[error("video fragment extends beyond the manifest window")]
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
