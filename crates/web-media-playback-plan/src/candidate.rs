use std::collections::HashSet;

use audio_core::AudioDecodeCodecFamily;
use codec_core::{VideoCodec, VideoDecodeRequirement};
use web_media_core::{
    AudioTrackDescriptor, CandidateDescriptor, CandidateIdentity, CodecFamily, CodecKind,
    ContainerFamily, ContentProbedDescriptor, ContentProbedTrackEvidence, DynamicRange,
    ExtractionGeneration, HlsMuxedCodecDeferredDescriptor, MuxedComponentDescriptor,
    SemanticIdentity, SourceIdentity, StreamLayout, StreamLayoutKind, TransportFamily,
    VideoTrackDescriptor,
};

/// Stable service-provided quality score; большее значение предпочтительнее.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CandidateQualityScore(i64);

impl CandidateQualityScore {
    /// Создаёт score без интерпретации service-specific шкалы.
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Возвращает исходное значение для deterministic ordering.
    #[must_use]
    pub const fn value(self) -> i64 {
        self.0
    }
}

/// Runtime decode requirements, shape которых обязана совпадать с static layout.
#[derive(Debug, Clone, PartialEq)]
pub enum CandidateRuntimeRequirements {
    /// Один resource требует video и audio decode.
    Muxed {
        /// Полное existing video capability requirement.
        video: VideoDecodeRequirement,
        /// S20 audio codec family query.
        audio: AudioDecodeCodecFamily,
    },
    /// Раздельные resources требуют независимых decoder paths.
    Separate {
        /// Полное existing video capability requirement.
        video: VideoDecodeRequirement,
        /// S20 audio codec family query.
        audio: AudioDecodeCodecFamily,
    },
    /// Video-only resource.
    VideoOnly {
        /// Полное existing video capability requirement.
        video: VideoDecodeRequirement,
    },
    /// Audio-only resource.
    AudioOnly {
        /// S20 audio codec family query.
        audio: AudioDecodeCodecFamily,
    },
    /// Muxed HLS без static codec evidence; decode proof отложен до manifest open.
    HlsMuxedCodecDeferred,
    /// Single resource с частично известными requirements; остальные доказывает content probe.
    ContentProbed {
        /// Declared video requirement, если extractor действительно объявил video codec.
        video: Option<VideoDecodeRequirement>,
        /// Declared audio requirement, если extractor действительно объявил audio codec.
        audio: Option<AudioDecodeCodecFamily>,
    },
}

impl CandidateRuntimeRequirements {
    /// Возвращает layout kind без доступа к внутренним decode полям.
    #[must_use]
    pub const fn layout_kind(&self) -> StreamLayoutKind {
        match self {
            Self::Muxed { .. } => StreamLayoutKind::Muxed,
            Self::Separate { .. } => StreamLayoutKind::Separate,
            Self::VideoOnly { .. } => StreamLayoutKind::VideoOnly,
            Self::AudioOnly { .. } => StreamLayoutKind::AudioOnly,
            Self::HlsMuxedCodecDeferred => StreamLayoutKind::HlsMuxedCodecDeferred,
            Self::ContentProbed { .. } => StreamLayoutKind::ContentProbed,
        }
    }
}

/// Static resource evidence, уже очищенное от unknown/profile-excluded значений.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlanningResource {
    /// Transport family одного request component-а.
    pub(crate) transport: TransportFamily,
    /// Непротиворечивая container family одного request component-а.
    pub(crate) container: ContainerFamily,
}

/// Resource shape, совпадающая с `StreamLayout` candidate-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanningResourceLayout {
    /// Один muxed resource.
    Muxed(PlanningResource),
    /// Два независимых resource-а.
    Separate {
        /// Video-only resource.
        video: PlanningResource,
        /// Audio-only resource.
        audio: PlanningResource,
    },
    /// Один video-only resource.
    VideoOnly(PlanningResource),
    /// Один audio-only resource.
    AudioOnly(PlanningResource),
    /// Muxed HLS без pinned container; требует TS/fMP4 demux intersection.
    HlsMuxedCodecDeferred {
        /// Transport family одного HLS resource.
        transport: TransportFamily,
    },
    /// Single content-probed resource с provider-resolved demux container family.
    ContentProbed(PlanningResource),
}

