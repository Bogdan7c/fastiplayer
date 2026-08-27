//! Typed candidate rejections, deterministic aggregation и secret-safe summary.

use audio_core::{AudioDecodeCapabilityQueryError, AudioDecodeCodecFamily};
use capability_core::UnsupportedVideoRequirement;
use codec_core::VideoCodec;
use demux_api::DemuxInputCapabilities;
use web_media_core::{
    CandidateIdentity, ContainerFamily, ExtractionGeneration, SemanticIdentity, TransportFamily,
};

use super::CandidateEvaluation;
use crate::candidate::{exact_identity, semantic_identity};

impl CandidateEvaluation<'_> {
    /// Превращает rejected evaluation в safe owned diagnostics без изменения порядка причин.
    pub(super) fn into_rejection(self) -> CandidateRejection {
        CandidateRejection {
            exact_identity: exact_identity(self.candidate).clone(),
            semantic_identity: semantic_identity(self.candidate).clone(),
            reasons: self.rejection_reasons.into_boxed_slice(),
        }
    }
}

/// Safe typed rejection одного exact candidate-а.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateRejection {
    /// Exact identity без raw locator/request material.
    exact_identity: CandidateIdentity,
    /// Semantic identity с redacted Debug.
    semantic_identity: SemanticIdentity,
    /// Все обнаруженные capability/policy причины.
    reasons: Box<[CandidateRejectionReason]>,
}

impl CandidateRejection {
    /// Возвращает exact identity.
    pub const fn exact_identity(&self) -> &CandidateIdentity {
        &self.exact_identity
    }

    /// Возвращает semantic identity.
    pub const fn semantic_identity(&self) -> &SemanticIdentity {
        &self.semantic_identity
    }

    /// Возвращает ordered exact rejection reasons.
    pub const fn reasons(&self) -> &[CandidateRejectionReason] {
        &self.reasons
    }
}

/// Capability и selection policy не смешиваются в одной generic причине.
#[derive(Debug, Clone, PartialEq)]
pub enum CandidateRejectionReason {
    /// Runtime capability snapshot не закрывает слой.
    Capability(CandidateCapabilityRejection),
    /// Candidate playable по runtime, но исключён explicit selection policy.
    Policy(CandidatePolicyRejection),
}

/// Exact runtime capability layer rejection.
#[derive(Debug, Clone, PartialEq)]
pub enum CandidateCapabilityRejection {
    /// Transport family не зарегистрирована.
    Transport(TransportCapabilityRejection),
    /// Container/input demux path отсутствует.
    Demux(DemuxCapabilityRejection),
    /// Existing video system capability intersection отклонил requirement.
    Video {
        /// Physical component, которому нужен decoder.
        component: PlaybackComponent,
        /// Existing detailed capability-core rejection.
        unsupported: UnsupportedVideoRequirement,
    },
    /// S20 snapshot не содержит runtime decoder path.
    AudioUnavailable {
        /// Physical component, которому нужен decoder.
        component: PlaybackComponent,
        /// Exact static-profile-approved audio family.
        family: AudioDecodeCodecFamily,
    },
    /// S20 snapshot отверг сам typed query вместо ответа Available/Unavailable.
    AudioQueryRejected {
        /// Physical component, которому нужен decoder.
        component: PlaybackComponent,
        /// Exact S20 query error.
        error: AudioDecodeCapabilityQueryError,
    },
}

/// Typed transport-layer rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportCapabilityRejection {
    /// Physical component без transport provider path.
    pub component: PlaybackComponent,
    /// Exact normalized transport family.
    pub family: TransportFamily,
}

/// Typed demux-layer rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemuxCapabilityRejection {
    /// Container family не зарегистрирована ни одним demux factory.
    ContainerUnavailable {
        /// Physical component.
        component: PlaybackComponent,
        /// Exact normalized container family.
        container: ContainerFamily,
    },
    /// Transport и demux существуют, но не имеют общей neutral input shape.
    InputShapeMismatch {
        /// Physical component.
        component: PlaybackComponent,
        /// Exact normalized container family.
        container: ContainerFamily,
        /// Возможные transport outputs.
        transport_outputs: DemuxInputCapabilities,
        /// Поддержанные demux inputs.
        demux_inputs: DemuxInputCapabilities,
    },
}

/// Explicit policy rejection после capability intersection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidatePolicyRejection {
    /// Dynamic-range metadata недостаточно для SDR/HDR bucket-а.
    UnknownDynamicRange,
    /// `SdrOnly` исключил HDR candidate.
    HdrExcluded,
    /// Codec отсутствует в explicit configured preference list.
    VideoCodecExcluded {
        /// Canonical existing video codec.
        codec: VideoCodec,
    },
}

/// Physical component identity для точной локализации missing layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlaybackComponent {
    /// Один muxed resource.
    Muxed,
    /// Single resource, чья track topology подтверждается после demux open.
    ContentProbed,
    /// Video-only resource.
    Video,
    /// Audio-only resource.
    Audio,
}

