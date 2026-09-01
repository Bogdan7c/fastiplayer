//! Logical Representation lanes и neutral additive catalog projection.

mod build;
mod resolution;

pub(crate) use resolution::rematch_logical_selection;

use std::num::NonZeroUsize;

use dash_mpd_core::{
    DashAudioChannelConfiguration, DashColorMetadata, DashContainer, DashFrameRate, DashMediaKind,
    DashMpd, DashRepresentation,
};
use source_core::HttpRequestTarget;
use thiserror::Error;
use web_media_core::{
    AudioComponentVariant, AudioTrackDescriptor, Bitrate, ChannelCount, CodecKind, CodecMediaKind,
    ComponentKind, ComponentVariantCatalog, ComponentVariantCatalogEntries,
    ComponentVariantCatalogIdentity, ComponentVariantCatalogLimit,
    ComponentVariantCompatibilityEdge, ComponentVariantCompatibilityEntries,
    ComponentVariantEdgeLimit, ComponentVariantError, ComponentVariantExactIdentity,
    ComponentVariantExactKey, ComponentVariantKeyError, ComponentVariantSelection,
    ComponentVariantSelectionRequest, ComponentVariantSemanticIdentity,
    ComponentVariantSemanticKey, CoupledComponentVariant, CoupledVariantExactIdentity,
    CoupledVariantSemanticIdentity, DynamicRange, FrameRate, LanguageTag,
    MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES, NormalizedCodec, PreferredHeightPolicy, RawCodecIdentity,
    SampleRate, SemanticIdentity, VideoComponentVariant, VideoHeight, VideoTrackDescriptor,
    VideoWidth,
};

use crate::selection::{
    DashPresentationSelection, DashRepresentationEvidence, representation_matches,
};

/// Timeline profile, которым проверяются separate A/V compatibility edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashRepresentationLaneTimelineMode {
    /// Полное покрытие finite static Period обязательно.
    Static,
    /// Sliding head/tail dynamic snapshot допустимы.
    Dynamic,
}

/// Snapshot-local opaque lane identity для provider-owned proof operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DashRepresentationLaneProbeId(u64);

/// Safe proof request без URL, Representation id, parser row или source order.
#[derive(Clone, PartialEq, Eq)]
pub struct DashRepresentationLaneProbe {
    lane: DashRepresentationLaneProbeId,
    kind: DashMediaKind,
    pub(crate) logical_lane: DashLogicalRepresentationLane,
    pub(crate) contract: LaneContract,
}

impl std::fmt::Debug for DashRepresentationLaneProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DashRepresentationLaneProbe")
            .field("lane", &self.lane)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl DashRepresentationLaneProbe {
    /// Opaque identity текущего catalog build-а.
    pub const fn lane(&self) -> DashRepresentationLaneProbeId {
        self.lane
    }

    /// Required track shape logical lane-а.
    pub const fn kind(&self) -> DashMediaKind {
        self.kind
    }
}

/// Fully proven actual demux track shape после capability intersection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DashRepresentationLaneProof {
    /// Ровно один video track.
    VideoOnly(VideoTrackDescriptor),
    /// Ровно один audio track.
    AudioOnly(AudioTrackDescriptor),
    /// Coupled Representation содержит оба track-а.
    Muxed {
        /// Proven video track.
        video: VideoTrackDescriptor,
        /// Proven audio track.
        audio: AudioTrackDescriptor,
    },
}

/// Operational proof outcome отделяет isolatable sibling от whole-job fences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DashRepresentationLaneProbeError {
    /// Catalog job отменён.
    #[error("DASH representation lane proof cancelled")]
    Cancelled,
    /// Catalog/extraction generation стала stale.
    #[error("DASH representation lane proof generation is stale")]
    StaleGeneration,
    /// Transport resource lane-а недоступен.
    #[error("DASH representation lane transport is unavailable")]
    TransportUnavailable,
    /// Initial bytes/container не соответствуют profile.
    #[error("DASH representation lane container is unsupported")]
    UnsupportedContainer,
    /// Demux track shape не соответствует logical lane.
    #[error("DASH representation lane track shape is unsupported")]
    UnsupportedTrackShape,
    /// Codec/audio/video capability intersection отклонил lane.
    #[error("DASH representation lane capability is rejected")]
    CapabilityRejected,
    /// MPD metadata противоречит фактически probed track metadata.
    #[error("DASH representation lane manifest evidence conflicts with probed media")]
    ManifestEvidenceConflict,
}

