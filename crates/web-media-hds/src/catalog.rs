//! Provider-owned HDS rendition discovery и neutral coupled catalog mapping.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

use anyhow::{Context, Result, bail};
use demux_api::DemuxRegistry;
use media_core::{Demuxer, TrackInfo, TrackKind};
use source_core::SourceRuntimeConfig;
use web_media_adaptive::AdaptiveHttpContext;
use web_media_core::{
    AudioTrackDescriptor, ChannelCount, ComponentVariantCatalog, ComponentVariantCatalogEntries,
    ComponentVariantCatalogIdentity, ComponentVariantCatalogLimit,
    ComponentVariantCompatibilityEntries, ComponentVariantExactKey, ComponentVariantSelection,
    ComponentVariantSelectionRequest, ComponentVariantSemanticKey, CoupledComponentVariant,
    CoupledVariantExactIdentity, CoupledVariantSemanticIdentity, DynamicRange, NormalizedCodec,
    PreferredHeightPolicy, RawCodecIdentity, SampleRate, VideoHeight, VideoTrackDescriptor,
    VideoWidth,
};
use web_media_transport_api::{MediaPresentation, TransportOpenRequest};

use crate::policy::HdsVodOpenPolicy;
use crate::resolve::{
    HdsRenditionRejection, HdsRenditionRejectionReason, ResolvedHdsRendition, resolve_presentation,
};
use crate::runtime::{
    HdsDemuxPlan, HdsVodOpenResult, open_transactional_demuxer, prepare_probed_hds_vod,
};

/// Synchronous provider discovery request; app может выполнить его на bounded background worker-е.
pub struct HdsCatalogDiscoveryRequest<'capabilities> {
    /// Secret-scoped root manifest request.
    pub transport_request: TransportOpenRequest,
    /// Cloneable source-core network configuration.
    pub source_config: SourceRuntimeConfig,
    /// Existing injected demux registry с F4F factory.
    pub demux_registry: Arc<DemuxRegistry>,
    /// Existing caller-owned HDS bounds.
    pub policy: HdsVodOpenPolicy,
    /// Parent exact identity + caller-owned catalog generation fence.
    pub catalog_identity: ComponentVariantCatalogIdentity,
    /// Injected immutable capability intersection over exact probed tracks.
    pub capability_probe: &'capabilities dyn HdsRenditionCapabilityProbe,
    /// Provider-default ranking policy.
    pub preferred_height: PreferredHeightPolicy,
}

/// Safe capability rejection: diagnostics не получают backend или track payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdsRenditionCapabilityRejection;

/// Typed итог полного discovery pass-а, в котором ни одна rendition не прошла
/// provider content/capability proof.
///
/// Marker намеренно не содержит locator, codec payload или backend details:
/// app может безопасно отличить retryable parent-content rejection от
/// network/parser/cancellation ошибок, не раскрывая secret-scoped request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdsNoPlayableRendition;

impl fmt::Display for HdsNoPlayableRendition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HDS catalog contains no probed playable rendition")
    }
}

impl std::error::Error for HdsNoPlayableRendition {}

/// Existing-composition adapter для immutable video/audio capability snapshots.
///
/// Provider передаёт только уже boundedly probed exact tracks; реализация не
/// должна создавать decoder или менять runtime state.
pub trait HdsRenditionCapabilityProbe: Send + Sync {
    /// Подтверждает, что coupled HDS video+audio shape playable целиком.
    fn check_coupled_av(
        &self,
        video: &TrackInfo,
        audio: &TrackInfo,
    ) -> std::result::Result<(), HdsRenditionCapabilityRejection>;
}

/// Discovered catalog с neutral public rows и private exact runtime mapping.
pub struct HdsRenditionCatalog {
    catalog: ComponentVariantCatalog,
    provider_default: ComponentVariantSelection,
    rejections: Box<[HdsRenditionRejection]>,
    rows: Box<[DiscoveredHdsRendition]>,
}

impl fmt::Debug for HdsRenditionCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HdsRenditionCatalog")
            .field("catalog_identity", self.catalog.identity())
            .field("published_rows", &self.rows.len())
            .field("rejected_rows", &self.rejections.len())
            .finish_non_exhaustive()
    }
}

impl HdsRenditionCatalog {
    /// Возвращает provider-neutral coupled catalog без runtime locator state.
    #[must_use]
    pub const fn catalog(&self) -> &ComponentVariantCatalog {
        &self.catalog
    }

    /// Возвращает exact provider default уже внутри текущего catalog generation.
    #[must_use]
    pub const fn provider_default(&self) -> &ComponentVariantSelection {
        &self.provider_default
    }