/// Secret-safe сводка слоёв, из-за которых planner не смог выбрать candidate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaybackPlanningFailureSummary {
    /// Количество candidates, для которых planner сохранил typed rejection.
    rejected_candidates: usize,
    /// Количество отсутствующих transport paths среди всех physical components.
    transport_rejections: usize,
    /// Количество отсутствующих или несовместимых demux paths.
    demux_rejections: usize,
    /// Количество несовместимых video decode requirements.
    video_rejections: usize,
    /// Количество отсутствующих или отвергнутых audio decode capabilities.
    audio_rejections: usize,
    /// Количество explicit selection-policy exclusions.
    policy_rejections: usize,
}

impl PlaybackPlanningFailureSummary {
    /// Возвращает число candidates с typed rejection.
    pub const fn rejected_candidates(self) -> usize {
        self.rejected_candidates
    }

    /// Возвращает число transport-layer отказов.
    pub const fn transport_rejections(self) -> usize {
        self.transport_rejections
    }

    /// Возвращает число demux-layer отказов.
    pub const fn demux_rejections(self) -> usize {
        self.demux_rejections
    }

    /// Возвращает число video capability отказов.
    pub const fn video_rejections(self) -> usize {
        self.video_rejections
    }

    /// Возвращает число audio capability отказов.
    pub const fn audio_rejections(self) -> usize {
        self.audio_rejections
    }

    /// Возвращает число policy-layer отказов.
    pub const fn policy_rejections(self) -> usize {
        self.policy_rejections
    }

    /// Учитывает одну typed причину без копирования candidate identity.
    fn record_reason(&mut self, reason: &CandidateRejectionReason) {
        match reason {
            CandidateRejectionReason::Capability(CandidateCapabilityRejection::Transport(_)) => {
                self.transport_rejections += 1;
            }
            CandidateRejectionReason::Capability(CandidateCapabilityRejection::Demux(_)) => {
                self.demux_rejections += 1;
            }
            CandidateRejectionReason::Capability(CandidateCapabilityRejection::Video {
                ..
            }) => {
                self.video_rejections += 1;
            }
            CandidateRejectionReason::Capability(
                CandidateCapabilityRejection::AudioUnavailable { .. }
                | CandidateCapabilityRejection::AudioQueryRejected { .. },
            ) => {
                self.audio_rejections += 1;
            }
            CandidateRejectionReason::Policy(_) => {
                self.policy_rejections += 1;
            }
        }
    }

    /// Учитывает один candidate rejection и все независимые physical-layer причины.
    fn record_candidate(&mut self, rejection: &CandidateRejection) {
        self.rejected_candidates += 1;
        for reason in rejection.reasons() {
            self.record_reason(reason);
        }
    }
}

impl std::fmt::Display for PlaybackPlanningFailureSummary {
    /// Печатает только bounded counters без source/candidate/request identities.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "rejected_candidates={}, transport={}, demux={}, video={}, audio={}, policy={}",
            self.rejected_candidates,
            self.transport_rejections,
            self.demux_rejections,
            self.video_rejections,
            self.audio_rejections,
            self.policy_rejections,
        )
    }
}

/// Selection failure, отделённый от static compatibility и operational open failures.
#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackPlanningError {
    /// BestPlayable получил пустой statically-compatible inventory.
    EmptyCandidates,
    /// Exact identity относится к другой source lineage.
    ExactSourceMismatch,
    /// Exact identity относится к старой extraction generation.
    StaleExactIdentity {
        /// Generation сохранённого intent-а.
        requested: ExtractionGeneration,
        /// Текущая immutable snapshot generation.
        current: ExtractionGeneration,
    },
    /// Exact ID отсутствует в matching snapshot.
    ExactCandidateUnavailable,
    /// Exact ID найден, но semantic attributes изменились.
    ExactSemanticIdentityChanged,
    /// Exact candidate существует, но не проходит capability/policy intersection.
    ExactCandidateNotPlayable(CandidateRejection),
    /// Ни один candidate не прошёл capability/policy intersection.
    NoPlayableCandidates {
        /// Typed diagnostics всех отклонённых candidates.
        rejections: Box<[CandidateRejection]>,
    },
}

impl PlaybackPlanningError {
    /// Агрегирует только safe rejection categories для production diagnostics.
    #[must_use]
    pub fn safe_summary(&self) -> PlaybackPlanningFailureSummary {
        let mut summary = PlaybackPlanningFailureSummary::default();
        match self {
            Self::ExactCandidateNotPlayable(rejection) => {
                summary.record_candidate(rejection);
            }
            Self::NoPlayableCandidates { rejections } => {
                for rejection in rejections {
                    summary.record_candidate(rejection);
                }
            }
            Self::EmptyCandidates
            | Self::ExactSourceMismatch
            | Self::StaleExactIdentity { .. }
            | Self::ExactCandidateUnavailable
            | Self::ExactSemanticIdentityChanged => {}
        }
        summary
    }
}

impl std::fmt::Display for PlaybackPlanningError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::EmptyCandidates => "нет statically-compatible candidates",
            Self::ExactSourceMismatch => "exact candidate относится к другому source",
            Self::StaleExactIdentity { .. } => "exact candidate identity устарела",
            Self::ExactCandidateUnavailable => "exact candidate отсутствует в snapshot",
            Self::ExactSemanticIdentityChanged => "semantic attributes exact candidate изменились",
            Self::ExactCandidateNotPlayable(_) => "exact candidate сейчас не playable",
            Self::NoPlayableCandidates { .. } => "нет candidate-а с полным capability path",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for PlaybackPlanningError {}
