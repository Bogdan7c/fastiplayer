//! Production composition selected yt-dlp HLS candidate -> uninstalled HLS VOD runtime.

#[path = "web_media_hls_open/native_live.rs"]
mod native_live;
#[path = "web_media_hls_open/native_vod.rs"]
mod native_vod;
#[path = "web_media_hls_open/runtime_policy.rs"]
mod runtime_policy;

pub(crate) use native_live::{
    PreparedNativeHlsLive, prepare_native_hls_catalog_live, prepare_native_hls_live,
};
#[cfg(test)]
pub(crate) use native_vod::prepare_native_hls_player_media;
pub(crate) use native_vod::{
    PreparedNativeHlsVod, discover_native_hls_catalog, prepare_native_hls_catalog_vod,
    prepare_native_hls_vod,
};

use std::num::{NonZeroU8, NonZeroU32};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use demux_api::{
    DemuxRegistry, ProgressiveAsyncSeekEnqueueError, ProgressiveAsyncSeekHandle,
    ProgressiveAsyncSeekOutcome, ProgressiveDemuxReadiness, ProgressiveSeekFence,
    ProgressiveSeekRequestId,
};
use fastiplayer_config::NetworkConfig;
use media_core::{
    DemuxReadEvent, Demuxer, DynamicMediaTimelineEpoch, DynamicMediaTimelinePort,
    DynamicMediaTimelinePortGeneration, TrackKind,
};
use player_core::{
    PreparedDemuxSeekEnqueueError, PreparedDemuxSeekLandingPolicy, PreparedDemuxSeekOutcome,
    PreparedDemuxSeekPort, PreparedDemuxSeekReceipt, PreparedDemuxSeekRequestId,
    PreparedInitialPosition,
};
use service_ytdlp::{
    YtDlpHlsManifestInputKind, YtDlpNormalizedCandidate, YtDlpTransportRequestContext,
};
use source_core::{CancellationToken, SourceRuntimeConfig};
use web_media_adaptive::{AdaptiveHttpContext, AdaptiveRetryPolicy};
use web_media_core::{ContainerFamily, StreamLayout, TransportFamily};
use web_media_hls::{
    ExtractorAesOverride, HlsAudioLayoutIntent, HlsAudioRenditionEvidence,
    HlsCatalogCapabilityProofPort, HlsCatalogDiscoveryOutcome, HlsCatalogDiscoveryRequest,
    HlsCatalogPresentation, HlsComponentContainerIntent, HlsContainerEvidence,
    HlsEndpointRefreshPort, HlsInitialPositionProofCapability, HlsInitialPositionProofTakeOutcome,
    HlsInitialReadinessCapability, HlsLiveOpenRequest, HlsMainTrackLayoutIntent, HlsManifestInput,
    HlsRequestOverrides, HlsRequiredContainer, HlsVariantSelectionIntent, HlsVodOpenRequest,
    HlsVodOpenResult, HlsVodRestoreFallbackReason, HlsVodSeekLandingPolicy, HlsVodStartDisposition,
    HlsVodStartIntent, SecretInlineMediaPlaylist, discover_hls_catalog,
    prepare_hls_catalog_live_receipted, prepare_hls_catalog_vod_receipted,
    prepare_hls_catalog_vod_receipted_at_start, prepare_hls_live_receipted,
    prepare_hls_vod_receipted, prepare_hls_vod_receipted_at_start,
};
use web_media_transport_api::{SourceGeneration, TransportProviderId};

use runtime_policy::{hls_async_seek_limits, hls_catalog_policy};
pub(crate) use runtime_policy::{hls_policy, hls_transport_input};

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
        StreamLayout::ContentProbed(_) => false,
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
    endpoint_expiry_observer: Option<Arc<dyn web_media_transport_api::EndpointExpiryObserver>>,
) -> Result<PreparedHlsCandidate> {
    let generation = crate::web_media_adaptive_config::initial_adaptive_source_generation();
    let projected = project_hls_runtime_material(
        candidate,
        provider_id,
        generation,
        source_config,
        network_config,
        cancellation,
        endpoint_expiry_observer,
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
                        provider_default_variant_index: None,
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
        let initial_readiness = opened.initial_readiness();
        let (mut demuxer, timeline_port, _) = opened.into_parts();
        // Любой live HLS должен доказать хотя бы один реальный track до Installed.
        // Иначе неизвестный fMP4 sample entry превращается в бесконечное пустое playback state.
        wait_for_initial_hls_tracks(demuxer.as_mut(), &initial_readiness)
            .context("HLS live не достиг install-ready track состояния")?;
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
                    provider_default_variant_index: None,
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
    let initial_readiness = opened.initial_readiness();
    let mut demuxer = opened.into_demuxer();
    // Static HLS обязан опубликовать tracks и duration до PreparedMedia/Installed:
    // startup restore начинается сразу после Installed и не ждёт поздний TracksChanged.
    wait_for_initial_hls_tracks(demuxer.as_mut(), &initial_readiness)
        .context("HLS VOD не достиг install-ready track/timeline состояния")?;
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
    endpoint_expiry_observer: Option<Arc<dyn web_media_transport_api::EndpointExpiryObserver>>,
) -> Result<HlsProjectedRuntimeMaterial> {
    let context = YtDlpTransportRequestContext::new(provider_id, generation, cancellation);
    let mut transport_request = candidate
        .hls_transport_request(&context)
        .context("Не удалось спроецировать yt-dlp HLS transport material")?;
    if let Some(observer) = endpoint_expiry_observer {
        transport_request = transport_request.with_endpoint_expiry_observer(observer);
    }
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
            crate::web_media_adaptive_config::maximum_adaptive_retry_after(),
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
        StreamLayout::ContentProbed(_) => {
            return Err(anyhow!(
                "HLS open не поддерживает generic content-probed layout"
            ));
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
                hls_main_container_evidence(
                    container
                        .consistent_family()
                        .map_err(|conflict| anyhow!("HLS container hints conflict: {conflict:?}"))?
                        .ok_or_else(|| anyhow!("HLS container evidence отсутствует"))?,
                )?
            },
            alternate_audio: matches!(layout, StreamLayout::Muxed(_))
                .then_some(HlsContainerEvidence::ContentProbe),
        },
    ))
}

