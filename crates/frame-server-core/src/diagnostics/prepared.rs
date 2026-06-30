use crate::working_set::TimelineHoverPrepareDemoteBackRejection;

/// Нейтральное состояние video runway для prepared resume.
///
/// `frame-server-core` не владеет player lifecycle, поэтому caller передаёт уже
/// принятое owner-ом состояние, а diagnostics только считает typed категории.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubResumeRunwayState {
    Pending,
    Repositioned,
    PostTargetPacketAccepted,
    DisplayableFrameQueued,
    NextFrameAlmostReady,
}

impl ScrubResumeRunwayState {
    /// Только эти состояния можно считать готовыми к commit с video стороны.
    #[must_use]
    pub const fn is_commit_ready(self) -> bool {
        matches!(
            self,
            Self::DisplayableFrameQueued | Self::NextFrameAlmostReady
        )
    }
}

/// Почему prepared frame hit не стал instant resume-ready hit-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubPreparedFrameResumePendingReason {
    FrameOnly,
    ContinuationMissing,
    RunwayPending(ScrubResumeRunwayState),
    CommitGatePending {
        video_runway: ScrubResumeRunwayState,
    },
    AudioGatePending {
        video_runway: ScrubResumeRunwayState,
    },
}

/// Итог prepared-frame route decision после ownership transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubPreparedFrameHitOutcome {
    ResumeReady {
        video_runway: ScrubResumeRunwayState,
    },
    ResumePending {
        reason: ScrubPreparedFrameResumePendingReason,
    },
}

/// Coarse typed reason demote-back rejection-а без key/resource payload-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubPreparedFrameDemoteRejectionKind {
    CancelReasonDoesNotAllowDemote,
    PromotedKeyNotCurrent,
    TimingRejected,
    RecentSupersededRetentionDisabled,
}

impl From<TimelineHoverPrepareDemoteBackRejection> for ScrubPreparedFrameDemoteRejectionKind {
    fn from(reason: TimelineHoverPrepareDemoteBackRejection) -> Self {
        match reason {
            TimelineHoverPrepareDemoteBackRejection::CancelReasonDoesNotAllowDemote { .. } => {
                Self::CancelReasonDoesNotAllowDemote
            }
            TimelineHoverPrepareDemoteBackRejection::PromotedKeyNotCurrent { .. } => {
                Self::PromotedKeyNotCurrent
            }
            TimelineHoverPrepareDemoteBackRejection::TimingRejected(_) => Self::TimingRejected,
            TimelineHoverPrepareDemoteBackRejection::RecentSupersededRetentionDisabled {
                ..
            } => Self::RecentSupersededRetentionDisabled,
        }
    }
}

/// Ownership event для promoted prepared branch-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrubPreparedFrameOwnershipEvent {
    PromotedResumeReadyBranch,
    PromotedVisualOverrideResumePending,
    DemotedToRecentSuperseded,
    DemoteRejected(ScrubPreparedFrameDemoteRejectionKind),
    ReleasedWithoutDemote,
    NoPromotedFrameOnRelease,
}

/// Prepared diagnostics, достаточные для prepared-vs-cold scrub validation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubPreparedFrameDiagnosticsCounters {
    pub prepared_frame_hits: u64,
    pub resume_ready_prepared_hits: u64,
    pub prepared_frame_resume_runway_pending: u64,
    pub prepared_frame_commit_gate_pending: u64,
    pub prepared_frame_audio_gate_pending: u64,
    pub cold_exact_decode_pending: u64,
    pub resume_pending_reasons: ScrubPreparedFrameResumePendingReasonCounters,
    pub video_runway: ScrubResumeRunwayStateCounters,
    pub ownership: ScrubPreparedFrameOwnershipCounters,
}

impl ScrubPreparedFrameDiagnosticsCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prepared_frame_hits: 0,
            resume_ready_prepared_hits: 0,
            prepared_frame_resume_runway_pending: 0,
            prepared_frame_commit_gate_pending: 0,
            prepared_frame_audio_gate_pending: 0,
            cold_exact_decode_pending: 0,
            resume_pending_reasons: ScrubPreparedFrameResumePendingReasonCounters::new(),
            video_runway: ScrubResumeRunwayStateCounters::new(),
            ownership: ScrubPreparedFrameOwnershipCounters::new(),
        }
    }

    pub fn record_prepared_hit(&mut self, outcome: ScrubPreparedFrameHitOutcome) {
        self.prepared_frame_hits = self.prepared_frame_hits.saturating_add(1);
        match outcome {
            ScrubPreparedFrameHitOutcome::ResumeReady { video_runway } => {
                self.resume_ready_prepared_hits = self.resume_ready_prepared_hits.saturating_add(1);
                self.video_runway.increment(video_runway);
            }
            ScrubPreparedFrameHitOutcome::ResumePending { reason } => {
                self.resume_pending_reasons.increment(reason);
                match reason {
                    ScrubPreparedFrameResumePendingReason::RunwayPending(video_runway) => {
                        self.prepared_frame_resume_runway_pending =
                            self.prepared_frame_resume_runway_pending.saturating_add(1);
                        self.video_runway.increment(video_runway);
                    }
                    ScrubPreparedFrameResumePendingReason::AudioGatePending { video_runway } => {
                        self.prepared_frame_audio_gate_pending =
                            self.prepared_frame_audio_gate_pending.saturating_add(1);
                        self.video_runway.increment(video_runway);
                    }
                    ScrubPreparedFrameResumePendingReason::CommitGatePending { video_runway } => {
                        self.prepared_frame_commit_gate_pending =
                            self.prepared_frame_commit_gate_pending.saturating_add(1);
                        self.video_runway.increment(video_runway);
                    }
                    ScrubPreparedFrameResumePendingReason::FrameOnly
                    | ScrubPreparedFrameResumePendingReason::ContinuationMissing => {}
                }
            }
        }
    }

    pub fn record_cold_exact_decode_pending(&mut self) {
        self.cold_exact_decode_pending = self.cold_exact_decode_pending.saturating_add(1);
    }

    pub fn record_ownership_event(&mut self, event: ScrubPreparedFrameOwnershipEvent) {
        self.ownership.increment(event);
    }
}

/// Counts конкретных resume-pending причин.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubPreparedFrameResumePendingReasonCounters {
    pub frame_only: u64,
    pub continuation_missing: u64,
    pub runway_pending: u64,
    pub commit_gate_pending: u64,
    pub audio_gate_pending: u64,
}

impl ScrubPreparedFrameResumePendingReasonCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            frame_only: 0,
            continuation_missing: 0,
            runway_pending: 0,
            commit_gate_pending: 0,
            audio_gate_pending: 0,
        }
    }

    pub fn increment(&mut self, reason: ScrubPreparedFrameResumePendingReason) {
        match reason {
            ScrubPreparedFrameResumePendingReason::FrameOnly => {
                self.frame_only = self.frame_only.saturating_add(1);
            }
            ScrubPreparedFrameResumePendingReason::ContinuationMissing => {
                self.continuation_missing = self.continuation_missing.saturating_add(1);
            }
            ScrubPreparedFrameResumePendingReason::RunwayPending(_) => {
                self.runway_pending = self.runway_pending.saturating_add(1);
            }
            ScrubPreparedFrameResumePendingReason::CommitGatePending { .. } => {
                self.commit_gate_pending = self.commit_gate_pending.saturating_add(1);
            }
            ScrubPreparedFrameResumePendingReason::AudioGatePending { .. } => {
                self.audio_gate_pending = self.audio_gate_pending.saturating_add(1);
            }
        }
    }
}

/// Counts runway states плюс split progress-only vs commit-ready.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubResumeRunwayStateCounters {
    pub pending: u64,
    pub repositioned: u64,
    pub post_target_packet_accepted: u64,
    pub displayable_frame_queued: u64,
    pub next_frame_almost_ready: u64,
    pub progress_only: u64,
    pub commit_ready: u64,
}

impl ScrubResumeRunwayStateCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending: 0,
            repositioned: 0,
            post_target_packet_accepted: 0,
            displayable_frame_queued: 0,
            next_frame_almost_ready: 0,
            progress_only: 0,
            commit_ready: 0,
        }
    }

    pub fn increment(&mut self, state: ScrubResumeRunwayState) {
        match state {
            ScrubResumeRunwayState::Pending => {
                self.pending = self.pending.saturating_add(1);
            }
            ScrubResumeRunwayState::Repositioned => {
                self.repositioned = self.repositioned.saturating_add(1);
            }
            ScrubResumeRunwayState::PostTargetPacketAccepted => {
                self.post_target_packet_accepted =
                    self.post_target_packet_accepted.saturating_add(1);
            }
            ScrubResumeRunwayState::DisplayableFrameQueued => {
                self.displayable_frame_queued = self.displayable_frame_queued.saturating_add(1);
            }
            ScrubResumeRunwayState::NextFrameAlmostReady => {
                self.next_frame_almost_ready = self.next_frame_almost_ready.saturating_add(1);
            }
        }

        if state.is_commit_ready() {
            self.commit_ready = self.commit_ready.saturating_add(1);
        } else {
            self.progress_only = self.progress_only.saturating_add(1);
        }
    }
}

