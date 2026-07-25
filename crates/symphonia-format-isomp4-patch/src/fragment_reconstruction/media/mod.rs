//! Public opt-in canonical reconstruction одного media fragment-а.

mod error;
mod model;
mod plan;
mod write;

#[cfg(test)]
mod tests;

pub use error::{
    FragmentMediaBoxType, FragmentReconstructionError, FragmentWriteArithmeticOperation,
    FragmentWriteCancellationPhase, FragmentWriteError, FragmentWriteLimitBuildError,
};
pub use model::{
    FragmentMediaKind, FragmentReconstructionRequest, FragmentTrackReconstructionIntent,
    FragmentWriteLimits, ReconstructedMediaSegment,
};

use super::error::FragmentInspectionError;
use super::inspect::inspect_media_fragment;
use super::model::{FragmentInspectionRequest, FragmentRapRequirement, FragmentTrackExpectation};
use plan::plan_media_fragment;
use write::write_media_fragment;

/// Инспектирует недоверенный input и публикует deterministic canonical `moof+mdat`.
pub fn reconstruct_media_fragment(
    request: FragmentReconstructionRequest<'_, '_>,
) -> Result<ReconstructedMediaSegment, FragmentReconstructionError> {
    if request.is_cancelled() {
        return Err(FragmentInspectionError::Cancelled.into());
    }

    let track = request.track();
    let rap_requirement = match track.media_kind() {
        FragmentMediaKind::VideoWithRequiredProvenRandomAccess => {
            FragmentRapRequirement::RequireProvenVideoRandomAccess
        }
        FragmentMediaKind::AudioWithoutRandomAccessRequirement => {
            FragmentRapRequirement::NotRequiredForAudio
        }
    };
    let expectation = FragmentTrackExpectation::new(
        track.track_id(),
        track.base_decode_time(),
        rap_requirement,
        track.sample_defaults(),
    );
    let inspection_request = FragmentInspectionRequest::new(
        request.input(),
        expectation,
        request.inspection_limits(),
        request.cancellation(),
    );
    let normalized = inspect_media_fragment(&inspection_request)?;
    let layout = plan_media_fragment(
        &normalized,
        track.media_kind(),
        request.write_limits(),
        request.cancellation(),
    )?;
    let bytes = write_media_fragment(&normalized, layout, request.cancellation())?;

    Ok(ReconstructedMediaSegment::verified(
        bytes,
        normalized.sequence_number(),
        normalized.track_id(),
        normalized.coded_coverage(),
    ))
}