    /// Возвращает bounded safe diagnostics для скрытых siblings.
    #[must_use]
    pub fn rejections(&self) -> &[HdsRenditionRejection] {
        &self.rejections
    }
}

/// Private exact mapping одной доказанной coupled row.
struct DiscoveredHdsRendition {
    exact_identity: CoupledVariantExactIdentity,
    plan: Arc<HdsDemuxPlan>,
    demuxer: Box<dyn Demuxer + Send>,
}

/// Пробует каждый advertised rendition и публикует catalog только после complete pass-а.
pub fn discover_hds_renditions(
    request: HdsCatalogDiscoveryRequest<'_>,
) -> Result<HdsRenditionCatalog> {
    if request.transport_request.presentation() != MediaPresentation::Vod {
        bail!("HDS catalog discovery accepts only VOD transport presentation");
    }
    let root_target = request
        .transport_request
        .target()
        .as_http()
        .cloned()
        .context("HDS catalog root target is not HTTP")?;
    let http = AdaptiveHttpContext::new(
        request.transport_request,
        &request.source_config,
        request.policy.adaptive_limits,
        request.policy.adaptive_retry,
    )
    .context("HDS catalog HTTP context creation failed")?;
    let resolved = resolve_presentation(root_target, &http, request.policy)?;
    let mut first_unavailable = resolved.first_unavailable;
    let mut rejections = resolved.rejections;
    let mut admitted = Vec::new();

    let probe_results = probe_renditions_bounded(
        resolved.renditions,
        &http,
        &request.demux_registry,
        request.policy,
        request.capability_probe,
    )?;
    for (rendition, probe_result) in probe_results {
        if http.cancellation().is_cancelled() {
            bail!("HDS catalog discovery was cancelled");
        }
        match probe_result {
            Ok(probe) => {
                let semantic_key = coupled_semantic_key(&rendition, &probe);
                admitted.push(PendingDiscoveredHdsRendition {
                    semantic_key,
                    rendition,
                    video: probe.video,
                    audio: probe.audio,
                    plan: probe.plan,
                    demuxer: probe.demuxer,
                });
            }
            Err(HdsRenditionProbeFailure::Rejected(reason)) => {
                if http.cancellation().is_cancelled() {
                    bail!("HDS catalog discovery was cancelled");
                }
                rejections.push(HdsRenditionRejection::new(reason));
            }
            Err(HdsRenditionProbeFailure::Unavailable(error)) => {
                if http.cancellation().is_cancelled() {
                    return Err(error.context("HDS catalog discovery was cancelled"));
                }
                first_unavailable.get_or_insert(error);
            }
        }
    }

    let mut semantic_counts = HashMap::new();
    for row in &admitted {
        *semantic_counts
            .entry(row.semantic_key.clone())
            .or_insert(0_usize) += 1;
    }
    admitted.retain(|row| {
        if semantic_counts.get(&row.semantic_key).copied() == Some(1) {
            true
        } else {
            rejections.push(HdsRenditionRejection::new(
                HdsRenditionRejectionReason::AmbiguousSemanticIdentity,
            ));
            false
        }
    });
    if admitted.is_empty() {
        if let Some(error) = first_unavailable {
            return Err(error.context(
                "HDS catalog has no admitted rendition because provider infrastructure is unavailable",
            ));
        }
        return Err(HdsNoPlayableRendition.into());
    }

    admitted.sort_by(|left, right| compare_admitted(left, right, request.preferred_height));
    let mut coupled = Vec::with_capacity(admitted.len());
    let mut runtime_rows = Vec::with_capacity(admitted.len());
    for row in admitted {
        let exact_key = ComponentVariantExactKey::new(row.semantic_key.clone())
            .context("HDS exact coupled key is invalid")?;
        let semantic_key = ComponentVariantSemanticKey::new(row.semantic_key.clone())
            .context("HDS semantic coupled key is invalid")?;
        let exact_identity =
            CoupledVariantExactIdentity::new(request.catalog_identity.clone(), exact_key);
        let semantic_identity = CoupledVariantSemanticIdentity::new(
            request.catalog_identity.parent().semantic().clone(),
            semantic_key,
        );
        coupled.push(CoupledComponentVariant::new(
            exact_identity.clone(),
            semantic_identity,
            row.video,
            row.audio,
        ));
        runtime_rows.push(DiscoveredHdsRendition {
            exact_identity,
            plan: row.plan,
            demuxer: row.demuxer,
        });
    }

    let catalog_limit = ComponentVariantCatalogLimit::new(request.policy.maximum_renditions)
        .context("HDS catalog limit is outside neutral bounds")?;
    let catalog = ComponentVariantCatalog::new(
        request.catalog_identity,
        catalog_limit,
        ComponentVariantCatalogEntries::Topology {
            video: Vec::new(),
            audio: Vec::new(),
            compatibility: ComponentVariantCompatibilityEntries::Unavailable,
            coupled,
            video_only: Vec::new(),
            audio_only: Vec::new(),
        },
    )
    .context("HDS coupled catalog validation failed")?;
    let default_exact = runtime_rows
        .first()
        .context("HDS provider default disappeared after catalog validation")?
        .exact_identity
        .clone();
    let provider_default = catalog
        .select_exact(ComponentVariantSelectionRequest::Coupled {
            presentation: default_exact,
        })
        .context("HDS provider default selection failed")?;

    Ok(HdsRenditionCatalog {
        catalog,
        provider_default,
        rejections: rejections.into_boxed_slice(),
        rows: runtime_rows.into_boxed_slice(),
    })
}

