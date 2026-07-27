//! Production composition selected yt-dlp HLS candidate -> uninstalled HLS VOD runtime.

use std::num::{NonZeroU8, NonZeroU32, NonZeroUsize};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use demux_api::{
    CompositeComponentLeadPolicy, DemuxInputCapability, DemuxRegistry, DemuxSniffBudget,
    ProgressiveAsyncSeekEnqueueError, ProgressiveAsyncSeekHandle, ProgressiveAsyncSeekLimits,
    ProgressiveAsyncSeekOutcome, ProgressiveDemuxBufferLimits, ProgressiveSeekFence,
    ProgressiveSeekRequestId,
};
use hls_playlist_core::HlsParserLimits;
use media_core::{
    DemuxReadEvent, DemuxRetryHint, Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelinePort,
    DynamicMediaTimelinePortGeneration, TrackKind,
};
use player_core::{
    PreparedDemuxSeekEnqueueError, PreparedDemuxSeekOutcome, PreparedDemuxSeekPort,
    PreparedDemuxSeekReceipt, PreparedDemuxSeekRequestId,
};
use rustiplayer_config::NetworkConfig;
use service_ytdlp::{
    YtDlpHlsManifestInputKind, YtDlpNormalizedCandidate, YtDlpTransportRequestContext,
};
use source_core::{CancellationToken, SourceRuntimeConfig};
use web_media_adaptive::{AdaptiveHttpContext, AdaptiveRetryPolicy, AdaptiveTransportLimits};
use web_media_core::{ContainerFamily, StreamLayout, TransportFamily};
use web_media_hls::{
    ExtractorAesOverride, HlsAudioLayoutIntent, HlsAudioRenditionEvidence, HlsCatalogBuildPolicy,
    HlsCatalogCapabilityProofPort, HlsCatalogDiscoveryOutcome, HlsCatalogDiscoveryRequest,
    HlsCatalogPresentation, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsEndpointRefreshPort, HlsLiveOpenRequest, HlsMainTrackLayoutIntent, HlsManifestInput,
    HlsRequestOverrides, HlsRequiredContainer, HlsVariantSelectionIntent, HlsVodOpenPolicy,
    HlsVodOpenRequest, SecretInlineMediaPlaylist, discover_hls_catalog,
    prepare_hls_catalog_live_receipted, prepare_hls_catalog_vod_receipted,
    prepare_hls_live_receipted, prepare_hls_vod_receipted,
};
use web_media_transport_api::{SourceGeneration, TransportProviderId};

/// Secret-safe результат HLS preparation для общего coordinator-а.
pub(crate) struct PreparedHlsCandidate {
    pub(crate) demuxer: Box<dyn Demuxer + Send>,
    pub(crate) seek_port: Arc<dyn PreparedDemuxSeekPort>,
    pub(crate) subtitles: Arc<[crate::web_media_hls_subtitles::InstalledHlsSubtitleRendition]>,
    pub(crate) timeline_port: Option<DynamicMediaTimelinePort>,
    pub(crate) component_variants:
        crate::web_media_open::component_variants::PreparedComponentVariantCatalog,
}

struct HlsPreparedDemuxSeekPort {
    handle: ProgressiveAsyncSeekHandle,
}

impl PreparedDemuxSeekPort for HlsPreparedDemuxSeekPort {
    fn enqueue_seek(
        &self,
        request_id: PreparedDemuxSeekRequestId,
        request: media_core::DemuxSeekRequest,
    ) -> Result<(), PreparedDemuxSeekEnqueueError> {
        self.handle
            .enqueue(
                ProgressiveSeekFence {
                    runtime_generation: self.handle.runtime_generation(),
                    request_id: ProgressiveSeekRequestId::new(request_id.value()),
                },
                request,
            )
            .map_err(map_enqueue_error)
    }

