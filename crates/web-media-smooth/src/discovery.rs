//! Bounded sibling proof, atomic AllPairs publication и provider-owned reopen.

use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Arc;

use demux_api::{
    DemuxSniffBudget, OrderedSegment, OrderedSegmentKind, OrderedSegmentReadError,
    OrderedSegmentSource, PresentationWindowOrderedSegment,
    PresentationWindowOrderedSegmentReadOutcome, PresentationWindowOrderedSegmentSource,
};
use media_core::{DemuxReadEvent, Demuxer, TrackInfo, TrackKind};
use source_core::CancellationToken;
use web_media_core::{
    ComponentKind, ComponentVariantCatalog, ComponentVariantError, ComponentVariantSelection,
    ComponentVariantSelectionRequest, ComponentVariantSemanticSelectionRequest,
};

use crate::catalog::{
    PendingAudioRow, PendingVideoRow, SmoothCatalogBuildRequest, build_catalog_candidates,
    publish_catalog,
};
use crate::demux::{SmoothIsoBmffDemuxFactory, SmoothVodDemuxBuildError, SmoothVodDemuxPolicy};
use crate::error::{SmoothPrepareError, SmoothSiblingRejection, SmoothSiblingRejectionReason};
use crate::model::{SmoothPreparedCatalog, SmoothRuntimeSeed};
use crate::prepare::{SmoothManifestPreparation, into_prepared_catalog, prepare_manifest};
use crate::source::{
    SmoothAudioFragmentSource, SmoothFragmentSourceBuildError, SmoothFragmentSourcePolicy,
    SmoothVideoFragmentSource, build_audio_probe_source, build_video_probe_source,
};
use crate::{
    SmoothAudioDemuxOpenRequest, SmoothPrepareRequest, SmoothVideoDemuxOpenRequest,
    SmoothVodOpenResult,
};

/// Safe capability rejection без backend diagnostics или codec-private payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmoothComponentCapabilityRejection;

/// Immutable composition adapter для exact demux track capability intersection.
pub trait SmoothComponentCapabilityProbe: Send + Sync {
    /// Проверяет independently publishable video quality.
    fn check_video(&self, track: &TrackInfo) -> Result<(), SmoothComponentCapabilityRejection>;

    /// Проверяет independently publishable audio quality.
    fn check_audio(&self, track: &TrackInfo) -> Result<(), SmoothComponentCapabilityRejection>;
}

/// Caller-owned bounds first-fragment demux proof-а.
#[derive(Clone, Debug)]
pub struct SmoothCatalogDiscoveryPolicy {
    fragment_source: SmoothFragmentSourcePolicy,
    sniff_budget: DemuxSniffBudget,
    maximum_probe_events: NonZeroUsize,
}

impl SmoothCatalogDiscoveryPolicy {
    /// Собирает discovery policy без скрытых transport/content/demux limits.
    #[must_use]
    pub const fn new(
        fragment_source: SmoothFragmentSourcePolicy,
        sniff_budget: DemuxSniffBudget,
        maximum_probe_events: NonZeroUsize,
    ) -> Self {
        Self {
            fragment_source,
            sniff_budget,
            maximum_probe_events,
        }
    }
}

/// Synchronous request; composition запускает его на bounded background worker-е.
pub struct SmoothCatalogDiscoveryRequest<'config, 'capabilities> {
    preparation: SmoothPrepareRequest<'config>,
    demux_factory: Arc<dyn SmoothIsoBmffDemuxFactory>,
    capability_probe: &'capabilities dyn SmoothComponentCapabilityProbe,
    policy: SmoothCatalogDiscoveryPolicy,
}

impl<'config, 'capabilities> SmoothCatalogDiscoveryRequest<'config, 'capabilities> {
    /// Создаёт полный discovery request с named proof owners.
    #[must_use]
    pub fn new(
        preparation: SmoothPrepareRequest<'config>,
        demux_factory: Arc<dyn SmoothIsoBmffDemuxFactory>,
        capability_probe: &'capabilities dyn SmoothComponentCapabilityProbe,
        policy: SmoothCatalogDiscoveryPolicy,
    ) -> Self {
        Self {
            preparation,
            demux_factory,
            capability_probe,
            policy,
        }
    }
}