/// Counts ownership transitions для promoted prepared resources.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubPreparedFrameOwnershipCounters {
    pub promoted_to_seek_ownership: u64,
    pub promoted_resume_ready_branch: u64,
    pub promoted_visual_override_resume_pending: u64,
    pub demoted_to_recent_superseded: u64,
    pub demote_rejected: u64,
    pub released_without_demote: u64,
    pub no_promoted_frame_on_release: u64,
    pub demote_rejection_reasons: ScrubPreparedFrameDemoteRejectionCounters,
}

impl ScrubPreparedFrameOwnershipCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            promoted_to_seek_ownership: 0,
            promoted_resume_ready_branch: 0,
            promoted_visual_override_resume_pending: 0,
            demoted_to_recent_superseded: 0,
            demote_rejected: 0,
            released_without_demote: 0,
            no_promoted_frame_on_release: 0,
            demote_rejection_reasons: ScrubPreparedFrameDemoteRejectionCounters::new(),
        }
    }

    pub fn increment(&mut self, event: ScrubPreparedFrameOwnershipEvent) {
        match event {
            ScrubPreparedFrameOwnershipEvent::PromotedResumeReadyBranch => {
                self.promoted_to_seek_ownership = self.promoted_to_seek_ownership.saturating_add(1);
                self.promoted_resume_ready_branch =
                    self.promoted_resume_ready_branch.saturating_add(1);
            }
            ScrubPreparedFrameOwnershipEvent::PromotedVisualOverrideResumePending => {
                self.promoted_to_seek_ownership = self.promoted_to_seek_ownership.saturating_add(1);
                self.promoted_visual_override_resume_pending = self
                    .promoted_visual_override_resume_pending
                    .saturating_add(1);
            }
            ScrubPreparedFrameOwnershipEvent::DemotedToRecentSuperseded => {
                self.demoted_to_recent_superseded =
                    self.demoted_to_recent_superseded.saturating_add(1);
            }
            ScrubPreparedFrameOwnershipEvent::DemoteRejected(reason) => {
                self.demote_rejected = self.demote_rejected.saturating_add(1);
                self.demote_rejection_reasons.increment(reason);
            }
            ScrubPreparedFrameOwnershipEvent::ReleasedWithoutDemote => {
                self.released_without_demote = self.released_without_demote.saturating_add(1);
            }
            ScrubPreparedFrameOwnershipEvent::NoPromotedFrameOnRelease => {
                self.no_promoted_frame_on_release =
                    self.no_promoted_frame_on_release.saturating_add(1);
            }
        }
    }
}

/// Counts demote rejection kinds без хранения key/resource payload-а.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ScrubPreparedFrameDemoteRejectionCounters {
    pub cancel_reason_does_not_allow_demote: u64,
    pub promoted_key_not_current: u64,
    pub timing_rejected: u64,
    pub recent_superseded_retention_disabled: u64,
}

impl ScrubPreparedFrameDemoteRejectionCounters {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cancel_reason_does_not_allow_demote: 0,
            promoted_key_not_current: 0,
            timing_rejected: 0,
            recent_superseded_retention_disabled: 0,
        }
    }

    pub fn increment(&mut self, reason: ScrubPreparedFrameDemoteRejectionKind) {
        match reason {
            ScrubPreparedFrameDemoteRejectionKind::CancelReasonDoesNotAllowDemote => {
                self.cancel_reason_does_not_allow_demote =
                    self.cancel_reason_does_not_allow_demote.saturating_add(1);
            }
            ScrubPreparedFrameDemoteRejectionKind::PromotedKeyNotCurrent => {
                self.promoted_key_not_current = self.promoted_key_not_current.saturating_add(1);
            }
            ScrubPreparedFrameDemoteRejectionKind::TimingRejected => {
                self.timing_rejected = self.timing_rejected.saturating_add(1);
            }
            ScrubPreparedFrameDemoteRejectionKind::RecentSupersededRetentionDisabled => {
                self.recent_superseded_retention_disabled =
                    self.recent_superseded_retention_disabled.saturating_add(1);
            }
        }
    }
}