/// Один statically-compatible candidate и его provider-neutral runtime requirements.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanningCandidate {
    /// S19 neutral descriptor с exact/semantic identities.
    descriptor: CandidateDescriptor,
    /// Existing decode requirements без decoder factory или mutable state.
    runtime: CandidateRuntimeRequirements,
    /// Service-owned deterministic quality rank.
    quality_score: CandidateQualityScore,
    /// Проверенное resource evidence для infallible pure planning.
    resources: PlanningResourceLayout,
    /// Canonical video codec для policy ordering, если candidate содержит video.
    video_codec: Option<VideoCodec>,
    /// Dynamic range для HDR bucket policy, если candidate содержит video.
    dynamic_range: Option<DynamicRange>,
}

impl PlanningCandidate {
    /// Проверяет static/runtime shape и codec correspondence до помещения в snapshot.
    pub fn new(
        descriptor: CandidateDescriptor,
        runtime: CandidateRuntimeRequirements,
        quality_score: CandidateQualityScore,
    ) -> Result<Self, PlanningCandidateBuildError> {
        if descriptor.layout().kind() != runtime.layout_kind() {
            return Err(PlanningCandidateBuildError::LayoutMismatch {
                descriptor: descriptor.layout().kind(),
                runtime: runtime.layout_kind(),
            });
        }

        let (resources, video_codec, dynamic_range) =
            validate_candidate_contract(descriptor.layout(), &runtime)?;

        Ok(Self {
            descriptor,
            runtime,
            quality_score,
            resources,
            video_codec,
            dynamic_range,
        })
    }

    /// Возвращает neutral candidate descriptor.
    pub const fn descriptor(&self) -> &CandidateDescriptor {
        &self.descriptor
    }

    /// Возвращает runtime decode requirements без создания decoder-а.
    pub const fn runtime_requirements(&self) -> &CandidateRuntimeRequirements {
        &self.runtime
    }

    /// Возвращает deterministic service quality score.
    pub const fn quality_score(&self) -> CandidateQualityScore {
        self.quality_score
    }

    /// Возвращает проверенную resource shape для planner-а.
    pub(crate) const fn resources(&self) -> PlanningResourceLayout {
        self.resources
    }

    /// Возвращает canonical video codec для policy ordering.
    pub(crate) const fn video_codec(&self) -> Option<VideoCodec> {
        self.video_codec
    }

    /// Возвращает dynamic range video track-а.
    pub(crate) const fn dynamic_range(&self) -> Option<DynamicRange> {
        self.dynamic_range
    }
}

/// Immutable extraction snapshot только из statically-compatible candidates.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanningCandidateSnapshot {
    /// Source lineage всего snapshot-а.
    source: SourceIdentity,
    /// Exact immutable extraction generation.
    generation: ExtractionGeneration,
    /// Candidate inventory без rejected/static rows.
    candidates: Box<[PlanningCandidate]>,
}

impl PlanningCandidateSnapshot {
    /// Проверяет source/generation каждого candidate-а и unique exact identities.
    pub fn new(
        source: SourceIdentity,
        generation: ExtractionGeneration,
        candidates: Vec<PlanningCandidate>,
    ) -> Result<Self, PlanningSnapshotBuildError> {
        let mut exact_identities = HashSet::with_capacity(candidates.len());
        for candidate in &candidates {
            let identity = candidate.descriptor().identity();
            if identity.source() != source {
                return Err(PlanningSnapshotBuildError::CandidateSourceMismatch);
            }
            if identity.generation() != generation {
                return Err(PlanningSnapshotBuildError::CandidateGenerationMismatch);
            }
            if !exact_identities.insert(identity.clone()) {
                return Err(PlanningSnapshotBuildError::DuplicateExactIdentity);
            }
        }

        Ok(Self {
            source,
            generation,
            candidates: candidates.into_boxed_slice(),
        })
    }

    /// Возвращает source lineage snapshot-а.
    pub const fn source(&self) -> SourceIdentity {
        self.source
    }

    /// Возвращает extraction generation snapshot-а.
    pub const fn generation(&self) -> ExtractionGeneration {
        self.generation
    }

    /// Возвращает immutable playable-input inventory.
    pub const fn candidates(&self) -> &[PlanningCandidate] {
        &self.candidates
    }
}