    fn poll_seek_receipt(&self) -> Option<PreparedDemuxSeekReceipt> {
        self.handle
            .poll_receipt()
            .map(|receipt| PreparedDemuxSeekReceipt {
                request_id: PreparedDemuxSeekRequestId::new(receipt.fence.request_id.value()),
                outcome: match receipt.outcome {
                    ProgressiveAsyncSeekOutcome::Succeeded(result) => {
                        PreparedDemuxSeekOutcome::Succeeded(result)
                    }
                    ProgressiveAsyncSeekOutcome::Failed => PreparedDemuxSeekOutcome::Failed,
                    ProgressiveAsyncSeekOutcome::Cancelled => PreparedDemuxSeekOutcome::Cancelled,
                    ProgressiveAsyncSeekOutcome::Superseded => PreparedDemuxSeekOutcome::Superseded,
                    ProgressiveAsyncSeekOutcome::Stale => PreparedDemuxSeekOutcome::Stale,
                },
            })
    }
}

pub(crate) struct HlsProjectedRuntimeMaterial {
    pub(crate) http: AdaptiveHttpContext,
    pub(crate) manifest: HlsManifestInput,
    pub(crate) overrides: HlsRequestOverrides,
}

/// Typed граница между single-master alternate audio и extractor compound resources.
#[derive(Debug, thiserror::Error)]
enum HlsCandidateTopologyError {
    /// Два независимых manifest URL уже являются compound topology, а не AUDIO rendition master-а.
    #[error(
        "compound HLS candidate содержит два независимых manifest resource; alternate AUDIO поддерживается только внутри одного master playlist"
    )]
    IndependentManifestResources,
}

/// Проверяет transport family без открытия provider-а.
pub(crate) fn candidate_is_hls(candidate: &YtDlpNormalizedCandidate) -> bool {
    match candidate.descriptor().layout() {
        StreamLayout::Muxed(component) => component.transport().family() == TransportFamily::Hls,
        StreamLayout::HlsMuxedCodecDeferred(component) => {
            component.transport().family() == TransportFamily::Hls
        }
        StreamLayout::VideoOnly(component) => {
            component.transport().family() == TransportFamily::Hls
        }
        StreamLayout::AudioOnly(component) => {
            component.transport().family() == TransportFamily::Hls
        }
        StreamLayout::Separate { video, audio } => {
            video.transport().family() == TransportFamily::Hls
                && audio.transport().family() == TransportFamily::Hls
        }
    }
}

