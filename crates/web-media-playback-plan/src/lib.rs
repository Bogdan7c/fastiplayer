//! Pure planning playable web-media layout-а до любого I/O.
//!
//! Crate пересекает только immutable neutral capability snapshots. Здесь нет
//! provider factory, source, demuxer, decoder, player, service или UI типов,
//! поэтому selection не может случайно запустить operational side effect.

#![forbid(unsafe_code)]

mod candidate;
mod capability;
mod planner;
mod policy;

pub use candidate::{
    CandidateQualityScore, CandidateRuntimeRequirements, PlanningCandidate,
    PlanningCandidateBuildError, PlanningCandidateSnapshot, PlanningComponent,
    PlanningSnapshotBuildError,
};
pub use capability::{
    CapabilitySnapshotBuildError, DemuxCapabilityRegistration, DemuxCapabilitySnapshot,
    PlaybackCapabilitySnapshot, TransportCapabilityRegistration, TransportCapabilitySnapshot,
};
pub use planner::{
    CandidateCapabilityRejection, CandidatePolicyRejection, CandidateRejection,
    CandidateRejectionReason, DemuxCapabilityRejection, PlaybackComponent, PlaybackPlan,
    PlaybackPlanningError, PlaybackPlanningFailureSummary, PlaybackPlanningOutcome,
    TransportCapabilityRejection, plan_playback,
};
pub use policy::{
    ContainerPreferenceRank, HdrSelectionPolicy, PlaybackSelectionPolicy, SelectionPolicyBuildError,
};

#[cfg(test)]
mod tests;