/// Provider composition hook; вызывается ровно один раз на logical lane.
pub trait DashRepresentationLaneProofPort {
    /// Выполняет transport/content/demux/capability proof и возвращает neutral descriptors.
    fn prove_lane(
        &mut self,
        request: DashRepresentationLaneProbe,
    ) -> Result<DashRepresentationLaneProof, DashRepresentationLaneProbeError>;
}

/// Полный caller-owned request без скрытых catalog/edge/segment budgets.
pub struct DashRepresentationLaneCatalogBuildRequest<'request> {
    /// Checked MPD snapshot; serialized fragment input сюда не представим типом.
    pub presentation: &'request DashMpd,
    /// Effective MPD target нужен только existing planner-у, не semantic identity.
    pub manifest_base: &'request HttpRequestTarget,
    /// Exact parent и catalog generation.
    pub catalog_identity: ComponentVariantCatalogIdentity,
    /// Refresh-stable parent selection identity.
    pub parent_semantic: &'request SemanticIdentity,
    /// Exact extractor evidence либо native deterministic ranking policy.
    pub provider_default: DashRepresentationLaneProviderDefault<'request>,
    /// Additive row budget.
    pub catalog_limit: ComponentVariantCatalogLimit,
    /// Sparse compatibility edge budget.
    pub compatibility_edge_limit: ComponentVariantEdgeLimit,
    /// Existing addressing expansion budget для каждого proof.
    pub maximum_planned_segments: NonZeroUsize,
    /// Static/dynamic timeline semantics.
    pub timeline_mode: DashRepresentationLaneTimelineMode,
}

/// Источник default selection не смешивает extractor evidence и native ranking.
#[derive(Debug, Clone, Copy)]
pub enum DashRepresentationLaneProviderDefault<'presentation> {
    /// Existing extractor projection обязана exact-совпасть с proven lane.
    ExactEvidence(&'presentation DashPresentationSelection),
    /// Direct MPD выбирает полный playable layout из уже filtered catalog-а.
    NativePreferredHeight(PreferredHeightPolicy),
}

/// Почему отдельная structurally isolated sibling lane не опубликована.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashRepresentationLaneRejectionReason {
    /// Metadata не переводится в neutral proven descriptor.
    UnsupportedMetadata,
    /// Semantic contract отсутствует в одном из required Periods.
    MissingRequiredPeriod,
    /// Semantic contract повторён внутри required Period и не выбирается однозначно.
    AmbiguousRequiredPeriod,
    /// Provider proof не смог открыть resource.
    TransportUnavailable,
    /// Provider proof отверг container bytes.
    UnsupportedContainer,
    /// Provider proof отверг фактический track shape.
    UnsupportedTrackShape,
    /// Capability intersection не допускает lane.
    CapabilityRejected,
    /// Probed media противоречит MPD evidence.
    ManifestEvidenceConflict,
    /// Addressing/timeline lane-а нельзя открыть через existing DASH planner.
    TimelineIncompatible,
}

/// Safe bounded rejection без Representation id, URL или parser row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DashRepresentationLaneRejection {
    reason: DashRepresentationLaneRejectionReason,
    required_period_ordinal: usize,
}

impl DashRepresentationLaneRejection {
    /// Stable typed reason.
    pub const fn reason(&self) -> DashRepresentationLaneRejectionReason {
        self.reason
    }

    /// Required Period ordinal безопасен: это не semantic identity и не locator.
    pub const fn required_period_ordinal(&self) -> usize {
        self.required_period_ordinal
    }
}

/// Snapshot-local exact lane через каждый required Period.
#[derive(Clone, PartialEq, Eq)]
pub struct DashLogicalRepresentationLane {
    pub(crate) semantic_key: String,
    pub(crate) locations: Box<[(usize, usize)]>,
    pub(crate) contract: LaneContract,
}

impl std::fmt::Debug for DashLogicalRepresentationLane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DashLogicalRepresentationLane")
            .field("semantic_key", &"<sha256>")
            .field("required_period_count", &self.locations.len())
            .finish()
    }
}

impl DashLogicalRepresentationLane {
    /// Число Period, в которых lane доказана ровно один раз.
    pub fn required_period_count(&self) -> usize {
        self.locations.len()
    }
}

