use std::cmp::Ordering;

use audio_core::AudioDecodeCapabilityQueryError;
use audio_core::{AudioDecodeCapability, AudioDecodeCodecFamily, AudioDecodeCodecFamilyQuery};
use capability_core::{SupportedVideoOutput, UnsupportedVideoRequirement};
use codec_core::{VideoCodec, VideoDecodeRequirement};
use demux_api::DemuxInputCapabilities;
use web_media_core::{
    CandidateIdentity, ContainerFamily, DynamicRange, ExactSelectionIdentity, ExtractionGeneration,
    SelectionRequest, SemanticIdentity, StreamLayoutKind, TransportFamily,
};

use crate::candidate::{
    CandidateRuntimeRequirements, PlanningCandidate, PlanningCandidateSnapshot, PlanningResource,
    PlanningResourceLayout, exact_identity, semantic_identity,
};
use crate::capability::PlaybackCapabilitySnapshot;
use crate::policy::{HdrSelectionPolicy, PlaybackSelectionPolicy};

/// Выбирает playable candidate, не создавая ни одного runtime object-а.
pub fn plan_playback(
    candidates: &PlanningCandidateSnapshot,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    request: &SelectionRequest,
    policy: &PlaybackSelectionPolicy,
) -> Result<PlaybackPlanningOutcome, PlaybackPlanningError> {
    match request {
        SelectionRequest::BestPlayable => plan_best_playable(candidates, capabilities, policy),
        SelectionRequest::Exact(exact) => plan_exact(candidates, capabilities, exact, policy),
    }
}

/// Проверяет Exact identity и не выполняет semantic rematch внутри pure planner-а.
fn plan_exact(
    snapshot: &PlanningCandidateSnapshot,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    requested: &ExactSelectionIdentity,
    policy: &PlaybackSelectionPolicy,
) -> Result<PlaybackPlanningOutcome, PlaybackPlanningError> {
    if requested.exact().source() != snapshot.source() {
        return Err(PlaybackPlanningError::ExactSourceMismatch);
    }
    if requested.exact().generation() != snapshot.generation() {
        return Err(PlaybackPlanningError::StaleExactIdentity {
            requested: requested.exact().generation(),
            current: snapshot.generation(),
        });
    }

    let candidate = snapshot
        .candidates()
        .iter()
        .find(|candidate| exact_identity(candidate) == requested.exact())
        .ok_or(PlaybackPlanningError::ExactCandidateUnavailable)?;
    if semantic_identity(candidate) != requested.semantic() {
        return Err(PlaybackPlanningError::ExactSemanticIdentityChanged);
    }

    let evaluation = evaluate_candidate(candidate, capabilities, policy);
    if !evaluation.rejection_reasons.is_empty() {
        return Err(PlaybackPlanningError::ExactCandidateNotPlayable(
            evaluation.into_rejection(),
        ));
    }

    Ok(PlaybackPlanningOutcome {
        selected: evaluation.into_plan(),
        rejected_candidates: Box::new([]),
    })
}

/// Проверяет весь inventory, чтобы один несовместимый candidate не блокировал другой.
fn plan_best_playable(
    snapshot: &PlanningCandidateSnapshot,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    policy: &PlaybackSelectionPolicy,
) -> Result<PlaybackPlanningOutcome, PlaybackPlanningError> {
    let (ranked, rejected_candidates) = rank_playable_candidates(snapshot, capabilities, policy)?;
    let selected = ranked
        .into_iter()
        .next()
        .ok_or(PlaybackPlanningError::EmptyCandidates)?;

    Ok(PlaybackPlanningOutcome {
        selected,
        rejected_candidates,
    })
}

