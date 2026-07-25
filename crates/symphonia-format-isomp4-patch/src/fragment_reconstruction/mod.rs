//! Ограниченная реконструкция fragmented ISO BMFF.
//!
//! Модуль владеет тремя раздельными границами: crate-private inspection недоверенного media
//! fragment-а, public opt-in canonical media reconstruction и public opt-in builder отдельного
//! initialization segment-а. Ни одна граница не открывает provider/runtime и не меняет
//! существующий demux API.

mod atom;
mod budget;
mod error;
mod initialization;
mod inspect;
mod limits;
mod media;
mod model;
mod normalize;
mod parse;
mod support;

#[cfg(test)]
mod tests;

pub use error::{
    FragmentArithmeticOperation, FragmentBoxKind, FragmentDrmEvidence, FragmentInspectionError,
    FragmentInspectionLimitKind, FragmentPrivateExtension, FragmentStructureContext,
    FragmentTimingEvidence, FragmentUnsupportedLayout,
};
pub use initialization::{
    FragmentAacAudioSpecificConfig, FragmentAacChannelCount, FragmentAacLcConfiguration,
    FragmentAacSampleRate, FragmentBoxType, FragmentCodecConfigurationIssue, FragmentCodecKind,
    FragmentH264Configuration, FragmentH264PictureParameterSet, FragmentH264SequenceParameterSet,
    FragmentInitializationCodec, FragmentInitializationError, FragmentInitializationField,
    FragmentInitializationLimitBuildError, FragmentInitializationLimitKind,
    FragmentInitializationLimits, FragmentInitializationLimitsBuilder,
    FragmentInitializationRequest, FragmentInitializationSegment, FragmentTimescale,
    FragmentVideoDimensions, FragmentVideoHeight, FragmentVideoWidth,
    build_fragmented_initialization_segment,
};
pub use limits::{
    FragmentInspectionLimitBuildError, FragmentInspectionLimits, FragmentInspectionLimitsBuilder,
};
pub use media::{
    FragmentMediaBoxType, FragmentMediaKind, FragmentReconstructionError,
    FragmentReconstructionRequest, FragmentTrackReconstructionIntent,
    FragmentWriteArithmeticOperation, FragmentWriteCancellationPhase, FragmentWriteError,
    FragmentWriteLimitBuildError, FragmentWriteLimits, ReconstructedMediaSegment,
    reconstruct_media_fragment,
};
pub use model::{
    FragmentBaseDecodeTime, FragmentCodedCoverage, FragmentSampleDefaults, FragmentTrackId,
};