/// Provider-owned runtime projection neutral selection-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DashLogicalRepresentationSelection {
    /// Muxed/video-only/audio-only lane.
    Single(DashLogicalRepresentationLane),
    /// Только доказанная sparse pair.
    Separate {
        /// Video lane.
        video: DashLogicalRepresentationLane,
        /// Audio lane.
        audio: DashLogicalRepresentationLane,
    },
}

/// Ошибка exact neutral-to-provider resolution без fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DashRepresentationLaneSelectionError {
    /// Selection не принадлежит retained catalog либо row mapping отсутствует.
    #[error("DASH logical representation selection is absent")]
    Absent,
    /// Layout не соответствует provider lane kind.
    #[error("DASH logical representation selection layout is invalid")]
    Layout,
}

/// Полностью построенный neutral catalog плюс private provider mapping.
pub struct DashRepresentationLaneCatalog {
    catalog: ComponentVariantCatalog,
    provider_default: ComponentVariantSelection,
    rejections: Box<[DashRepresentationLaneRejection]>,
    runtime_rows: Box<[PublishedLane]>,
}

impl std::fmt::Debug for DashRepresentationLaneCatalog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DashRepresentationLaneCatalog")
            .field("catalog", &self.catalog)
            .field("provider_default", &self.provider_default)
            .field("rejection_count", &self.rejections.len())
            .finish_non_exhaustive()
    }
}