/// Выполняет manifest/profile/container preflight на existing media-open worker-е.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_hls_candidate(
    candidate: &YtDlpNormalizedCandidate,
    provider_id: TransportProviderId,
    source_config: &SourceRuntimeConfig,
    network_config: &NetworkConfig,
    demux_registry: Arc<DemuxRegistry>,
    cancellation: CancellationToken,
    live_intent: service_ytdlp::YtDlpLiveIntent,
    endpoint_refresh: Option<Arc<dyn HlsEndpointRefreshPort>>,
    timeline_port_generation: DynamicMediaTimelinePortGeneration,
    component_selection_intent:
        crate::web_media_open::component_variants::YtDlpComponentSelectionOpenIntent,
    catalog_identity: web_media_core::ComponentVariantCatalogIdentity,
    capability_probe: &mut crate::web_media_open::catalog_capabilities::AppCatalogCapabilityProbe,
) -> Result<PreparedHlsCandidate> {
    let generation = crate::web_media_adaptive_config::initial_adaptive_source_generation();
    let projected = project_hls_runtime_material(
        candidate,
        provider_id,
        generation,
        source_config,
        network_config,
        cancellation,
    )?;
    let HlsProjectedRuntimeMaterial {
        http,
        manifest,
        overrides,
    } = projected;
    let (selection, containers) = selection_and_containers(candidate.descriptor().layout())?;
    let policy = hls_policy(crate::web_media_adaptive_config::adaptive_transport_limits(
        network_config,
    )?)?;
    if live_intent == service_ytdlp::YtDlpLiveIntent::Live {
        let endpoint_refresh = endpoint_refresh
            .ok_or_else(|| anyhow!("HLS live candidate потерял app endpoint refresh port"))?;
        let request = HlsLiveOpenRequest {
            common: HlsVodOpenRequest {
                http,
                generation,
                manifest,
                selection,
                overrides,
                containers,
                demux_registry,
                policy,
            },
            endpoint_refresh,
            timeline_port_generation,
            initial_source_epoch: DynamicMediaTimelineEpoch::new(0),
        };
        let (opened, component_variants) = match component_selection_intent {
            crate::web_media_open::component_variants::YtDlpComponentSelectionOpenIntent::ProviderDefault => (
                prepare_hls_live_receipted(request, hls_async_seek_limits())
                    .context("HLS live preflight завершился ошибкой")?,
                crate::web_media_open::component_variants::PreparedComponentVariantCatalog::Unavailable,
            ),
            crate::web_media_open::component_variants::YtDlpComponentSelectionOpenIntent::Semantic(semantic) => {
                let discovered = discover_hls_catalog(
                    HlsCatalogDiscoveryRequest {
                        open: &request.common,
                        catalog_identity,
                        presentation: HlsCatalogPresentation::Live,
                        policy: hls_catalog_policy()?,
                    },
                    capability_probe,
                )?;
                let HlsCatalogDiscoveryOutcome::Installed(snapshot) = discovered else {
                    anyhow::bail!("HLS live semantic reopen requires a master catalog");
                };
                let selected = snapshot.catalog().rematch_semantic(semantic)?;
                let reopen = snapshot.reopen_exact(&selected)?;
                let catalog = Arc::new(snapshot.catalog().clone());
                (
                    prepare_hls_catalog_live_receipted(request, reopen, hls_async_seek_limits())
                        .context("HLS live catalog reopen завершился ошибкой")?,
                    crate::web_media_open::component_variants::PreparedComponentVariantCatalog::Installed {
                        catalog,
                        provider_selection: selected,
                    },
                )
            }
        };
        let subtitles = opened
            .subtitle_renditions()
            .iter()
            .map(crate::web_media_hls_subtitles::InstalledHlsSubtitleRendition::from_prepared)
            .collect::<Vec<_>>()
            .into();
        let seek_handle = opened
            .async_seek_handle()
            .ok_or_else(|| anyhow!("HLS live runtime потерял receipted seek handle"))?;
        let (mut demuxer, timeline_port, _) = opened.into_parts();
        prove_deferred_hls_codec_evidence(candidate, demuxer.as_mut(), capability_probe)
            .context("HLS deferred candidate не прошёл post-open codec proof")?;
        return Ok(PreparedHlsCandidate {
            demuxer,
            seek_port: Arc::new(HlsPreparedDemuxSeekPort {
                handle: seek_handle,
            }),
            subtitles,
            timeline_port: Some(timeline_port),
            component_variants,
        });
    }
    if !matches!(
        live_intent,
        service_ytdlp::YtDlpLiveIntent::Unspecified | service_ytdlp::YtDlpLiveIntent::NotLive
    ) {
        return Err(anyhow!(
            "yt-dlp live intent несовместим с HLS playback profile"
        ));
    }
    let request = HlsVodOpenRequest {
        http,
        generation,
        manifest,
        selection,
        overrides,
        containers,
        demux_registry,
        policy,
    };
    let (opened, component_variants) = match component_selection_intent {
        crate::web_media_open::component_variants::YtDlpComponentSelectionOpenIntent::ProviderDefault => (
            prepare_hls_vod_receipted(request, hls_async_seek_limits())
                .context("HLS VOD preflight завершился ошибкой")?,
            crate::web_media_open::component_variants::PreparedComponentVariantCatalog::Unavailable,
        ),
        crate::web_media_open::component_variants::YtDlpComponentSelectionOpenIntent::Semantic(semantic) => {
            let discovered = discover_hls_catalog(
                HlsCatalogDiscoveryRequest {
                    open: &request,
                    catalog_identity,
                    presentation: HlsCatalogPresentation::Vod,
                    policy: hls_catalog_policy()?,
                },
                capability_probe,
            )?;
            let HlsCatalogDiscoveryOutcome::Installed(snapshot) = discovered else {
                anyhow::bail!("HLS VOD semantic reopen requires a master catalog");
            };
            let selected = snapshot.catalog().rematch_semantic(semantic)?;
            let reopen = snapshot.reopen_exact(&selected)?;
            let catalog = Arc::new(snapshot.catalog().clone());
            (
                prepare_hls_catalog_vod_receipted(request, reopen, hls_async_seek_limits())
                    .context("HLS VOD catalog reopen завершился ошибкой")?,
                crate::web_media_open::component_variants::PreparedComponentVariantCatalog::Installed {
                    catalog,
                    provider_selection: selected,
                },
            )
        }
    };
    let subtitles = opened
        .subtitle_renditions()
        .iter()
        .map(crate::web_media_hls_subtitles::InstalledHlsSubtitleRendition::from_prepared)
        .collect::<Vec<_>>()
        .into();
    let seek_handle = opened
        .async_seek_handle()
        .ok_or_else(|| anyhow!("HLS VOD runtime потерял receipted seek handle"))?;
    let mut demuxer = opened.into_demuxer();
    prove_deferred_hls_codec_evidence(candidate, demuxer.as_mut(), capability_probe)
        .context("HLS deferred candidate не прошёл post-open codec proof")?;
    Ok(PreparedHlsCandidate {
        demuxer,
        seek_port: Arc::new(HlsPreparedDemuxSeekPort {
            handle: seek_handle,
        }),
        subtitles,
        timeline_port: None,
        component_variants,
    })
}

