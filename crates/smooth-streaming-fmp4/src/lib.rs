//! Чистый адаптер между validated Smooth Streaming manifest и ISO-BMFF F1 boundary.
//!
//! Crate отвечает только за выбор manifest track-а, точное отображение codec/init
//! полей, план относительного fragment path и классификацию reconstructed coverage.
//! HTTP, provider state, demux, playback, seek, live и фактический audio clipping
//! намеренно остаются у следующих владельцев.

mod error;
mod initialization;
mod mapping;
mod planning;
mod reconstruction;

pub use error::{
    SmoothFragmentPlanError, SmoothFragmentReconstructionError, SmoothInitializationError,
    SmoothTrackMappingError,
};
pub use initialization::{
    SmoothInitializationRequest, SmoothInitializationSegment, build_smooth_initialization_segment,
};
pub use mapping::{
    SmoothFragmentIndex, SmoothMappedTrack, SmoothStreamOrdinal, SmoothTrackIdentity,
    SmoothTrackMappingRequest, SmoothTrackMediaKind, SmoothTrackSelection, map_smooth_track,
};
pub use planning::{
    SmoothFragmentPlan, SmoothFragmentPlanRequest, SmoothFragmentRelativePath,
    SmoothManifestWindow, plan_smooth_fragment,
};
pub use reconstruction::{
    SmoothAdmittedFragment, SmoothAudioPresentationWindowAdjustment,
    SmoothFragmentReconstructionRequest, SmoothPendingAudioPresentationWindow,
    SmoothReconstructedFragment, SmoothTimingRelation, reconstruct_smooth_fragment,
};