/// Typed catalog construction failure; variants не содержат MPD-provided strings.
#[derive(Debug, Error)]
pub enum DashRepresentationLaneCatalogBuildError {
    /// MPD без required Period не образует lane catalog.
    #[error("DASH representation lane catalog requires at least one Period")]
    EmptyPresentation,
    /// Ни одной structurally valid sibling lane не осталось.
    #[error("DASH representation lane catalog has no selectable lanes")]
    NoSelectableLane,
    /// Outer provider default исчез из logical inventory.
    #[error("DASH provider default logical lane is missing")]
    ProviderDefaultMissing,
    /// Outer provider default соответствует нескольким logical lanes.
    #[error("DASH provider default logical lane is ambiguous")]
    ProviderDefaultAmbiguous,
    /// Separate outer default не имеет proven compatibility edge.
    #[error("DASH provider default logical pair is incompatible")]
    ProviderDefaultIncompatible,
    /// Authoritative outer lane нельзя изолировать как обычный sibling.
    #[error("DASH provider default logical lane proof failed: {0}")]
    ProviderDefaultRejected(DashRepresentationLaneProbeError),
    /// Authoritative outer lane не образует exact runtime timeline.
    #[error("DASH provider default logical lane timeline is incompatible")]
    ProviderDefaultTimelineIncompatible,
    /// Catalog proof job отменён.
    #[error("DASH representation lane catalog proof cancelled")]
    Cancelled,
    /// Catalog proof generation стала stale.
    #[error("DASH representation lane catalog proof generation is stale")]
    StaleGeneration,
    /// Neutral catalog identity/budget invariant.
    #[error("DASH neutral representation catalog rejected: {0}")]
    Catalog(#[from] ComponentVariantError),
    /// Bounded opaque key construction.
    #[error("DASH logical representation key rejected")]
    Key(#[from] ComponentVariantKeyError),
    /// Potential compatibility relation превышает caller budget до scanning.
    #[error("DASH logical representation compatibility budget exceeded")]
    CompatibilityBudget,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LaneContract {
    pub(crate) kind: DashMediaKind,
    pub(crate) container: DashContainer,
    pub(crate) video_codec: Option<String>,
    pub(crate) audio_codec: Option<String>,
    pub(crate) bandwidth: Option<u64>,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) frame_rate: Option<DashFrameRate>,
    pub(crate) audio_sampling_rate: Option<u32>,
    pub(crate) audio_channel_configuration: Option<DashAudioChannelConfiguration>,
    pub(crate) language: Option<String>,
    pub(crate) color: DashColorMetadata,
}

struct LogicalLane {
    contract: LaneContract,
    lane: DashLogicalRepresentationLane,
}

struct ProvenLane {
    logical: LogicalLane,
    proof: DashRepresentationLaneProof,
}

struct PublishedLane {
    kind: DashMediaKind,
    lane: DashLogicalRepresentationLane,
    component_exact: Option<ComponentVariantExactIdentity>,
    coupled_exact: Option<CoupledVariantExactIdentity>,
}

/// Строит lane catalog атомарно; malformed sibling не уничтожает structurally safe соседей.
pub fn build_dash_representation_lane_catalog(
    request: DashRepresentationLaneCatalogBuildRequest<'_>,
    proof_port: &mut dyn DashRepresentationLaneProofPort,
) -> Result<DashRepresentationLaneCatalog, DashRepresentationLaneCatalogBuildError> {
    build::build(request, proof_port)
}

fn rejection(
    reason: DashRepresentationLaneRejectionReason,
    required_period_ordinal: usize,
) -> DashRepresentationLaneRejection {
    DashRepresentationLaneRejection {
        reason,
        required_period_ordinal,
    }
}

pub(crate) fn lane_contract(representation: &DashRepresentation) -> Result<LaneContract, ()> {
    if representation.audio_channel_configuration
        == Some(DashAudioChannelConfiguration::Unsupported)
    {
        return Err(());
    }
    let mut video_codec = None;
    let mut audio_codec = None;
    for raw in representation.codecs.split(',').map(str::trim) {
        let normalized = normalized_codec(raw)?;
        match normalized.kind() {
            CodecKind::Known(family) if family.media_kind() == CodecMediaKind::Video => {
                if video_codec.replace(raw.to_owned()).is_some() {
                    return Err(());
                }
            }
            CodecKind::Known(family) if family.media_kind() == CodecMediaKind::Audio => {
                if audio_codec.replace(raw.to_owned()).is_some() {
                    return Err(());
                }
            }
            _ => return Err(()),
        }
    }
    if !matches!(
        (
            representation.media_kind,
            video_codec.is_some(),
            audio_codec.is_some()
        ),
        (DashMediaKind::Video, true, false)
            | (DashMediaKind::Audio, false, true)
            | (DashMediaKind::Muxed, true, true)
    ) {
        return Err(());
    }
    let contract = LaneContract {
        kind: representation.media_kind,
        container: representation.container,
        video_codec,
        audio_codec,
        bandwidth: representation.bandwidth,
        width: representation.width,
        height: representation.height,
        frame_rate: representation.frame_rate,
        audio_sampling_rate: representation.audio_sampling_rate,
        audio_channel_configuration: representation.audio_channel_configuration,
        language: representation.language.clone(),
        color: representation.color,
    };
    match contract.kind {
        DashMediaKind::Video => {
            video_descriptor(&contract)?;
        }
        DashMediaKind::Audio => {
            audio_descriptor(&contract)?;
        }
        DashMediaKind::Muxed => {
            video_descriptor(&contract)?;
            audio_descriptor(&contract)?;
        }
    }
    Ok(contract)
}

pub(crate) fn normalized_codec(raw: &str) -> Result<NormalizedCodec, ()> {
    RawCodecIdentity::new(raw)
        .map(NormalizedCodec::parse)
        .map_err(|_| ())
}

fn provider_default_lane_keys(
    lanes: &[LogicalLane],
    presentation: &DashMpd,
    provider_default: &DashPresentationSelection,
) -> Result<Vec<String>, DashRepresentationLaneCatalogBuildError> {
    match provider_default {
        DashPresentationSelection::Single { main } => Ok(vec![
            unique_default_logical_lane(lanes, presentation, main)?
                .lane
                .semantic_key
                .clone(),
        ]),
        DashPresentationSelection::Separate { video, audio } => Ok(vec![
            unique_default_logical_lane(lanes, presentation, video)?
                .lane
                .semantic_key
                .clone(),
            unique_default_logical_lane(lanes, presentation, audio)?
                .lane
                .semantic_key
                .clone(),
        ]),
    }
}

fn unique_default_logical_lane<'lanes>(
    lanes: &'lanes [LogicalLane],
    presentation: &DashMpd,
    evidence: &DashRepresentationEvidence,
) -> Result<&'lanes LogicalLane, DashRepresentationLaneCatalogBuildError> {
    let mut matches = lanes
        .iter()
        .filter(|lane| lane_matches_evidence(&lane.lane, presentation, evidence));
    let first = matches
        .next()
        .ok_or(DashRepresentationLaneCatalogBuildError::ProviderDefaultMissing)?;
    if matches.next().is_some() {
        return Err(DashRepresentationLaneCatalogBuildError::ProviderDefaultAmbiguous);
    }
    Ok(first)
}