fn hls_async_seek_limits() -> ProgressiveAsyncSeekLimits {
    ProgressiveAsyncSeekLimits::new(NonZeroUsize::new(16).expect("HLS outstanding seek receipts"))
}

const fn map_enqueue_error(
    error: ProgressiveAsyncSeekEnqueueError,
) -> PreparedDemuxSeekEnqueueError {
    match error {
        ProgressiveAsyncSeekEnqueueError::ReceiptQueueFull => {
            PreparedDemuxSeekEnqueueError::ReceiptQueueFull
        }
        ProgressiveAsyncSeekEnqueueError::NonMonotonicRequestIdentity => {
            PreparedDemuxSeekEnqueueError::NonMonotonicRequestIdentity
        }
        ProgressiveAsyncSeekEnqueueError::WorkerStopped => {
            PreparedDemuxSeekEnqueueError::WorkerStopped
        }
        ProgressiveAsyncSeekEnqueueError::CapabilityAbsent => {
            PreparedDemuxSeekEnqueueError::CapabilityUnavailable
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn project_hls_runtime_material(
    candidate: &YtDlpNormalizedCandidate,
    provider_id: TransportProviderId,
    generation: SourceGeneration,
    source_config: &SourceRuntimeConfig,
    network_config: &NetworkConfig,
    cancellation: CancellationToken,
) -> Result<HlsProjectedRuntimeMaterial> {
    let context = YtDlpTransportRequestContext::new(provider_id, generation, cancellation);
    let transport_request = candidate
        .hls_transport_request(&context)
        .context("Не удалось спроецировать yt-dlp HLS transport material")?;
    let material = candidate
        .hls_request_material()
        .context("Не удалось получить validated yt-dlp HLS material")?;
    let selected_target = transport_request
        .target()
        .as_http()
        .context("HLS transport request должен содержать HTTP target")?
        .clone();
    let manifest = match material.manifest().kind() {
        YtDlpHlsManifestInputKind::FetchSelectedUrl => HlsManifestInput::Fetch {
            selected_url: selected_target,
        },
        YtDlpHlsManifestInputKind::Inline => HlsManifestInput::InlineMedia {
            selected_url: selected_target,
            playlist: SecretInlineMediaPlaylist::new(
                material
                    .manifest()
                    .inline_playlist_for_parse()
                    .ok_or_else(|| anyhow!("inline HLS type-state потерял playlist bytes"))?,
            ),
        },
    };
    let aes = material
        .aes_override()
        .map(|override_material| {
            ExtractorAesOverride::new(
                override_material.replacement_key_uri_for_fetch(),
                override_material.key_hex_for_crypto(),
                override_material.iv_hex_for_crypto(),
            )
        })
        .transpose()
        .context("yt-dlp hls_aes нарушил validated AES boundary")?;
    let adaptive_limits =
        crate::web_media_adaptive_config::adaptive_transport_limits(network_config)?;
    let http = AdaptiveHttpContext::new(
        transport_request,
        source_config,
        adaptive_limits,
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(3).expect("non-zero retry attempts"),
            Duration::from_millis(100),
            Duration::from_secs(2),
        )
        .context("HLS retry policy invalid")?,
    )
    .context("Не удалось создать HLS adaptive HTTP context")?;
    Ok(HlsProjectedRuntimeMaterial {
        http,
        manifest,
        overrides: HlsRequestOverrides::new(aes),
    })
}

fn selection_and_containers(
    layout: &StreamLayout,
) -> Result<(HlsVariantSelectionIntent, HlsComponentContainerIntent)> {
    let (video, audio_track, container, audio, main_track_layout) = match layout {
        StreamLayout::Muxed(component) => (
            Some(component.video()),
            Some(component.audio()),
            component.container(),
            HlsAudioLayoutIntent::ManifestResolved(audio_rendition_evidence(component.audio())),
            HlsMainTrackLayoutIntent::MuxedAv,
        ),
        StreamLayout::HlsMuxedCodecDeferred(component) => (
            None,
            None,
            component.container(),
            HlsAudioLayoutIntent::Muxed,
            HlsMainTrackLayoutIntent::MuxedAv,
        ),
        StreamLayout::VideoOnly(component) => (
            Some(component.video()),
            None,
            component.container(),
            HlsAudioLayoutIntent::Muxed,
            HlsMainTrackLayoutIntent::VideoOnly,
        ),
        StreamLayout::AudioOnly(component) => (
            None,
            Some(component.audio()),
            component.container(),
            HlsAudioLayoutIntent::Muxed,
            HlsMainTrackLayoutIntent::AudioOnly,
        ),
        StreamLayout::Separate { .. } => {
            return Err(HlsCandidateTopologyError::IndependentManifestResources.into());
        }
    };
    let resolution = match layout {
        StreamLayout::HlsMuxedCodecDeferred(component) => {
            let height = NonZeroU32::new(component.height().pixels())
                .ok_or_else(|| anyhow!("HLS deferred video height равен нулю"))?;
            let width = component
                .width()
                .and_then(|width| NonZeroU32::new(width.pixels()));
            Some((width.unwrap_or(height), height))
        }
        _ => video
            .map(|track| -> Result<(NonZeroU32, NonZeroU32)> {
                let width = NonZeroU32::new(
                    track
                        .width_pixels()
                        .ok_or_else(|| anyhow!("HLS video width evidence отсутствует"))?,
                )
                .ok_or_else(|| anyhow!("HLS video width равен нулю"))?;
                let height = NonZeroU32::new(
                    track
                        .height()
                        .ok_or_else(|| anyhow!("HLS video height evidence отсутствует"))?
                        .pixels(),
                )
                .ok_or_else(|| anyhow!("HLS video height равен нулю"))?;
                Ok((width, height))
            })
            .transpose()?,
    };
    let codecs = match (video, audio_track) {
        (Some(video), Some(audio)) => Some(
            format!(
                "{},{}",
                video.codec().raw().as_str(),
                audio.codec().raw().as_str()
            )
            .into_boxed_str(),
        ),
        (Some(video), None) => Some(video.codec().raw().as_str().into()),
        (None, Some(audio)) => Some(audio.codec().raw().as_str().into()),
        (None, None) => None,
    };
    Ok((
        HlsVariantSelectionIntent {
            resolution,
            codecs,
            audio,
            main_track_layout,
        },
        HlsComponentContainerIntent {
            main: if matches!(layout, StreamLayout::HlsMuxedCodecDeferred(_)) {
                HlsContainerEvidence::ContentProbe
            } else {
                HlsContainerEvidence::Exact(required_container(
                    container
                        .consistent_family()
                        .map_err(|conflict| anyhow!("HLS container hints conflict: {conflict:?}"))?
                        .ok_or_else(|| anyhow!("HLS container evidence отсутствует"))?,
                )?)
            },
            alternate_audio: matches!(layout, StreamLayout::Muxed(_))
                .then_some(HlsContainerEvidence::ContentProbe),
        },
    ))
}

/// Fail-closed codec proof для deferred HLS после manifest/demux open.
fn prove_deferred_hls_codec_evidence(
    candidate: &YtDlpNormalizedCandidate,
    demuxer: &mut dyn Demuxer,
    capability_probe: &mut crate::web_media_open::catalog_capabilities::AppCatalogCapabilityProbe,
) -> Result<()> {
    let StreamLayout::HlsMuxedCodecDeferred(_) = candidate.descriptor().layout() else {
        return Ok(());
    };
    // ProgressiveDemuxer публикует tracks только через initial TracksChanged.
    wait_for_initial_tracks_changed(demuxer)?;
    let tracks = demuxer.tracks();
    let video = tracks
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .ok_or_else(|| anyhow!("deferred HLS demuxer не содержит video track"))?;
    let audio = tracks
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .ok_or_else(|| anyhow!("deferred HLS demuxer не содержит audio track"))?;
    capability_probe
        .prove_video(video)
        .map_err(|_| anyhow!("deferred HLS video codec не поддерживается"))?;
    capability_probe
        .prove_audio(audio)
        .map_err(|_| anyhow!("deferred HLS audio codec не поддерживается"))?;
    Ok(())
}

/// Media-open worker ждёт first TracksChanged до capability prove / Installed.
fn wait_for_initial_tracks_changed(demuxer: &mut dyn Demuxer) -> Result<()> {
    const INITIAL_TRACKS_DEADLINE: Duration = Duration::from_secs(30);
    let deadline = Instant::now() + INITIAL_TRACKS_DEADLINE;
    loop {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "deferred HLS не опубликовал TracksChanged до open deadline"
            ));
        }
        match demuxer.next_event().context("deferred HLS demuxer next_event")? {
            DemuxReadEvent::TracksChanged(_) => return Ok(()),
            DemuxReadEvent::TemporarilyUnavailable(hint) => {
                std::thread::sleep(hint.retry_after().min(Duration::from_millis(50)));
            }
            DemuxReadEvent::EndOfStream => {
                return Err(anyhow!(
                    "deferred HLS достиг EOS до initial TracksChanged"
                ));
            }
            DemuxReadEvent::Packet(_) | DemuxReadEvent::MediaMetadataChanged(_) => {
                // Packet без TracksChanged нарушает ProgressiveDemuxer contract; всё равно fail-closed.
                return Err(anyhow!(
                    "deferred HLS опубликовал media event до initial TracksChanged"
                ));
            }
        }
    }
}