/// Ошибка admission statically-compatible candidate-а в planning boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanningCandidateBuildError {
    /// Static layout и runtime requirement shape расходятся.
    LayoutMismatch {
        /// Shape из S19 descriptor-а.
        descriptor: StreamLayoutKind,
        /// Shape runtime requirements.
        runtime: StreamLayoutKind,
    },
    /// Declared/unknown/absent track evidence не совпало с optional runtime requirement.
    ContentProbedRequirementMismatch {
        /// Дорожка, для которой потеряна correspondence.
        component: PlanningComponent,
    },
    /// Component содержит unknown либо profile-excluded transport family.
    StaticTransportRejected {
        /// Роль component-а внутри layout.
        component: PlanningComponent,
        /// Недопустимая family.
        family: TransportFamily,
    },
    /// Container hints конфликтуют или не дают одну family.
    UnresolvedContainer {
        /// Роль component-а внутри layout.
        component: PlanningComponent,
    },
    /// Container family должна была быть отсеяна static profile owner-ом.
    StaticContainerRejected {
        /// Роль component-а внутри layout.
        component: PlanningComponent,
        /// Недопустимая family.
        family: ContainerFamily,
    },
    /// Video descriptor codec не совпал с normalized runtime requirement.
    VideoCodecMismatch {
        /// Codec из descriptor-а.
        descriptor: CodecKind,
        /// Codec из existing `VideoDecodeRequirement`.
        runtime: VideoCodec,
    },
    /// Audio descriptor codec не совпал с S20 family.
    AudioCodecMismatch {
        /// Codec из descriptor-а.
        descriptor: CodecKind,
        /// S20 runtime family.
        runtime: AudioDecodeCodecFamily,
    },
    /// Typed SDR/HDR descriptor противоречит video requirement.
    DynamicRangeMismatch,
    /// Descriptor resolution расходится с capability requirement.
    VideoResolutionMismatch,
}

/// Component identity только для admission diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanningComponent {
    /// Единственный muxed resource.
    Muxed,
    /// Video-only resource.
    Video,
    /// Audio-only resource.
    Audio,
}

impl std::fmt::Display for PlanningCandidateBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "candidate не прошёл planning admission: {self:?}"
        )
    }
}

impl std::error::Error for PlanningCandidateBuildError {}

/// Ошибка сборки immutable candidate snapshot-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanningSnapshotBuildError {
    /// Candidate принадлежит другой source lineage.
    CandidateSourceMismatch,
    /// Candidate принадлежит другой extraction generation.
    CandidateGenerationMismatch,
    /// Snapshot содержит повторную exact identity.
    DuplicateExactIdentity,
}

impl std::fmt::Display for PlanningSnapshotBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "candidate snapshot недействителен: {self:?}")
    }
}

impl std::error::Error for PlanningSnapshotBuildError {}

/// Проверяет layout и возвращает pre-resolved resource/policy evidence.
fn validate_candidate_contract(
    layout: &StreamLayout,
    runtime: &CandidateRuntimeRequirements,
) -> Result<
    (
        PlanningResourceLayout,
        Option<VideoCodec>,
        Option<DynamicRange>,
    ),
    PlanningCandidateBuildError,
> {
    match (layout, runtime) {
        (StreamLayout::Muxed(component), CandidateRuntimeRequirements::Muxed { video, audio }) => {
            validate_video(component.video(), video)?;
            validate_audio(component.audio(), *audio)?;
            Ok((
                PlanningResourceLayout::Muxed(validate_muxed_resource(component)?),
                Some(video.codec),
                Some(component.video().dynamic_range()),
            ))
        }
        (
            StreamLayout::Separate { video, audio },
            CandidateRuntimeRequirements::Separate {
                video: video_requirement,
                audio: audio_requirement,
            },
        ) => {
            validate_video(video.video(), video_requirement)?;
            validate_audio(audio.audio(), *audio_requirement)?;
            Ok((
                PlanningResourceLayout::Separate {
                    video: validate_resource(
                        PlanningComponent::Video,
                        video.transport().family(),
                        video.container(),
                    )?,
                    audio: validate_resource(
                        PlanningComponent::Audio,
                        audio.transport().family(),
                        audio.container(),
                    )?,
                },
                Some(video_requirement.codec),
                Some(video.video().dynamic_range()),
            ))
        }
        (StreamLayout::VideoOnly(component), CandidateRuntimeRequirements::VideoOnly { video }) => {
            validate_video(component.video(), video)?;
            Ok((
                PlanningResourceLayout::VideoOnly(validate_resource(
                    PlanningComponent::Video,
                    component.transport().family(),
                    component.container(),
                )?),
                Some(video.codec),
                Some(component.video().dynamic_range()),
            ))
        }
        (StreamLayout::AudioOnly(component), CandidateRuntimeRequirements::AudioOnly { audio }) => {
            validate_audio(component.audio(), *audio)?;
            Ok((
                PlanningResourceLayout::AudioOnly(validate_resource(
                    PlanningComponent::Audio,
                    component.transport().family(),
                    component.container(),
                )?),
                None,
                None,
            ))
        }
        (
            StreamLayout::HlsMuxedCodecDeferred(component),
            CandidateRuntimeRequirements::HlsMuxedCodecDeferred,
        ) => validate_hls_muxed_codec_deferred(component),
        (
            StreamLayout::ContentProbed(component),
            CandidateRuntimeRequirements::ContentProbed { video, audio },
        ) => validate_content_probed(component, video.as_ref(), *audio),
        _ => Err(PlanningCandidateBuildError::LayoutMismatch {
            descriptor: layout.kind(),
            runtime: runtime.layout_kind(),
        }),
    }
}