fn lane_matches_evidence(
    lane: &DashLogicalRepresentationLane,
    presentation: &DashMpd,
    evidence: &DashRepresentationEvidence,
) -> bool {
    lane.locations.len() == presentation.periods.len()
        && presentation.periods.iter().zip(&lane.locations).all(
            |(period, &(adaptation_index, representation_index))| {
                period
                    .adaptation_sets
                    .get(adaptation_index)
                    .and_then(|adaptation| adaptation.representations.get(representation_index))
                    .is_some_and(|representation| representation_matches(representation, evidence))
            },
        )
}

fn probe_rejection(
    error: DashRepresentationLaneProbeError,
) -> DashRepresentationLaneRejectionReason {
    match error {
        DashRepresentationLaneProbeError::TransportUnavailable => {
            DashRepresentationLaneRejectionReason::TransportUnavailable
        }
        DashRepresentationLaneProbeError::UnsupportedContainer => {
            DashRepresentationLaneRejectionReason::UnsupportedContainer
        }
        DashRepresentationLaneProbeError::UnsupportedTrackShape => {
            DashRepresentationLaneRejectionReason::UnsupportedTrackShape
        }
        DashRepresentationLaneProbeError::CapabilityRejected => {
            DashRepresentationLaneRejectionReason::CapabilityRejected
        }
        DashRepresentationLaneProbeError::ManifestEvidenceConflict => {
            DashRepresentationLaneRejectionReason::ManifestEvidenceConflict
        }
        DashRepresentationLaneProbeError::Cancelled
        | DashRepresentationLaneProbeError::StaleGeneration => {
            unreachable!("whole-job proof fences are handled before sibling rejection")
        }
    }
}

fn proof_matches_contract(proof: &DashRepresentationLaneProof, contract: &LaneContract) -> bool {
    match (proof, contract.kind) {
        (DashRepresentationLaneProof::VideoOnly(video), DashMediaKind::Video) => {
            video_metadata_matches(video, contract)
        }
        (DashRepresentationLaneProof::AudioOnly(audio), DashMediaKind::Audio) => {
            audio_metadata_matches(audio, contract)
        }
        (DashRepresentationLaneProof::Muxed { video, audio }, DashMediaKind::Muxed) => {
            video_metadata_matches(video, contract) && audio_metadata_matches(audio, contract)
        }
        _ => false,
    }
}

fn video_metadata_matches(video: &VideoTrackDescriptor, contract: &LaneContract) -> bool {
    let Ok(expected) = video_descriptor(contract) else {
        return false;
    };
    video.codec().raw().as_str() == expected.codec().raw().as_str()
        && expected
            .width_pixels()
            .is_none_or(|width| video.width_pixels() == Some(width))
        && expected
            .height()
            .is_none_or(|height| video.height() == Some(height))
        && expected
            .frame_rate()
            .is_none_or(|frame_rate| video.frame_rate() == Some(frame_rate))
        && expected
            .bitrate()
            .is_none_or(|bitrate| video.bitrate() == Some(bitrate))
        && (expected.dynamic_range() == DynamicRange::Unknown
            || video.dynamic_range() == expected.dynamic_range())
}

fn audio_metadata_matches(audio: &AudioTrackDescriptor, contract: &LaneContract) -> bool {
    let Ok(expected) = audio_descriptor(contract) else {
        return false;
    };
    audio.codec().raw().as_str() == expected.codec().raw().as_str()
        && expected
            .sample_rate()
            .is_none_or(|sample_rate| audio.sample_rate() == Some(sample_rate))
        && expected
            .channels()
            .is_none_or(|channels| audio.channels() == Some(channels))
        && expected
            .bitrate()
            .is_none_or(|bitrate| audio.bitrate() == Some(bitrate))
        && expected
            .language()
            .is_none_or(|language| audio.language() == Some(language))
}