/// Полный fresh catalog плюс private manifest/HTTP/init rows для exact reopen.
pub struct SmoothDiscoveredCatalog {
    prepared: SmoothPreparedCatalog,
    rejections: Box<[SmoothSiblingRejection]>,
    demux_factory: Arc<dyn SmoothIsoBmffDemuxFactory>,
}

impl fmt::Debug for SmoothDiscoveredCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothDiscoveredCatalog")
            .field("catalog_identity", self.prepared.catalog().identity())
            .field(
                "published_rows",
                &self.prepared.catalog().stored_variant_count(),
            )
            .field("rejected_rows", &self.rejections.len())
            .finish_non_exhaustive()
    }
}

impl SmoothDiscoveredCatalog {
    /// Возвращает atomically published additive AllPairs catalog.
    #[must_use]
    pub const fn catalog(&self) -> &ComponentVariantCatalog {
        self.prepared.catalog()
    }

    /// Возвращает лучший proven provider selection текущего generation.
    #[must_use]
    pub const fn provider_default_selection(&self) -> &ComponentVariantSelection {
        self.prepared.provider_default_selection()
    }

    /// Возвращает bounded safe diagnostics изолированных qualities.
    #[must_use]
    pub fn sibling_rejections(&self) -> &[SmoothSiblingRejection] {
        &self.rejections
    }

    /// Exact reopen не допускает fallback и использует private rows этого catalog.
    pub fn open_exact(
        self,
        request: ComponentVariantSelectionRequest,
        fragment_policy: SmoothFragmentSourcePolicy,
        demux_policy: SmoothVodDemuxPolicy,
    ) -> Result<SmoothVodOpenResult, SmoothCatalogReopenError> {
        let selection = self
            .prepared
            .catalog()
            .select_exact(request)
            .map_err(SmoothCatalogReopenError::Selection)?;
        self.open_selection(selection, fragment_policy, demux_policy)
    }

    /// Semantic reopen rematch-ит только fresh identities без default fallback.
    pub fn open_semantic(
        self,
        request: ComponentVariantSemanticSelectionRequest,
        fragment_policy: SmoothFragmentSourcePolicy,
        demux_policy: SmoothVodDemuxPolicy,
    ) -> Result<SmoothVodOpenResult, SmoothCatalogReopenError> {
        let selection = self
            .prepared
            .catalog()
            .rematch_semantic(request)
            .map_err(SmoothCatalogReopenError::Selection)?;
        self.open_selection(selection, fragment_policy, demux_policy)
    }

    fn open_selection(
        self,
        selection: ComponentVariantSelection,
        fragment_policy: SmoothFragmentSourcePolicy,
        demux_policy: SmoothVodDemuxPolicy,
    ) -> Result<SmoothVodOpenResult, SmoothCatalogReopenError> {
        let demux_factory = Arc::clone(&self.demux_factory);
        self.prepared
            .into_selected_fragment_sources(selection, fragment_policy)
            .map_err(SmoothCatalogReopenError::Sources)?
            .into_progressive_demuxer(demux_factory, demux_policy)
            .map_err(SmoothCatalogReopenError::Demux)
    }
}