/// Открывает ровно одну exact row из fresh discovered catalog.
pub fn prepare_discovered_hds_vod(
    discovered: HdsRenditionCatalog,
    exact_identity: CoupledVariantExactIdentity,
) -> Result<HdsVodOpenResult> {
    discovered
        .catalog
        .select_exact(ComponentVariantSelectionRequest::Coupled {
            presentation: exact_identity.clone(),
        })
        .context("HDS exact discovered selection is invalid")?;
    let selected_index = discovered
        .rows
        .iter()
        .position(|row| row.exact_identity == exact_identity)
        .context("HDS exact discovered row has no private runtime mapping")?;

    let HdsRenditionCatalog { catalog, rows, .. } = discovered;
    let mut rows = rows.into_vec();
    let selected = rows.swap_remove(selected_index);
    prepare_probed_hds_vod(selected.plan, selected.demuxer, catalog)
}

struct PendingDiscoveredHdsRendition {
    semantic_key: String,
    rendition: ResolvedHdsRendition,
    video: VideoTrackDescriptor,
    audio: AudioTrackDescriptor,
    plan: Arc<HdsDemuxPlan>,
    demuxer: Box<dyn Demuxer + Send>,
}

struct HdsRenditionProbe {
    video: VideoTrackDescriptor,
    audio: AudioTrackDescriptor,
    track_semantic_evidence: String,
    plan: Arc<HdsDemuxPlan>,
    demuxer: Box<dyn Demuxer + Send>,
}

/// Один bounded probe либо доказал content rejection, либо не смог вынести
/// вердикт из-за infrastructure failure.
enum HdsRenditionProbeFailure {
    /// Container/track/profile/capability несовместимы и разрешают fallback.
    Rejected(HdsRenditionRejectionReason),
    /// Transport/demux open не доказали несовместимость содержимого.
    Unavailable(anyhow::Error),
}

type HdsRenditionProbeResult = std::result::Result<HdsRenditionProbe, HdsRenditionProbeFailure>;