pub(crate) fn video_descriptor(contract: &LaneContract) -> Result<VideoTrackDescriptor, ()> {
    let codec = normalized_codec(contract.video_codec.as_deref().ok_or(())?)?;
    let width = contract
        .width
        .map(VideoWidth::new)
        .transpose()
        .map_err(|_| ())?;
    let height = contract
        .height
        .map(VideoHeight::new)
        .transpose()
        .map_err(|_| ())?;
    let frame_rate = contract
        .frame_rate
        .map(|rate| FrameRate::new(rate.numerator, rate.denominator))
        .transpose()
        .map_err(|_| ())?;
    let bitrate = (contract.kind == DashMediaKind::Video)
        .then_some(contract.bandwidth)
        .flatten()
        .map(Bitrate::new)
        .transpose()
        .map_err(|_| ())?;
    Ok(VideoTrackDescriptor::new(
        codec,
        width,
        height,
        frame_rate,
        bitrate,
        dynamic_range(contract.color),
    ))
}

pub(crate) fn audio_descriptor(contract: &LaneContract) -> Result<AudioTrackDescriptor, ()> {
    let codec = normalized_codec(contract.audio_codec.as_deref().ok_or(())?)?;
    let sample_rate = contract
        .audio_sampling_rate
        .map(SampleRate::new)
        .transpose()
        .map_err(|_| ())?;
    let channels = channel_count(contract.audio_channel_configuration)
        .map(ChannelCount::new)
        .transpose()
        .map_err(|_| ())?;
    let bitrate = (contract.kind == DashMediaKind::Audio)
        .then_some(contract.bandwidth)
        .flatten()
        .map(Bitrate::new)
        .transpose()
        .map_err(|_| ())?;
    let language = contract
        .language
        .clone()
        .map(LanguageTag::new)
        .transpose()
        .map_err(|_| ())?;
    Ok(AudioTrackDescriptor::new(
        codec,
        sample_rate,
        channels,
        bitrate,
        language,
    ))
}

pub(crate) fn channel_count(configuration: Option<DashAudioChannelConfiguration>) -> Option<u16> {
    match configuration {
        Some(DashAudioChannelConfiguration::Mpeg23003_3(value @ 1..=6)) => Some(value),
        Some(DashAudioChannelConfiguration::Mpeg23003_3(7)) => Some(8),
        _ => None,
    }
}

pub(crate) fn dynamic_range(color: DashColorMetadata) -> DynamicRange {
    match color.transfer_characteristics {
        Some(16 | 18) => DynamicRange::Hdr,
        Some(1 | 4..=15) => DynamicRange::Sdr,
        _ => DynamicRange::Unknown,
    }
}

fn semantic_key(contract: &LaneContract) -> Option<String> {
    let mut canonical = String::from("rustiplayer-dash-lane-v1|");
    use std::fmt::Write as _;
    write!(
        &mut canonical,
        "{:?}|{:?}|",
        contract.kind, contract.container
    )
    .expect("String formatting cannot fail");
    for value in [
        &contract.video_codec,
        &contract.audio_codec,
        &contract.language,
    ] {
        write!(
            &mut canonical,
            "{}:{}|",
            value.as_deref().map_or(0, str::len),
            value.as_deref().unwrap_or_default()
        )
        .expect("String formatting cannot fail");
    }
    write!(
        &mut canonical,
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
        contract.bandwidth,
        contract.width,
        contract.height,
        contract.frame_rate,
        contract.audio_sampling_rate,
        contract.audio_channel_configuration,
        contract.color.colour_primaries,
        contract.color.transfer_characteristics,
        (
            contract.color.matrix_coefficients,
            contract.color.video_full_range
        )
    )
    .expect("String formatting cannot fail");

    (canonical.len() <= MAX_COMPONENT_VARIANT_KEY_UTF8_BYTES).then_some(canonical)
}