/// Fatal whole-job discovery failure; ordinary sibling defects остаются diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum SmoothCatalogDiscoveryError {
    /// Manifest/default-independent preparation boundary failed.
    #[error("Smooth catalog manifest preparation failed")]
    Preparation(#[source] SmoothPrepareError),
    /// Cancellation during row proof discards the complete unpublished snapshot.
    #[error("Smooth catalog discovery was cancelled")]
    Cancelled,
    /// После isolation одна required component axis пуста.
    #[error("Smooth catalog has no publishable {component:?} quality")]
    NoPublishableAxis { component: ComponentKind },
    /// Final neutral AllPairs publication rejected bounds/identity.
    #[error("Smooth catalog atomic publication failed")]
    Publication(#[source] SmoothPrepareError),
}

/// Exact/semantic provider reopen failure без fallback.
#[derive(Debug, thiserror::Error)]
pub enum SmoothCatalogReopenError {
    /// Requested selection отсутствует или несовместима в retained catalog.
    #[error("Smooth discovered selection is unavailable")]
    Selection(#[source] ComponentVariantError),
    /// Private row нельзя преобразовать в fragment sources.
    #[error("Smooth discovered component sources failed")]
    Sources(#[source] SmoothFragmentSourceBuildError),
    /// Nonblocking receipted runtime не запустился.
    #[error("Smooth discovered demux runtime failed")]
    Demux(#[source] SmoothVodDemuxBuildError),
}

/// Доказывает каждый sibling и публикует catalog только после полного pass-а.
pub fn discover_smooth_vod_catalog(
    request: SmoothCatalogDiscoveryRequest<'_, '_>,
) -> Result<SmoothDiscoveredCatalog, SmoothCatalogDiscoveryError> {
    let prepared =
        prepare_manifest(request.preparation).map_err(SmoothCatalogDiscoveryError::Preparation)?;
    let cancellation = prepared.http.cancellation().clone();
    let is_cancelled = || cancellation.is_cancelled();
    let build_request = catalog_build_request(&prepared, &is_cancelled);
    let candidates = build_catalog_candidates(&build_request)
        .map_err(SmoothCatalogDiscoveryError::Preparation)?;
    let probe_seed = runtime_seed_for_candidates(&prepared, &candidates);
    let mut rejections = candidates.rejections;
    let video_rows = prove_video_rows(
        candidates.video_rows,
        &probe_seed,
        request.demux_factory.as_ref(),
        request.capability_probe,
        &request.policy,
        &mut rejections,
    )?;
    let audio_rows = prove_audio_rows(
        candidates.audio_rows,
        &probe_seed,
        request.demux_factory.as_ref(),
        request.capability_probe,
        &request.policy,
        &mut rejections,
    )?;
    if video_rows.is_empty() {
        return Err(SmoothCatalogDiscoveryError::NoPublishableAxis {
            component: ComponentKind::Video,
        });
    }
    if audio_rows.is_empty() {
        return Err(SmoothCatalogDiscoveryError::NoPublishableAxis {
            component: ComponentKind::Audio,
        });
    }
    if cancellation.is_cancelled() {
        return Err(SmoothCatalogDiscoveryError::Cancelled);
    }
    let publication_cancelled = || cancellation.is_cancelled();
    let build_request = catalog_build_request(&prepared, &publication_cancelled);
    let catalog_build = publish_catalog(build_request, video_rows, audio_rows)
        .map_err(SmoothCatalogDiscoveryError::Publication)?;
    if cancellation.is_cancelled() {
        return Err(SmoothCatalogDiscoveryError::Cancelled);
    }
    Ok(SmoothDiscoveredCatalog {
        prepared: into_prepared_catalog(prepared, catalog_build),
        rejections: rejections.into_boxed_slice(),
        demux_factory: request.demux_factory,
    })
}

fn catalog_build_request<'a>(
    prepared: &'a SmoothManifestPreparation,
    cancellation: &'a dyn Fn() -> bool,
) -> SmoothCatalogBuildRequest<'a> {
    SmoothCatalogBuildRequest {
        manifest: &prepared.manifest,
        catalog_identity: prepared.catalog_identity.clone(),
        parent_semantic: &prepared.parent_semantic,
        video_stream_ordinal: prepared.video_stream_ordinal,
        audio_stream_ordinal: prepared.audio_stream_ordinal,
        preferred_height: prepared.preferred_height,
        policy: &prepared.policy,
        cancellation,
    }
}

fn runtime_seed_for_candidates(
    prepared: &SmoothManifestPreparation,
    candidates: &crate::catalog::SmoothCatalogCandidates,
) -> SmoothRuntimeSeed {
    SmoothRuntimeSeed {
        http: prepared.http.clone(),
        effective_manifest_target: prepared.effective_manifest_target.clone(),
        fragment_secret_forwarding: prepared.fragment_secret_forwarding,
        manifest: Arc::clone(&prepared.manifest),
        video_rows: candidates
            .video_rows
            .iter()
            .map(|row| row.runtime.clone())
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        audio_rows: candidates
            .audio_rows
            .iter()
            .map(|row| row.runtime.clone())
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    }
}

fn prove_video_rows(
    rows: Vec<PendingVideoRow>,
    seed: &SmoothRuntimeSeed,
    factory: &dyn SmoothIsoBmffDemuxFactory,
    capabilities: &dyn SmoothComponentCapabilityProbe,
    policy: &SmoothCatalogDiscoveryPolicy,
    rejections: &mut Vec<SmoothSiblingRejection>,
) -> Result<Vec<PendingVideoRow>, SmoothCatalogDiscoveryError> {
    let mut admitted = Vec::with_capacity(rows.len());
    for row in rows {
        if seed.http.cancellation().is_cancelled() {
            return Err(SmoothCatalogDiscoveryError::Cancelled);
        }
        match prove_video_row(&row, seed, factory, capabilities, policy) {
            Ok(()) => admitted.push(row),
            Err(_) if seed.http.cancellation().is_cancelled() => {
                return Err(SmoothCatalogDiscoveryError::Cancelled);
            }
            Err(reason) => {
                rejections.push(SmoothSiblingRejection::new(ComponentKind::Video, reason))
            }
        }
    }
    Ok(admitted)
}

fn prove_audio_rows(
    rows: Vec<PendingAudioRow>,
    seed: &SmoothRuntimeSeed,
    factory: &dyn SmoothIsoBmffDemuxFactory,
    capabilities: &dyn SmoothComponentCapabilityProbe,
    policy: &SmoothCatalogDiscoveryPolicy,
    rejections: &mut Vec<SmoothSiblingRejection>,
) -> Result<Vec<PendingAudioRow>, SmoothCatalogDiscoveryError> {
    let mut admitted = Vec::with_capacity(rows.len());
    for row in rows {
        if seed.http.cancellation().is_cancelled() {
            return Err(SmoothCatalogDiscoveryError::Cancelled);
        }
        match prove_audio_row(&row, seed, factory, capabilities, policy) {
            Ok(()) => admitted.push(row),
            Err(_) if seed.http.cancellation().is_cancelled() => {
                return Err(SmoothCatalogDiscoveryError::Cancelled);
            }
            Err(reason) => {
                rejections.push(SmoothSiblingRejection::new(ComponentKind::Audio, reason))
            }
        }
    }
    Ok(admitted)
}

fn prove_video_row(
    row: &PendingVideoRow,
    seed: &SmoothRuntimeSeed,
    factory: &dyn SmoothIsoBmffDemuxFactory,
    capabilities: &dyn SmoothComponentCapabilityProbe,
    policy: &SmoothCatalogDiscoveryPolicy,
) -> Result<(), SmoothSiblingRejectionReason> {
    let source = build_video_probe_source(seed, &row.runtime, policy.fragment_source.clone())
        .map_err(source_rejection)?;
    let source = prove_video_content(source, seed.http.cancellation())?;
    let mut demuxer = factory
        .open_video(SmoothVideoDemuxOpenRequest::new(
            Box::new(source),
            seed.http.cancellation().clone(),
            policy.sniff_budget,
        ))
        .map_err(|_| SmoothSiblingRejectionReason::DemuxFailed)?;
    let track = exact_track(demuxer.as_ref(), TrackKind::Video)?;
    if !video_manifest_evidence_matches(row, track) {
        return Err(SmoothSiblingRejectionReason::ManifestEvidenceConflict);
    }
    let track_id = track.id;
    capabilities
        .check_video(track)
        .map_err(|_| SmoothSiblingRejectionReason::CapabilityUnavailable)?;
    prove_first_packet(
        demuxer.as_mut(),
        track_id,
        TrackKind::Video,
        policy.maximum_probe_events,
    )
}

fn prove_audio_row(
    row: &PendingAudioRow,
    seed: &SmoothRuntimeSeed,
    factory: &dyn SmoothIsoBmffDemuxFactory,
    capabilities: &dyn SmoothComponentCapabilityProbe,
    policy: &SmoothCatalogDiscoveryPolicy,
) -> Result<(), SmoothSiblingRejectionReason> {
    let source = build_audio_probe_source(seed, &row.runtime, policy.fragment_source.clone())
        .map_err(source_rejection)?;
    let source = prove_audio_content(source, seed.http.cancellation())?;
    let mut demuxer = factory
        .open_audio(SmoothAudioDemuxOpenRequest::new(
            Box::new(source),
            seed.http.cancellation().clone(),
            policy.sniff_budget,
        ))
        .map_err(|_| SmoothSiblingRejectionReason::DemuxFailed)?;
    let track = exact_track(demuxer.as_ref(), TrackKind::Audio)?;
    if !audio_manifest_evidence_matches(row, track) {
        return Err(SmoothSiblingRejectionReason::ManifestEvidenceConflict);
    }
    let track_id = track.id;
    capabilities
        .check_audio(track)
        .map_err(|_| SmoothSiblingRejectionReason::CapabilityUnavailable)?;
    prove_first_packet(
        demuxer.as_mut(),
        track_id,
        TrackKind::Audio,
        policy.maximum_probe_events,
    )
}

fn source_rejection(error: SmoothFragmentSourceBuildError) -> SmoothSiblingRejectionReason {
    match error {
        SmoothFragmentSourceBuildError::Cancelled => {
            SmoothSiblingRejectionReason::TransportOrContentUnavailable
        }
        _ => SmoothSiblingRejectionReason::TransportOrContentUnavailable,
    }
}

/// Возвращает уже прочитанные proof-сегменты demuxer-у без второго HTTP fetch-а.
struct ReplayingVideoProbeSource {
    source: SmoothVideoFragmentSource,
    replay: VecDeque<OrderedSegment>,
}

impl OrderedSegmentSource for ReplayingVideoProbeSource {
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        if cancellation.is_cancelled() {
            return Err(OrderedSegmentReadError::Cancelled);
        }
        match self.replay.pop_front() {
            Some(segment) => Ok(Some(segment)),
            None => self.source.next_segment(cancellation),
        }
    }
}

/// Audio proof сохраняет presentation-window intent вместе с уже полученными bytes.
struct ReplayingAudioProbeSource {
    source: SmoothAudioFragmentSource,
    replay: VecDeque<PresentationWindowOrderedSegment>,
}

impl PresentationWindowOrderedSegmentSource for ReplayingAudioProbeSource {
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<PresentationWindowOrderedSegmentReadOutcome, OrderedSegmentReadError> {
        if cancellation.is_cancelled() {
            return Err(OrderedSegmentReadError::Cancelled);
        }
        match self.replay.pop_front() {
            Some(segment) => Ok(PresentationWindowOrderedSegmentReadOutcome::Segment(
                segment,
            )),
            None => self.source.next_segment(cancellation),
        }
    }
}

/// Первый pass отличает transport/reconstruction от injected demux failure и затем replay-ится.
fn prove_video_content(
    mut source: SmoothVideoFragmentSource,
    cancellation: &CancellationToken,
) -> Result<ReplayingVideoProbeSource, SmoothSiblingRejectionReason> {
    let initialization = source
        .next_segment(cancellation)
        .map_err(|_| SmoothSiblingRejectionReason::TransportOrContentUnavailable)?;
    let media = source
        .next_segment(cancellation)
        .map_err(|_| SmoothSiblingRejectionReason::TransportOrContentUnavailable)?;
    let (Some(initialization), Some(media)) = (initialization, media) else {
        return Err(SmoothSiblingRejectionReason::TransportOrContentUnavailable);
    };
    if initialization.kind != OrderedSegmentKind::Initialization
        || media.kind != OrderedSegmentKind::Media
    {
        return Err(SmoothSiblingRejectionReason::TransportOrContentUnavailable);
    }
    Ok(ReplayingVideoProbeSource {
        source,
        replay: VecDeque::from([initialization, media]),
    })
}

fn prove_audio_content(
    mut source: SmoothAudioFragmentSource,
    cancellation: &CancellationToken,
) -> Result<ReplayingAudioProbeSource, SmoothSiblingRejectionReason> {
    let initialization = source
        .next_segment(cancellation)
        .map_err(|_| SmoothSiblingRejectionReason::TransportOrContentUnavailable)?;
    let media = source
        .next_segment(cancellation)
        .map_err(|_| SmoothSiblingRejectionReason::TransportOrContentUnavailable)?;
    let PresentationWindowOrderedSegmentReadOutcome::Segment(initialization) = initialization
    else {
        return Err(SmoothSiblingRejectionReason::TransportOrContentUnavailable);
    };
    let PresentationWindowOrderedSegmentReadOutcome::Segment(media) = media else {
        return Err(SmoothSiblingRejectionReason::TransportOrContentUnavailable);
    };
    if !matches!(
        &initialization,
        PresentationWindowOrderedSegment::Initialization { .. }
    ) || !matches!(&media, PresentationWindowOrderedSegment::Media { .. })
    {
        return Err(SmoothSiblingRejectionReason::TransportOrContentUnavailable);
    }
    Ok(ReplayingAudioProbeSource {
        source,
        replay: VecDeque::from([initialization, media]),
    })
}

fn exact_track(
    demuxer: &dyn Demuxer,
    expected_kind: TrackKind,
) -> Result<&TrackInfo, SmoothSiblingRejectionReason> {
    let [track] = demuxer.tracks() else {
        return Err(SmoothSiblingRejectionReason::UnsupportedTrackShape);
    };
    if track.kind != expected_kind {
        return Err(SmoothSiblingRejectionReason::UnsupportedTrackShape);
    }
    Ok(track)
}

fn video_manifest_evidence_matches(row: &PendingVideoRow, track: &TrackInfo) -> bool {
    let Some(video) = track.video.as_ref() else {
        return false;
    };
    track.codec_id == "V_MPEG4/ISO/AVC"
        && row.variant.track().width_pixels() == video.coded_width
        && row.variant.track().height().map(|height| height.pixels()) == video.coded_height
}

fn audio_manifest_evidence_matches(row: &PendingAudioRow, track: &TrackInfo) -> bool {
    track.codec_id == "A_AAC"
        && row.variant.track().sample_rate().map(|rate| rate.hertz()) == track.sample_rate
        && row
            .variant
            .track()
            .channels()
            .map(|channels| u32::from(channels.get()))
            == track.channels
}

fn prove_first_packet(
    demuxer: &mut dyn Demuxer,
    expected_track_id: media_core::TrackId,
    expected_kind: TrackKind,
    maximum_events: NonZeroUsize,
) -> Result<(), SmoothSiblingRejectionReason> {
    for _ in 0..maximum_events.get() {
        match demuxer.next_event() {
            Ok(DemuxReadEvent::Packet(packet))
                if packet.track_id == expected_track_id
                    && packet.kind == expected_kind
                    && !packet.data.is_empty() =>
            {
                return Ok(());
            }
            Ok(DemuxReadEvent::Packet(_)) | Ok(DemuxReadEvent::EndOfStream) => {
                return Err(SmoothSiblingRejectionReason::UnsupportedTrackShape);
            }
            Ok(
                DemuxReadEvent::TemporarilyUnavailable(_)
                | DemuxReadEvent::TracksChanged(_)
                | DemuxReadEvent::MediaMetadataChanged(_),
            ) => {}
            Err(_) => return Err(SmoothSiblingRejectionReason::TransportOrContentUnavailable),
        }
    }
    Err(SmoothSiblingRejectionReason::TransportOrContentUnavailable)
}

#[cfg(test)]
mod tests;