/// Проецирует только extractor evidence, которое можно точно сопоставить с AUDIO rendition.
fn audio_rendition_evidence(
    audio: &web_media_core::AudioTrackDescriptor,
) -> HlsAudioRenditionEvidence {
    HlsAudioRenditionEvidence {
        name: None,
        language: audio.language().map(|language| language.as_str().into()),
        channel_count: audio
            .channels()
            .and_then(|channels| std::num::NonZeroU16::new(channels.get())),
    }
}

fn required_container(container: ContainerFamily) -> Result<HlsRequiredContainer> {
    match container {
        ContainerFamily::MpegTs => Ok(HlsRequiredContainer::TransportStream),
        ContainerFamily::IsoBmff | ContainerFamily::FragmentedIsoBmff => {
            Ok(HlsRequiredContainer::FragmentedMp4)
        }
        other => Err(anyhow!(
            "HLS container {other:?} не входит в TS/fMP4 profile"
        )),
    }
}

pub(crate) fn hls_policy(limits: AdaptiveTransportLimits) -> Result<HlsVodOpenPolicy> {
    Ok(HlsVodOpenPolicy {
        parser_limits: HlsParserLimits::default(),
        demux_sniff_budget: DemuxSniffBudget::new(
            NonZeroUsize::new(64 * 1_024).expect("HLS sniff bytes"),
            NonZeroUsize::new(8).expect("HLS sniff segments"),
            Duration::from_secs(2),
        )?,
        progressive_limits: ProgressiveDemuxBufferLimits::new(
            NonZeroUsize::new(256).expect("HLS event queue"),
            NonZeroUsize::new(16 * 1_024 * 1_024).expect("HLS encoded queue"),
        ),
        retry_hint: DemuxRetryHint::new(Duration::from_millis(10))?,
        composite_lead_policy: CompositeComponentLeadPolicy::single_pending_packet(
            Duration::from_secs(3),
            NonZeroUsize::new(4 * 1_024 * 1_024).expect("HLS composite packet"),
        )?,
        maximum_key_resource_bytes: NonZeroUsize::new(64).expect("HLS key response"),
        maximum_seek_index_entries: NonZeroUsize::new(4_096).expect("HLS seek anchors"),
        maximum_seek_replay_events: NonZeroUsize::new(65_536).expect("HLS seek replay events"),
        maximum_seek_replay_bytes: limits.maximum_segment_bytes,
    })
}