fn provider_default_selection(
    catalog: &ComponentVariantCatalog,
    rows: &[PublishedLane],
    presentation: &DashMpd,
    provider_default: &DashPresentationSelection,
) -> Result<ComponentVariantSelection, DashRepresentationLaneCatalogBuildError> {
    match provider_default {
        DashPresentationSelection::Single { main } => {
            let row = unique_default_row(rows, presentation, main)?;
            let request = match row.kind {
                DashMediaKind::Video => ComponentVariantSelectionRequest::VideoOnly {
                    video: row
                        .component_exact
                        .clone()
                        .expect("video default invariant"),
                },
                DashMediaKind::Audio => ComponentVariantSelectionRequest::AudioOnly {
                    audio: row
                        .component_exact
                        .clone()
                        .expect("audio default invariant"),
                },
                DashMediaKind::Muxed => ComponentVariantSelectionRequest::Coupled {
                    presentation: row.coupled_exact.clone().expect("muxed default invariant"),
                },
            };
            catalog.select_exact(request).map_err(Into::into)
        }
        DashPresentationSelection::Separate { video, audio } => {
            let video = unique_default_row(rows, presentation, video)?;
            let audio = unique_default_row(rows, presentation, audio)?;
            if video.kind != DashMediaKind::Video || audio.kind != DashMediaKind::Audio {
                return Err(DashRepresentationLaneCatalogBuildError::ProviderDefaultMissing);
            }
            catalog
                .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
                    video: video
                        .component_exact
                        .clone()
                        .expect("video default invariant"),
                    audio: audio
                        .component_exact
                        .clone()
                        .expect("audio default invariant"),
                })
                .map_err(|error| match error {
                    ComponentVariantError::IncompatibleComponentPair => {
                        DashRepresentationLaneCatalogBuildError::ProviderDefaultIncompatible
                    }
                    other => DashRepresentationLaneCatalogBuildError::Catalog(other),
                })
        }
    }
}

/// Выбирает native default только из реально опубликованных selectable relations.
///
/// Приоритет сохраняет полную presentation: proven separate A/V, затем coupled,
/// затем честный video-only и audio-only fallback. Пары никогда не строятся как
/// Cartesian rows: каждая separate selection проверяется catalog compatibility.
fn native_provider_default_selection(
    catalog: &ComponentVariantCatalog,
    preferred_height: PreferredHeightPolicy,
) -> Result<ComponentVariantSelection, DashRepresentationLaneCatalogBuildError> {
    let mut ranked_video = catalog
        .required_video_variants()
        .map_or_else(|_| Vec::new(), |video| video.iter().collect::<Vec<_>>());
    ranked_video.sort_by(|left, right| {
        preferred_height.compare(left.track().height(), right.track().height())
    });

    if let (Some(compatibility), Ok(audio)) =
        (catalog.compatibility(), catalog.required_audio_variants())
    {
        for video in &ranked_video {
            if let Some(audio) = audio
                .iter()
                .find(|audio| compatibility.allows(video.exact_identity(), audio.exact_identity()))
            {
                return catalog
                    .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
                        video: video.exact_identity().clone(),
                        audio: audio.exact_identity().clone(),
                    })
                    .map_err(Into::into);
            }
        }
    }

    let coupled = catalog
        .coupled_presentations()
        .iter()
        .min_by(|left, right| {
            preferred_height.compare(left.video().height(), right.video().height())
        });
    if let Some(coupled) = coupled {
        return catalog
            .select_exact(ComponentVariantSelectionRequest::Coupled {
                presentation: coupled.exact_identity().clone(),
            })
            .map_err(Into::into);
    }

    if let Some(video) = ranked_video
        .into_iter()
        .find(|video| catalog.is_video_only_selectable(video.exact_identity()))
    {
        return catalog
            .select_exact(ComponentVariantSelectionRequest::VideoOnly {
                video: video.exact_identity().clone(),
            })
            .map_err(Into::into);
    }

    if let Ok(audio) = catalog.required_audio_variants()
        && let Some(audio) = audio
            .iter()
            .find(|audio| catalog.is_audio_only_selectable(audio.exact_identity()))
    {
        return catalog
            .select_exact(ComponentVariantSelectionRequest::AudioOnly {
                audio: audio.exact_identity().clone(),
            })
            .map_err(Into::into);
    }

    Err(DashRepresentationLaneCatalogBuildError::NoSelectableLane)
}

fn unique_default_row<'rows>(
    rows: &'rows [PublishedLane],
    presentation: &DashMpd,
    evidence: &DashRepresentationEvidence,
) -> Result<&'rows PublishedLane, DashRepresentationLaneCatalogBuildError> {
    let mut matches = rows
        .iter()
        .filter(|row| lane_matches_evidence(&row.lane, presentation, evidence));
    let first = matches
        .next()
        .ok_or(DashRepresentationLaneCatalogBuildError::ProviderDefaultMissing)?;
    if matches.next().is_some() {
        return Err(DashRepresentationLaneCatalogBuildError::ProviderDefaultAmbiguous);
    }
    Ok(first)
}