/// Выполняет complete pass с caller-owned concurrency bound и возвращает
/// результаты в manifest order, независимо от фактического порядка завершения.
fn probe_renditions_bounded(
    renditions: Vec<ResolvedHdsRendition>,
    http: &AdaptiveHttpContext,
    registry: &Arc<DemuxRegistry>,
    policy: HdsVodOpenPolicy,
    capability_probe: &dyn HdsRenditionCapabilityProbe,
) -> Result<Vec<(ResolvedHdsRendition, HdsRenditionProbeResult)>> {
    if renditions.len() <= 1 || policy.maximum_parallel_rendition_probes.get() == 1 {
        return Ok(renditions
            .into_iter()
            .map(|rendition| {
                let probe_result =
                    probe_rendition(&rendition, http, registry, policy, capability_probe);
                (rendition, probe_result)
            })
            .collect());
    }

    let worker_count = policy
        .maximum_parallel_rendition_probes
        .get()
        .min(renditions.len());
    let next_rendition_index = AtomicUsize::new(0);
    let (result_sender, result_receiver) = mpsc::sync_channel(renditions.len());
    thread::scope(|scope| -> Result<()> {
        let mut workers = Vec::with_capacity(worker_count);
        let mut first_worker_error = None;
        for worker_index in 0..worker_count {
            let worker_sender = result_sender.clone();
            let worker_renditions = &renditions;
            let worker_next_index = &next_rendition_index;
            let worker_name = format!("hds-rendition-probe-{worker_index}");
            match thread::Builder::new().name(worker_name).spawn_scoped(
                scope,
                move || -> Result<()> {
                    loop {
                        let rendition_index = worker_next_index.fetch_add(1, Ordering::Relaxed);
                        let Some(rendition) = worker_renditions.get(rendition_index) else {
                            break;
                        };
                        let probe_result = if http.cancellation().is_cancelled() {
                            Err(HdsRenditionProbeFailure::Unavailable(anyhow::anyhow!(
                                "HDS catalog discovery was cancelled"
                            )))
                        } else {
                            probe_rendition(rendition, http, registry, policy, capability_probe)
                        };
                        worker_sender
                            .send((rendition_index, probe_result))
                            .map_err(|_| {
                                anyhow::anyhow!("HDS rendition probe result receiver closed")
                            })?;
                    }
                    Ok(())
                },
            ) {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    first_worker_error = Some(
                        anyhow::Error::new(error)
                            .context("failed to spawn bounded HDS rendition probe worker"),
                    );
                    break;
                }
            }
        }

        for worker in workers {
            let worker_result = match worker.join() {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("HDS rendition probe worker panicked")),
            };
            if first_worker_error.is_none() {
                first_worker_error = worker_result.err();
            }
        }
        if let Some(error) = first_worker_error {
            return Err(error);
        }
        Ok(())
    })?;
    drop(result_sender);

    let mut indexed_results = Vec::with_capacity(renditions.len());
    for _ in 0..renditions.len() {
        indexed_results.push(
            result_receiver
                .recv()
                .context("bounded HDS rendition probe result is missing")?,
        );
    }
    indexed_results.sort_unstable_by_key(|(rendition_index, _)| *rendition_index);
    for (expected_index, (actual_index, _)) in indexed_results.iter().enumerate() {
        if *actual_index != expected_index {
            bail!("bounded HDS rendition probe result order is incomplete");
        }
    }

    Ok(renditions
        .into_iter()
        .zip(
            indexed_results
                .into_iter()
                .map(|(_rendition_index, probe_result)| probe_result),
        )
        .collect())
}

fn probe_rendition(
    rendition: &ResolvedHdsRendition,
    http: &AdaptiveHttpContext,
    registry: &Arc<DemuxRegistry>,
    policy: HdsVodOpenPolicy,
    capability_probe: &dyn HdsRenditionCapabilityProbe,
) -> std::result::Result<HdsRenditionProbe, HdsRenditionProbeFailure> {
    let plan = Arc::new(
        HdsDemuxPlan::new(
            rendition.clone(),
            http.clone(),
            Arc::clone(registry),
            policy,
        )
        .map_err(HdsRenditionProbeFailure::Unavailable)?,
    );
    let demuxer = open_transactional_demuxer(Arc::clone(&plan), 0)
        .map_err(HdsRenditionProbeFailure::Unavailable)?;
    let (video_track, audio_track) =
        exact_av_tracks(demuxer.as_ref()).map_err(HdsRenditionProbeFailure::Rejected)?;
    validate_profile_codecs(video_track, audio_track)
        .map_err(HdsRenditionProbeFailure::Rejected)?;
    capability_probe
        .check_coupled_av(video_track, audio_track)
        .map_err(|_| {
            HdsRenditionProbeFailure::Rejected(HdsRenditionRejectionReason::CapabilityUnavailable)
        })?;
    let video =
        video_evidence(rendition, video_track).map_err(HdsRenditionProbeFailure::Rejected)?;
    let audio = audio_evidence(audio_track).map_err(HdsRenditionProbeFailure::Rejected)?;
    let track_semantic_evidence = track_semantic_evidence(video_track, audio_track);
    Ok(HdsRenditionProbe {
        video,
        audio,
        track_semantic_evidence,
        plan,
        demuxer,
    })
}

fn exact_av_tracks(
    demuxer: &dyn Demuxer,
) -> Result<(&TrackInfo, &TrackInfo), HdsRenditionRejectionReason> {
    if demuxer.tracks().len() != 2 {
        return Err(HdsRenditionRejectionReason::UnsupportedTrackShape);
    }
    let video = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .ok_or(HdsRenditionRejectionReason::UnsupportedTrackShape)?;
    let audio = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .ok_or(HdsRenditionRejectionReason::UnsupportedTrackShape)?;
    Ok((video, audio))
}

fn validate_profile_codecs(
    video: &TrackInfo,
    audio: &TrackInfo,
) -> Result<(), HdsRenditionRejectionReason> {
    if video.codec_id != "V_MPEG4/ISO/AVC" || audio.codec_id != "A_AAC" {
        return Err(HdsRenditionRejectionReason::UnsupportedCodec);
    }
    Ok(())
}