/// Один policy pass для best selection и grouped opaque ranking.
pub(crate) fn rank_playable_candidates(
    snapshot: &PlanningCandidateSnapshot,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    policy: &PlaybackSelectionPolicy,
) -> Result<(Vec<PlaybackPlan>, Box<[CandidateRejection]>), PlaybackPlanningError> {
    if snapshot.candidates().is_empty() {
        return Err(PlaybackPlanningError::EmptyCandidates);
    }

    let mut playable = Vec::new();
    let mut rejected = Vec::new();
    for candidate in snapshot.candidates() {
        let evaluation = evaluate_candidate(candidate, capabilities, policy);
        if evaluation.rejection_reasons.is_empty() {
            playable.push(evaluation);
        } else {
            rejected.push(evaluation.into_rejection());
        }
    }

    if playable.is_empty() {
        return Err(PlaybackPlanningError::NoPlayableCandidates {
            rejections: rejected.into_boxed_slice(),
        });
    }

    playable.sort_by(|left, right| compare_playable(left, right, policy));
    Ok((
        playable
            .into_iter()
            .map(CandidateEvaluation::into_plan)
            .collect(),
        rejected.into_boxed_slice(),
    ))
}

/// Полностью оценивает transport/demux/decode/policy layers одного candidate-а.
fn evaluate_candidate<'candidate>(
    candidate: &'candidate PlanningCandidate,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    policy: &PlaybackSelectionPolicy,
) -> CandidateEvaluation<'candidate> {
    let mut rejection_reasons = Vec::new();
    check_resources(candidate.resources(), capabilities, &mut rejection_reasons);

    let matched_video_output = check_decode_requirements(
        candidate.runtime_requirements(),
        capabilities,
        &mut rejection_reasons,
    );
    check_selection_policy(candidate, policy, &mut rejection_reasons);

    CandidateEvaluation {
        candidate,
        matched_video_output,
        rejection_reasons,
    }
}

/// Проверяет каждый physical resource layout-а независимо.
fn check_resources(
    resources: PlanningResourceLayout,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    rejections: &mut Vec<CandidateRejectionReason>,
) {
    match resources {
        PlanningResourceLayout::Muxed(resource) => {
            check_resource(PlaybackComponent::Muxed, resource, capabilities, rejections);
        }
        PlanningResourceLayout::Separate { video, audio } => {
            check_resource(PlaybackComponent::Video, video, capabilities, rejections);
            check_resource(PlaybackComponent::Audio, audio, capabilities, rejections);
        }
        PlanningResourceLayout::VideoOnly(resource) => {
            check_resource(PlaybackComponent::Video, resource, capabilities, rejections);
        }
        PlanningResourceLayout::AudioOnly(resource) => {
            check_resource(PlaybackComponent::Audio, resource, capabilities, rejections);
        }
    }
}

/// Пересекает transport output shapes с container demux input shapes.
fn check_resource(
    component: PlaybackComponent,
    resource: PlanningResource,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    rejections: &mut Vec<CandidateRejectionReason>,
) {
    let transport_outputs = capabilities
        .transport()
        .output_inputs_for(resource.transport);
    let demux_inputs = capabilities
        .demux()
        .input_capabilities_for(resource.container);

    if transport_outputs.is_empty() {
        rejections.push(CandidateRejectionReason::Capability(
            CandidateCapabilityRejection::Transport(TransportCapabilityRejection {
                component,
                family: resource.transport,
            }),
        ));
    }

    if demux_inputs.is_empty() {
        rejections.push(CandidateRejectionReason::Capability(
            CandidateCapabilityRejection::Demux(DemuxCapabilityRejection::ContainerUnavailable {
                component,
                container: resource.container,
            }),
        ));
    } else if !transport_outputs.is_empty() && !transport_outputs.intersects(demux_inputs) {
        rejections.push(CandidateRejectionReason::Capability(
            CandidateCapabilityRejection::Demux(DemuxCapabilityRejection::InputShapeMismatch {
                component,
                container: resource.container,
                transport_outputs,
                demux_inputs,
            }),
        ));
    }
}

/// Проверяет video/audio snapshots без factory/decoder construction.
fn check_decode_requirements(
    runtime: &CandidateRuntimeRequirements,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    rejections: &mut Vec<CandidateRejectionReason>,
) -> Option<SupportedVideoOutput> {
    match runtime {
        CandidateRuntimeRequirements::Muxed { video, audio } => {
            let output = check_video(PlaybackComponent::Muxed, video, capabilities, rejections);
            check_audio(PlaybackComponent::Muxed, *audio, capabilities, rejections);
            output
        }
        CandidateRuntimeRequirements::Separate { video, audio } => {
            let output = check_video(PlaybackComponent::Video, video, capabilities, rejections);
            check_audio(PlaybackComponent::Audio, *audio, capabilities, rejections);
            output
        }
        CandidateRuntimeRequirements::VideoOnly { video } => {
            check_video(PlaybackComponent::Video, video, capabilities, rejections)
        }
        CandidateRuntimeRequirements::AudioOnly { audio } => {
            check_audio(PlaybackComponent::Audio, *audio, capabilities, rejections);
            None
        }
    }
}