fn hls_catalog_policy() -> Result<HlsCatalogBuildPolicy> {
    Ok(HlsCatalogBuildPolicy {
        catalog_limit: web_media_core::ComponentVariantCatalogLimit::new(256)?,
        compatibility_edge_limit: web_media_core::ComponentVariantEdgeLimit::new(4_096)?,
        maximum_unique_children: NonZeroUsize::new(256)
            .expect("HLS catalog child limit is non-zero"),
    })
}

/// Planner HLS transport output не делает TS playable для progressive HTTP rows.
pub(crate) fn hls_transport_input() -> DemuxInputCapability {
    DemuxInputCapability::OrderedSegments
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::time::Duration;

    use media_core::{
        DemuxReadEvent, DemuxRetryHint, DemuxSeekResult, DemuxSeekability, DemuxTrackListUpdate,
        Demuxer, TimelineNotSeekableReason, TrackId, TrackInfo, TrackKind,
    };

    use super::wait_for_initial_tracks_changed;

    struct ScriptedDemuxer {
        events: VecDeque<DemuxReadEvent>,
        tracks: Vec<TrackInfo>,
    }

    impl Demuxer for ScriptedDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &self.tracks
        }

        fn duration(&self) -> Option<Duration> {
            None
        }

        fn seekability(&self) -> DemuxSeekability {
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::UnknownTimeline,
            }
        }

        fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
            Ok(self
                .events
                .pop_front()
                .unwrap_or(DemuxReadEvent::EndOfStream))
        }

        fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            panic!("scripted demuxer is not seekable")
        }
    }

    fn track(id: u32, kind: TrackKind) -> TrackInfo {
        TrackInfo {
            id: TrackId::new(id),
            kind,
            codec_id: "test".into(),
            codec_private: None,
            time_base: None,
            duration: None,
            sample_rate: None,
            channels: None,
            video: None,
        }
    }

    #[test]
    fn wait_for_initial_tracks_skips_temporary_unavailability() {
        let published = vec![
            track(1, TrackKind::Video),
            track(2, TrackKind::Audio),
        ];
        let mut demuxer = ScriptedDemuxer {
            events: VecDeque::from([
                DemuxReadEvent::TemporarilyUnavailable(
                    DemuxRetryHint::new(Duration::from_millis(1)).expect("retry hint"),
                ),
                DemuxReadEvent::TemporarilyUnavailable(
                    DemuxRetryHint::new(Duration::from_millis(1)).expect("retry hint"),
                ),
                DemuxReadEvent::TracksChanged(DemuxTrackListUpdate::new(
                    published.clone(),
                    None,
                )),
            ]),
            tracks: published,
        };

        wait_for_initial_tracks_changed(&mut demuxer).expect("TracksChanged after unavailable");
        assert_eq!(demuxer.tracks().len(), 2);
    }

    #[test]
    fn wait_for_initial_tracks_rejects_eos_before_tracks() {
        let mut demuxer = ScriptedDemuxer {
            events: VecDeque::from([DemuxReadEvent::EndOfStream]),
            tracks: Vec::new(),
        };
        let error = wait_for_initial_tracks_changed(&mut demuxer).expect_err("EOS before tracks");
        assert!(error.to_string().contains("EOS"));
    }
}