fn video_evidence(
    rendition: &ResolvedHdsRendition,
    track: &TrackInfo,
) -> Result<VideoTrackDescriptor, HdsRenditionRejectionReason> {
    let probed_width = track.video.as_ref().and_then(|video| video.coded_width);
    let probed_height = track.video.as_ref().and_then(|video| video.coded_height);
    if rendition
        .summary
        .width
        .zip(probed_width)
        .is_some_and(|(advertised, probed)| advertised != probed)
        || rendition
            .summary
            .height
            .zip(probed_height)
            .is_some_and(|(advertised, probed)| advertised != probed)
    {
        return Err(HdsRenditionRejectionReason::UnsupportedTrackShape);
    }

    let width = probed_width
        .or(rendition.summary.width)
        .map(VideoWidth::new)
        .transpose()
        .map_err(|_| HdsRenditionRejectionReason::UnsupportedCodec)?;
    let height = probed_height
        .or(rendition.summary.height)
        .map(VideoHeight::new)
        .transpose()
        .map_err(|_| HdsRenditionRejectionReason::UnsupportedCodec)?;
    Ok(VideoTrackDescriptor::new(
        normalized_codec("h264")?,
        width,
        height,
        None,
        None,
        DynamicRange::Unknown,
    ))
}

fn audio_evidence(track: &TrackInfo) -> Result<AudioTrackDescriptor, HdsRenditionRejectionReason> {
    let sample_rate = track
        .sample_rate
        .map(SampleRate::new)
        .transpose()
        .map_err(|_| HdsRenditionRejectionReason::UnsupportedCodec)?;
    let channels = track
        .channels
        .map(|channels| {
            u16::try_from(channels)
                .map_err(|_| HdsRenditionRejectionReason::UnsupportedCodec)
                .and_then(|channels| {
                    ChannelCount::new(channels)
                        .map_err(|_| HdsRenditionRejectionReason::UnsupportedCodec)
                })
        })
        .transpose()?;
    Ok(AudioTrackDescriptor::new(
        normalized_codec("aac")?,
        sample_rate,
        channels,
        None,
        None,
    ))
}

fn normalized_codec(value: &'static str) -> Result<NormalizedCodec, HdsRenditionRejectionReason> {
    RawCodecIdentity::new(value)
        .map(NormalizedCodec::parse)
        .map_err(|_| HdsRenditionRejectionReason::UnsupportedCodec)
}

fn coupled_semantic_key(rendition: &ResolvedHdsRendition, probe: &HdsRenditionProbe) -> String {
    format!(
        "hds-v1-c|{}|{}|sr{}|ch{}",
        rendition.id.as_key(),
        probe.track_semantic_evidence,
        probe.audio.sample_rate().map_or(0, SampleRate::hertz),
        probe.audio.channels().map_or(0, ChannelCount::get),
    )
}

fn track_semantic_evidence(video: &TrackInfo, _audio: &TrackInfo) -> String {
    let metadata = video.video.as_ref();
    format!(
        "vp{:?}|bd{:?}|cs{:?}",
        metadata.and_then(|video| video.profile),
        metadata.and_then(|video| video.bit_depth),
        metadata.and_then(|video| video.chroma),
    )
}

fn compare_admitted(
    left: &PendingDiscoveredHdsRendition,
    right: &PendingDiscoveredHdsRendition,
    preference: PreferredHeightPolicy,
) -> std::cmp::Ordering {
    preference
        .compare(
            left.rendition.summary.height.and_then(valid_video_height),
            right.rendition.summary.height.and_then(valid_video_height),
        )
        .then_with(|| {
            right
                .rendition
                .summary
                .bitrate
                .cmp(&left.rendition.summary.bitrate)
        })
        .then_with(|| {
            right
                .rendition
                .summary
                .height
                .cmp(&left.rendition.summary.height)
        })
        .then_with(|| {
            right
                .rendition
                .summary
                .width
                .cmp(&left.rendition.summary.width)
        })
        .then_with(|| left.semantic_key.cmp(&right.semantic_key))
}

fn valid_video_height(height: u32) -> Option<VideoHeight> {
    VideoHeight::new(height).ok()
}

#[cfg(test)]
mod tests {
    use super::normalized_codec;
    use web_media_core::CodecKind;

    #[test]
    fn hds_profile_codecs_map_to_known_neutral_families() {
        assert!(matches!(
            normalized_codec("h264").unwrap().kind(),
            CodecKind::Known(_)
        ));
        assert!(matches!(
            normalized_codec("aac").unwrap().kind(),
            CodecKind::Known(_)
        ));
    }
}