/// Делегирует video intersection existing `SystemCapabilities` owner-у.
fn check_video(
    component: PlaybackComponent,
    requirement: &VideoDecodeRequirement,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    rejections: &mut Vec<CandidateRejectionReason>,
) -> Option<SupportedVideoOutput> {
    match capabilities.video().check_video_requirement(requirement) {
        Ok(output) => Some(output.clone()),
        Err(unsupported) => {
            rejections.push(CandidateRejectionReason::Capability(
                CandidateCapabilityRejection::Video {
                    component,
                    unsupported,
                },
            ));
            None
        }
    }
}

/// Делегирует audio query S20 snapshot-у без decoder construction.
fn check_audio(
    component: PlaybackComponent,
    family: AudioDecodeCodecFamily,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    rejections: &mut Vec<CandidateRejectionReason>,
) {
    let query = AudioDecodeCodecFamilyQuery::Known(family);
    match capabilities.audio().query(query) {
        Ok(AudioDecodeCapability::Available) => {}
        Ok(AudioDecodeCapability::Unavailable) => {
            rejections.push(CandidateRejectionReason::Capability(
                CandidateCapabilityRejection::AudioUnavailable { component, family },
            ));
        }
        Err(error) => {
            rejections.push(CandidateRejectionReason::Capability(
                CandidateCapabilityRejection::AudioQueryRejected { component, error },
            ));
        }
    }
}

/// Применяет HDR/codec policy только после полного capability query.
fn check_selection_policy(
    candidate: &PlanningCandidate,
    policy: &PlaybackSelectionPolicy,
    rejections: &mut Vec<CandidateRejectionReason>,
) {
    if let Some(dynamic_range) = candidate.dynamic_range() {
        match (policy.hdr(), dynamic_range) {
            (_, DynamicRange::Unknown) => rejections.push(CandidateRejectionReason::Policy(
                CandidatePolicyRejection::UnknownDynamicRange,
            )),
            (HdrSelectionPolicy::SdrOnly, DynamicRange::Hdr) => {
                rejections.push(CandidateRejectionReason::Policy(
                    CandidatePolicyRejection::HdrExcluded,
                ));
            }
            _ => {}
        }
    }

    if let Some(codec) = candidate.video_codec()
        && policy.video_codec_rank(Some(codec)).is_none()
    {
        rejections.push(CandidateRejectionReason::Policy(
            CandidatePolicyRejection::VideoCodecExcluded { codec },
        ));
    }
}

/// Сравнивает только уже playable candidates по D09/S20Q policy.
fn compare_playable(
    left: &CandidateEvaluation<'_>,
    right: &CandidateEvaluation<'_>,
    policy: &PlaybackSelectionPolicy,
) -> Ordering {
    av_completeness_rank(left.candidate)
        .cmp(&av_completeness_rank(right.candidate))
        .then_with(|| hdr_rank(left.candidate, policy).cmp(&hdr_rank(right.candidate, policy)))
        .then_with(|| {
            policy
                .video_codec_rank(left.candidate.video_codec())
                .cmp(&policy.video_codec_rank(right.candidate.video_codec()))
        })
        .then_with(|| {
            policy.preferred_height().compare(
                left.candidate.descriptor().layout().video_height(),
                right.candidate.descriptor().layout().video_height(),
            )
        })
        .then_with(|| {
            policy
                .container_rank(left.candidate.resources())
                .cmp(&policy.container_rank(right.candidate.resources()))
        })
        .then_with(|| {
            right
                .candidate
                .quality_score()
                .cmp(&left.candidate.quality_score())
        })
        .then_with(|| semantic_identity(left.candidate).cmp(semantic_identity(right.candidate)))
        .then_with(|| exact_identity(left.candidate).cmp(exact_identity(right.candidate)))
}