/// Проверяет declared часть content-probed layout-а, не подменяя unknown evidence.
fn validate_content_probed(
    component: &ContentProbedDescriptor,
    video_requirement: Option<&VideoDecodeRequirement>,
    audio_requirement: Option<AudioDecodeCodecFamily>,
) -> Result<
    (
        PlanningResourceLayout,
        Option<VideoCodec>,
        Option<DynamicRange>,
    ),
    PlanningCandidateBuildError,
> {
    match (component.video(), video_requirement) {
        (ContentProbedTrackEvidence::Declared(video), Some(requirement)) => {
            validate_video(video, requirement)?;
        }
        (ContentProbedTrackEvidence::Unknown | ContentProbedTrackEvidence::Absent, None) => {}
        _ => {
            return Err(
                PlanningCandidateBuildError::ContentProbedRequirementMismatch {
                    component: PlanningComponent::Video,
                },
            );
        }
    }
    match (component.audio(), audio_requirement) {
        (ContentProbedTrackEvidence::Declared(audio), Some(requirement)) => {
            validate_audio(audio, requirement)?;
        }
        (ContentProbedTrackEvidence::Unknown | ContentProbedTrackEvidence::Absent, None) => {}
        _ => {
            return Err(
                PlanningCandidateBuildError::ContentProbedRequirementMismatch {
                    component: PlanningComponent::Audio,
                },
            );
        }
    }
    let resource = validate_resource_family(
        PlanningComponent::Muxed,
        component.transport().family(),
        component.probe_container(),
    )?;
    let dynamic_range = match component.video() {
        ContentProbedTrackEvidence::Declared(video) => Some(video.dynamic_range()),
        ContentProbedTrackEvidence::Unknown | ContentProbedTrackEvidence::Absent => None,
    };
    Ok((
        PlanningResourceLayout::ContentProbed(resource),
        video_requirement.map(|requirement| requirement.codec),
        dynamic_range,
    ))
}

/// Проверяет deferred HLS muxed layout без static codec correspondence.
fn validate_hls_muxed_codec_deferred(
    component: &HlsMuxedCodecDeferredDescriptor,
) -> Result<
    (
        PlanningResourceLayout,
        Option<VideoCodec>,
        Option<DynamicRange>,
    ),
    PlanningCandidateBuildError,
> {
    let transport = component.transport().family();
    if transport != TransportFamily::Hls {
        return Err(PlanningCandidateBuildError::StaticTransportRejected {
            component: PlanningComponent::Muxed,
            family: transport,
        });
    }
    Ok((
        PlanningResourceLayout::HlsMuxedCodecDeferred { transport },
        None,
        Some(component.dynamic_range()),
    ))
}

/// Проверяет один muxed resource.
fn validate_muxed_resource(
    component: &MuxedComponentDescriptor,
) -> Result<PlanningResource, PlanningCandidateBuildError> {
    validate_resource(
        PlanningComponent::Muxed,
        component.transport().family(),
        component.container(),
    )
}

/// Не пропускает static incompatibility в runtime planner.
fn validate_resource(
    component: PlanningComponent,
    transport: TransportFamily,
    container: &web_media_core::ContainerIdentity,
) -> Result<PlanningResource, PlanningCandidateBuildError> {
    let container = container
        .consistent_family()
        .map_err(|_| PlanningCandidateBuildError::UnresolvedContainer { component })?
        .ok_or(PlanningCandidateBuildError::UnresolvedContainer { component })?;
    validate_resource_family(component, transport, container)
}