/// Fail-closed codec proof для уже опубликованных deferred HLS tracks.
fn prove_deferred_hls_codec_evidence(
    candidate: &YtDlpNormalizedCandidate,
    demuxer: &mut dyn Demuxer,
    capability_probe: &mut crate::web_media_open::catalog_capabilities::AppCatalogCapabilityProbe,
) -> Result<()> {
    let StreamLayout::HlsMuxedCodecDeferred(_) = candidate.descriptor().layout() else {
        return Ok(());
    };
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

/// Media-open worker ждёт непустой authoritative track snapshot до capability prove / Installed.
fn wait_for_initial_hls_tracks(
    demuxer: &mut dyn Demuxer,
    readiness: &HlsInitialReadinessCapability,
) -> Result<()> {
    const INITIAL_TRACKS_DEADLINE: Duration = Duration::from_secs(30);
    let deadline = Instant::now() + INITIAL_TRACKS_DEADLINE;
    loop {
        // HLS component может применить initial TracksChanged во время bootstrap и передать
        // app-level demuxer уже с готовым snapshot-ом. Состояние важнее наличия replay event-а.
        if !demuxer.tracks().is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "HLS runtime не опубликовал непустой track snapshot до open deadline"
            ));
        }
        match demuxer
            .next_event()
            .context("HLS runtime demuxer next_event")?
        {
            // Demuxer применяет lifecycle event к своему snapshot-у; следующая итерация
            // проверит именно owner state и не установит пустую topology.
            DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::MediaMetadataChanged(_) => {
                // Metadata revision законно может предшествовать track topology.
                // Demuxer уже сохранил snapshot в `media_metadata()`, поэтому install его не теряет.
            }
            DemuxReadEvent::TemporarilyUnavailable(_) => match readiness {
                HlsInitialReadinessCapability::AlreadySynchronous => {
                    return Err(anyhow!(
                        "синхронный HLS runtime не опубликовал initial topology сразу"
                    ));
                }
                HlsInitialReadinessCapability::Progressive(port) => {
                    match port.wait_until(deadline) {
                        ProgressiveDemuxReadiness::EventAvailable => {}
                        ProgressiveDemuxReadiness::Cancelled => {
                            return Err(anyhow!("HLS runtime отменён до initial track topology"));
                        }
                        ProgressiveDemuxReadiness::WorkerStopped => {
                            return Err(anyhow!(
                                "HLS runtime worker завершился до initial track topology"
                            ));
                        }
                        ProgressiveDemuxReadiness::DeadlineReached => {
                            return Err(anyhow!(
                                "HLS runtime не опубликовал непустой track snapshot до open deadline"
                            ));
                        }
                    }
                }
            },
            DemuxReadEvent::EndOfStream => {
                return Err(anyhow!(
                    "HLS runtime достиг EOS до непустого initial track snapshot"
                ));
            }
            DemuxReadEvent::Packet(_) => {
                // Packet при пустой topology нарушает ProgressiveDemuxer contract.
                return Err(anyhow!(
                    "HLS runtime опубликовал packet до непустого initial track snapshot"
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

/// Переводит extractor container hint в честное evidence о реальных HLS segments.
fn hls_main_container_evidence(container: ContainerFamily) -> Result<HlsContainerEvidence> {
    match container {
        // Exact MPEG-TS identity однозначно задаёт segment container без дополнительного probe.
        ContainerFamily::MpegTs => Ok(HlsContainerEvidence::Exact(
            HlsRequiredContainer::TransportStream,
        )),
        // Generic MP4 у yt-dlp описывает output format и не доказывает наличие EXT-X-MAP/fMP4.
        ContainerFamily::IsoBmff => Ok(HlsContainerEvidence::ContentProbe),
        // Явный fragmented ISO-BMFF identity остаётся достаточным exact evidence.
        ContainerFamily::FragmentedIsoBmff => Ok(HlsContainerEvidence::Exact(
            HlsRequiredContainer::FragmentedMp4,
        )),
        other => Err(anyhow!(
            "HLS container {other:?} не входит в TS/fMP4 profile"
        )),
    }
}

#[cfg(test)]
#[path = "web_media_hls_open/tests.rs"]
mod tests;