/// Полноценный A/V важнее предпочтений video codec/quality: silent fallback допустим
/// только когда playable A/V-кандидата действительно нет.
fn av_completeness_rank(candidate: &PlanningCandidate) -> u8 {
    match candidate.descriptor().layout().kind() {
        StreamLayoutKind::Muxed | StreamLayoutKind::Separate => 0,
        StreamLayoutKind::VideoOnly | StreamLayoutKind::AudioOnly => 1,
    }
}

/// Возвращает HDR bucket rank; policy-invalid candidates сюда уже не доходят.
fn hdr_rank(candidate: &PlanningCandidate, policy: &PlaybackSelectionPolicy) -> u8 {
    match (policy.hdr(), candidate.dynamic_range()) {
        (HdrSelectionPolicy::PreferHdrWhenAvailable, Some(DynamicRange::Hdr)) => 0,
        (HdrSelectionPolicy::PreferHdrWhenAvailable, Some(DynamicRange::Sdr)) => 1,
        _ => 0,
    }
}

/// Внутренний результат полной проверки одного candidate-а.
struct CandidateEvaluation<'candidate> {
    /// Borrowed immutable candidate.
    candidate: &'candidate PlanningCandidate,
    /// Existing matched video output proof, если layout содержит video.
    matched_video_output: Option<SupportedVideoOutput>,
    /// Все exact layer/policy rejections.
    rejection_reasons: Vec<CandidateRejectionReason>,
}

impl CandidateEvaluation<'_> {
    /// Превращает playable evaluation в owned plan.
    fn into_plan(self) -> PlaybackPlan {
        PlaybackPlan {
            exact_identity: exact_identity(self.candidate).clone(),
            semantic_identity: semantic_identity(self.candidate).clone(),
            layout: self.candidate.descriptor().layout().kind(),
            matched_video_output: self.matched_video_output,
        }
    }

    /// Превращает rejected evaluation в safe owned diagnostics.
    fn into_rejection(self) -> CandidateRejection {
        CandidateRejection {
            exact_identity: exact_identity(self.candidate).clone(),
            semantic_identity: semantic_identity(self.candidate).clone(),
            reasons: self.rejection_reasons.into_boxed_slice(),
        }
    }
}

/// Выбранный exact layout и capability proof до открытия ресурсов.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackPlan {
    /// Snapshot-local candidate identity для последующего exact open.
    exact_identity: CandidateIdentity,
    /// Semantic attributes, которые caller обязан сохранить рядом с Exact intent.
    semantic_identity: SemanticIdentity,
    /// Выбранная layout shape.
    layout: StreamLayoutKind,
    /// Existing matched decoder→renderer output, если layout содержит video.
    matched_video_output: Option<SupportedVideoOutput>,
}

impl PlaybackPlan {
    /// Возвращает exact candidate identity.
    pub const fn exact_identity(&self) -> &CandidateIdentity {
        &self.exact_identity
    }

    /// Возвращает semantic candidate identity.
    pub const fn semantic_identity(&self) -> &SemanticIdentity {
        &self.semantic_identity
    }

    /// Возвращает выбранную layout shape.
    pub const fn layout(&self) -> StreamLayoutKind {
        self.layout
    }

    /// Возвращает matched video output proof.
    pub const fn matched_video_output(&self) -> Option<&SupportedVideoOutput> {
        self.matched_video_output.as_ref()
    }
}

/// Успешный plan плюс diagnostics отклонённых соседних candidates.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackPlanningOutcome {
    /// Выбранный exact plan.
    selected: PlaybackPlan,
    /// Несовместимые соседи; они не блокируют выбранный candidate.
    rejected_candidates: Box<[CandidateRejection]>,
}

impl PlaybackPlanningOutcome {
    /// Возвращает выбранный plan.
    pub const fn selected(&self) -> &PlaybackPlan {
        &self.selected
    }

    /// Возвращает diagnostics отклонённых candidates.
    pub const fn rejected_candidates(&self) -> &[CandidateRejection] {
        &self.rejected_candidates
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
