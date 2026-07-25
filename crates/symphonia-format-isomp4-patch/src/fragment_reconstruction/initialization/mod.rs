//! Public opt-in builder отдельного fragmented MP4 initialization segment-а.

mod error;
mod model;
mod plan;
mod validate;
mod write;

#[cfg(test)]
mod tests;

pub use error::{
    FragmentBoxType, FragmentCodecConfigurationIssue, FragmentCodecKind,
    FragmentInitializationError, FragmentInitializationField,
    FragmentInitializationLimitBuildError, FragmentInitializationLimitKind,
};
pub use model::{
    FragmentAacAudioSpecificConfig, FragmentAacChannelCount, FragmentAacLcConfiguration,
    FragmentAacSampleRate, FragmentH264Configuration, FragmentH264PictureParameterSet,
    FragmentH264SequenceParameterSet, FragmentInitializationCodec, FragmentInitializationLimits,
    FragmentInitializationLimitsBuilder, FragmentInitializationRequest,
    FragmentInitializationSegment, FragmentTimescale, FragmentVideoDimensions, FragmentVideoHeight,
    FragmentVideoWidth,
};

use plan::plan_initialization_segment;
use write::write_initialization_segment;

/// Строит отдельный `ftyp + moov` для одного fragmented MP4 track-а.
///
/// Функция ничего не добавляет к media fragment-ам и публикует bytes только после
/// полной проверки codec configuration, checked planning и финальной cancellation fence.
pub fn build_fragmented_initialization_segment(
    request: FragmentInitializationRequest<'_, '_>,
) -> Result<FragmentInitializationSegment, FragmentInitializationError> {
    if request.is_cancelled() {
        return Err(FragmentInitializationError::Cancelled);
    }

    let plan = plan_initialization_segment(&request)?;

    if request.is_cancelled() {
        return Err(FragmentInitializationError::Cancelled);
    }

    let bytes = write_initialization_segment(&plan)?;

    if request.is_cancelled() {
        return Err(FragmentInitializationError::Cancelled);
    }

    Ok(FragmentInitializationSegment::verified(bytes))
}