/// Проверяет уже разрешённую provider-owned container family.
fn validate_resource_family(
    component: PlanningComponent,
    transport: TransportFamily,
    container: ContainerFamily,
) -> Result<PlanningResource, PlanningCandidateBuildError> {
    if matches!(
        transport,
        TransportFamily::KnownExcluded(_) | TransportFamily::Unknown
    ) {
        return Err(PlanningCandidateBuildError::StaticTransportRejected {
            component,
            family: transport,
        });
    }
    if matches!(
        container,
        ContainerFamily::MpegProgramStream
            | ContainerFamily::Avi
            | ContainerFamily::Asf
            | ContainerFamily::Unknown
    ) {
        return Err(PlanningCandidateBuildError::StaticContainerRejected {
            component,
            family: container,
        });
    }

    Ok(PlanningResource {
        transport,
        container,
    })
}

/// Проверяет neutral codec identity против existing video requirement.
fn validate_video(
    descriptor: &VideoTrackDescriptor,
    runtime: &VideoDecodeRequirement,
) -> Result<(), PlanningCandidateBuildError> {
    if video_codec(descriptor.codec().kind()) != Some(runtime.codec) {
        return Err(PlanningCandidateBuildError::VideoCodecMismatch {
            descriptor: descriptor.codec().kind(),
            runtime: runtime.codec,
        });
    }

    let runtime_is_hdr = runtime.requires_hdr_processing();
    if matches!(descriptor.dynamic_range(), DynamicRange::Hdr) != runtime_is_hdr
        && !matches!(descriptor.dynamic_range(), DynamicRange::Unknown)
    {
        return Err(PlanningCandidateBuildError::DynamicRangeMismatch);
    }

    let descriptor_height = descriptor.height().map(web_media_core::VideoHeight::pixels);
    if descriptor.width_pixels().is_some() && descriptor.width_pixels() != runtime.width
        || descriptor_height.is_some() && descriptor_height != runtime.height
    {
        return Err(PlanningCandidateBuildError::VideoResolutionMismatch);
    }

    Ok(())
}

/// Проверяет neutral codec identity против S20 audio family.
fn validate_audio(
    descriptor: &AudioTrackDescriptor,
    runtime: AudioDecodeCodecFamily,
) -> Result<(), PlanningCandidateBuildError> {
    if audio_codec(descriptor.codec().kind()) != Some(runtime) {
        return Err(PlanningCandidateBuildError::AudioCodecMismatch {
            descriptor: descriptor.codec().kind(),
            runtime,
        });
    }

    Ok(())
}

/// Маппит только доказанные video codec families без raw-string guessing.
const fn video_codec(codec: CodecKind) -> Option<VideoCodec> {
    match codec {
        CodecKind::Known(CodecFamily::Vp8) => Some(VideoCodec::Vp8),
        CodecKind::Known(CodecFamily::Vp9) => Some(VideoCodec::Vp9),
        CodecKind::Known(CodecFamily::Av1) => Some(VideoCodec::Av1),
        CodecKind::Known(CodecFamily::H264) => Some(VideoCodec::H264),
        CodecKind::Known(CodecFamily::H265) => Some(VideoCodec::H265),
        _ => None,
    }
}

/// Маппит только S20 exact audio codec families.
const fn audio_codec(codec: CodecKind) -> Option<AudioDecodeCodecFamily> {
    match codec {
        CodecKind::Known(CodecFamily::Aac) => Some(AudioDecodeCodecFamily::Aac),
        CodecKind::Known(CodecFamily::Adpcm) => Some(AudioDecodeCodecFamily::Adpcm),
        CodecKind::Known(CodecFamily::Alac) => Some(AudioDecodeCodecFamily::Alac),
        CodecKind::Known(CodecFamily::Flac) => Some(AudioDecodeCodecFamily::Flac),
        CodecKind::Known(CodecFamily::Mp1) => Some(AudioDecodeCodecFamily::Mp1),
        CodecKind::Known(CodecFamily::Mp2) => Some(AudioDecodeCodecFamily::Mp2),
        CodecKind::Known(CodecFamily::Mp3) => Some(AudioDecodeCodecFamily::Mp3),
        CodecKind::Known(CodecFamily::Opus) => Some(AudioDecodeCodecFamily::Opus),
        CodecKind::Known(CodecFamily::Pcm) => Some(AudioDecodeCodecFamily::Pcm),
        CodecKind::Known(CodecFamily::Vorbis) => Some(AudioDecodeCodecFamily::Vorbis),
        _ => None,
    }
}

/// Возвращает exact identity без раскрытия bounded format payload.
pub(crate) const fn exact_identity(candidate: &PlanningCandidate) -> &CandidateIdentity {
    candidate.descriptor().identity()
}

/// Возвращает semantic identity для deterministic final tie-break.
pub(crate) const fn semantic_identity(candidate: &PlanningCandidate) -> &SemanticIdentity {
    candidate.descriptor().semantic_identity()
}
