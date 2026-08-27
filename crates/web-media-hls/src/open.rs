use std::sync::Arc;

use demux_api::{
    DemuxOpenError, ProgressiveAsyncSeekLimits, ProgressiveDemuxStartupError, ProgressiveDemuxer,
    ProgressiveRuntimeGeneration, ProgressiveSeekController,
};
use hls_playlist_core::{
    HlsParseError, HlsParseRequest, HlsPlaylist, HlsProfileError, MasterPlaylist,
    MediaContainerIntent, MediaPlaylist, MediaRendition, MediaRenditionType, VariantStream,
    parse_hls_playlist, validate_initial_profile, validate_vod_profile,
};
use media_core::TrackKind;
use source_core::HttpRequestTarget;
use web_media_adaptive::{
    AdaptiveResourceFetchRequest, AdaptiveResourcePurpose, AdaptiveResourceQueryApplication,
    AdaptiveTransportError,
};

use crate::catalog::{HlsCatalogMatchMode, HlsCatalogReopenError, HlsCatalogReopenSelection};
use crate::epoch_demux::HlsComponentFactory;
use crate::initial_open::{
    HlsInitialComponentRole, HlsPreparedInitialComponent, prepare_initial_component,
};
use crate::initial_position_proof::HlsInitialPositionProofPublisher;
use crate::plan::{HlsComponentPlan, HlsPlanError, build_component_plan};
use crate::seek::SharedHlsSeekIndex;
use crate::{
    HlsAudioLayoutIntent, HlsAudioRenditionEvidence, HlsContainerEvidence,
    HlsMainTrackLayoutIntent, HlsManifestInput, HlsRequiredContainer,
    HlsSubtitleRenditionDescriptor, HlsVariantSelectionIntent, HlsVodOpenRequest,
    HlsVodStartIntent,
};

mod container_probe;
mod deferred_initial;
mod result;

pub(crate) use container_probe::{required_audio_container, required_main_container};
use deferred_initial::{HlsDeferredInitialComponent, open_deferred_initial_components};
pub use result::{HlsInitialReadinessCapability, HlsVodOpenResult};

/// Secret-safe prepare/open failure до production player mutation.
#[derive(Debug, thiserror::Error)]
pub enum HlsVodOpenError {
    #[error("HLS manifest fetch failed: {0}")]
    Transport(#[from] AdaptiveTransportError),
    #[error("HLS manifest invalid: {0}")]
    Parse(#[from] HlsParseError),
    #[error("HLS initial VOD profile rejected: {0}")]
    Profile(#[from] HlsProfileError),
    #[error("HLS child target resolution failed: {0}")]
    Target(#[from] source_core::HttpRequestTargetError),
    #[error("inline hls_media_playlist_data обязан быть media playlist")]
    InlineManifestWasMaster,
    #[error("fetched HLS manifest принадлежит другой source generation")]
    FetchedManifestGenerationMismatch,
    #[error("selected HLS child URI вернул nested master playlist")]
    NestedMasterPlaylist,
    #[error("HLS master не содержит variant, совместимый с explicit selection intent")]
    MissingVariant,
    #[error("HLS master содержит несколько variant, совместимых с explicit selection intent")]
    AmbiguousVariant,
    #[error("HLS master не содержит требуемый compatible AUDIO rendition")]
    MissingAudioRendition,
    #[error("HLS master содержит несколько equally compatible AUDIO rendition")]
    AmbiguousAudioRendition,
    #[error("media-only HLS playlist не может удовлетворить separate-audio intent")]
    SeparateAudioRequiresMaster,
    #[error("HLS main component не имеет required container evidence")]
    MissingMainContainerEvidence,
    #[error("HLS main component имеет ambiguous container evidence")]
    AmbiguousMainContainerEvidence,
    #[error("HLS main content probe доказал container вне TS/fMP4 profile")]
    UnsupportedMainContainer,
    #[error("HLS main content probe не смог открыть bounded media bytes: {0}")]
    MainContainerProbeOpen(#[source] DemuxOpenError),
    #[error("HLS alternate audio не имеет required container evidence")]
    MissingAudioContainerEvidence,
    #[error("HLS alternate audio имеет ambiguous container evidence")]
    AmbiguousAudioContainerEvidence,
    #[error("HLS alternate audio content probe доказал container вне TS/fMP4 profile")]
    UnsupportedAudioContainer,
    #[error("HLS alternate audio content probe не смог открыть bounded media bytes: {0}")]
    AudioContainerProbeOpen(#[source] DemuxOpenError),
    #[error("muxed HLS intent не принимает alternate-audio container evidence")]
    UnexpectedAudioContainerEvidence,
    #[error("HLS resource plan invalid: {0}")]
    Plan(#[from] HlsPlanError),
    #[error("HLS deferred demux worker failed to start: {0}")]
    ProgressiveStartup(#[from] ProgressiveDemuxStartupError),
    #[error("HLS key fetch bound должен вмещать exact 16-byte AES key")]
    KeyFetchBoundTooSmall,
    #[error("HLS key fetch bound превышает shared adaptive resource limit")]
    KeyFetchBoundExceedsAdaptiveLimit,
    #[error("HLS seek index должен вмещать как минимум initial video и audio anchors")]
    SeekIndexBoundTooSmall,
    #[error("HLS initial restore target находится за finite VOD duration")]
    InitialRestoreOutsideVod,
    #[error("HLS initial restore не нашёл containing manifest candidate")]
    InitialRestoreCandidateMissing,
    #[error("HLS catalog reopen rejected: {0}")]
    CatalogReopen(#[from] HlsCatalogReopenError),
}

/// Actual demux tracks нарушили explicit selected topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "HLS {component} track shape mismatch: expected {expected:?}, video={video_tracks}, audio={audio_tracks}"
)]
pub(crate) struct HlsTrackShapeError {
    component: &'static str,
    expected: HlsMainTrackLayoutIntent,
    video_tracks: usize,
    audio_tracks: usize,
}

/// Выполняет blocking manifest orchestration и возвращает неустановленный nonblocking runtime.
///
/// Caller вызывает функцию на media-open worker-е; app-owned staged preparation дожидается
/// initial tracks/capability preflight и только затем проходит существующий player commit barrier.
pub fn prepare_hls_vod(request: HlsVodOpenRequest) -> Result<HlsVodOpenResult, HlsVodOpenError> {
    prepare_hls_vod_with_seek_boundary(request, None, None, HlsVodStartIntent::Beginning)
}

/// Готовит HLS VOD с worker-executed generation-fenced seek receipt boundary.
///
/// Это provider-половина staged preauthorization seek. Caller сохраняет handle
/// до type erasure demuxer-а и позже прикрепляет его к neutral player seek port.
pub fn prepare_hls_vod_receipted(
    request: HlsVodOpenRequest,
    asynchronous_seek_limits: ProgressiveAsyncSeekLimits,
) -> Result<HlsVodOpenResult, HlsVodOpenError> {
    prepare_hls_vod_with_seek_boundary(
        request,
        Some(asynchronous_seek_limits),
        None,
        HlsVodStartIntent::Beginning,
    )
}

/// Готовит receipted HLS VOD сразу из caller-owned restore position.
pub fn prepare_hls_vod_receipted_at_start(
    request: HlsVodOpenRequest,
    asynchronous_seek_limits: ProgressiveAsyncSeekLimits,
    start: HlsVodStartIntent,
) -> Result<HlsVodOpenResult, HlsVodOpenError> {
    prepare_hls_vod_with_seek_boundary(request, Some(asynchronous_seek_limits), None, start)
}

/// Открывает exact proven catalog selection с worker-receipted seek boundary.
pub fn prepare_hls_catalog_vod_receipted(
    mut request: HlsVodOpenRequest,
    selection: HlsCatalogReopenSelection,
    asynchronous_seek_limits: ProgressiveAsyncSeekLimits,
) -> Result<HlsVodOpenResult, HlsVodOpenError> {
    request.selection = selection.runtime_intent();
    prepare_hls_vod_with_seek_boundary(
        request,
        Some(asynchronous_seek_limits),
        Some(selection),
        HlsVodStartIntent::Beginning,
    )
}

fn prepare_hls_vod_with_seek_boundary(
    request: HlsVodOpenRequest,
    asynchronous_seek_limits: Option<ProgressiveAsyncSeekLimits>,
    catalog_selection: Option<HlsCatalogReopenSelection>,
    start: HlsVodStartIntent,
) -> Result<HlsVodOpenResult, HlsVodOpenError> {
    if request.generation != request.http.source_generation() {
        return Err(HlsVodOpenError::Transport(
            AdaptiveTransportError::StaleGeneration {
                current: request.http.source_generation(),
                received: request.generation,
            },
        ));
    }
    if request.http.cancellation().is_cancelled() {
        return Err(HlsVodOpenError::Transport(
            AdaptiveTransportError::Cancelled,
        ));
    }
    validate_key_fetch_bound(&request)?;
    let (top_playlist, top_base, was_inline) = load_top_playlist(&request)?;
    validate_initial_profile(&top_playlist)?;

    let selected = match top_playlist {
        HlsPlaylist::Media(media) => {
            if catalog_selection.is_some() {
                return Err(HlsCatalogReopenError::MissingPrivateRow.into());
            }
            if matches!(&request.selection.audio, HlsAudioLayoutIntent::Separate(_)) {
                return Err(HlsVodOpenError::SeparateAudioRequiresMaster);
            }
            if !matches!(
                request.containers.alternate_audio,
                None | Some(HlsContainerEvidence::ContentProbe)
            ) {
                return Err(HlsVodOpenError::UnexpectedAudioContainerEvidence);
            }
            SelectedPlans {
                main: prepare_initial_component(
                    media,
                    &top_base,
                    &request,
                    request.containers.main,
                    HlsInitialComponentRole::Main,
                    start,
                )?,
                audio: None,
                subtitles: Vec::new(),
                main_track_layout: request.selection.main_track_layout,
            }
        }
        HlsPlaylist::Master(_) if was_inline => {
            return Err(HlsVodOpenError::InlineManifestWasMaster);
        }
        HlsPlaylist::Master(master) => match catalog_selection.as_ref() {
            Some(selection) => {
                select_and_load_catalog_master(master, &top_base, &request, selection)?
            }
            None => select_and_load_master(master, &top_base, &request, start)?,
        },
    };

    let duration = selected
        .audio
        .as_ref()
        .map_or(selected.main.plan.duration, |audio| {
            selected.main.plan.duration.max(audio.plan.duration)
        });
    let start_disposition = selected.main.start_disposition;
    let effective_start = start_disposition.effective_start();
    let cancellation = request.http.cancellation().clone();
    let main_http = request.http.clone();
    let audio_http = request.http.clone();
    let generation = request.generation;
    let policy = request.policy;
    let registry = request.demux_registry;
    let main_plan = selected.main.plan;
    let main_initial_open = selected.main.initial_open;
    let main_active_read_control = selected.main.active_read_control;
    let audio_component = selected.audio;
    let main_track_layout = selected.main_track_layout;
    let main_seek_index = SharedHlsSeekIndex::new(policy.maximum_seek_index_entries.get());
    let audio_seek_index = audio_component
        .as_ref()
        .map(|_| SharedHlsSeekIndex::new(policy.maximum_seek_index_entries.get()));
    let preview_main_index = main_seek_index.clone();
    let preview_audio_index = audio_seek_index.clone();
    let seek_controller = ProgressiveSeekController::new(move |request| {
        let main_result = preview_main_index.lock().preview_and_pin(request)?;
        if let Some(audio_index) = &preview_audio_index {
            audio_index
                .lock()
                .preview_and_pin(media_core::DemuxSeekRequest::accurate(request.timestamp))?;
        }
        Ok(main_result)
    });
    let main_factory = HlsComponentFactory::new(
        main_plan,
        main_http,
        generation,
        policy,
        Arc::clone(&registry),
        main_seek_index,
        main_active_read_control,
    );
    let audio_deferred = audio_component.map(|audio_component| {
        let audio_index = audio_seek_index
            .expect("audio seek index создаётся ровно вместе с alternate component");
        HlsDeferredInitialComponent {
            factory: HlsComponentFactory::new(
                audio_component.plan,
                audio_http,
                generation,
                policy,
                registry,
                audio_index,
                audio_component.active_read_control,
            ),
            initial_open: audio_component.initial_open,
        }
    });
    let (initial_position_proof, proof_publisher) =
        HlsInitialPositionProofPublisher::for_start(effective_start, generation);
    let open_inner = move || {
        open_deferred_initial_components(
            HlsDeferredInitialComponent {
                factory: main_factory,
                initial_open: main_initial_open,
            },
            audio_deferred,
            main_track_layout,
            policy,
            proof_publisher,
        )
    };
    let progressive = match asynchronous_seek_limits {
        Some(asynchronous_seek_limits) => ProgressiveDemuxer::new_deferred_receipted_seekable(
            open_inner,
            seek_controller,
            cancellation,
            policy.progressive_limits,
            policy.retry_hint,
            ProgressiveRuntimeGeneration::new(generation.value()),
            asynchronous_seek_limits,
        )?,
        None => ProgressiveDemuxer::new_deferred_seekable(
            open_inner,
            seek_controller,
            cancellation,
            policy.progressive_limits,
            policy.retry_hint,
        )?,
    };

    Ok(HlsVodOpenResult::new(
        progressive,
        selected.subtitles,
        duration,
        initial_position_proof,
        start_disposition,
    ))
}

pub(crate) fn validate_key_fetch_bound(request: &HlsVodOpenRequest) -> Result<(), HlsVodOpenError> {
    if request.policy.maximum_seek_index_entries.get() < 2 {
        return Err(HlsVodOpenError::SeekIndexBoundTooSmall);
    }
    if request.policy.maximum_key_resource_bytes.get() < crate::SecretAes128Key::BYTE_LENGTH {
        return Err(HlsVodOpenError::KeyFetchBoundTooSmall);
    }
    if request.policy.maximum_key_resource_bytes
        > request
            .http
            .maximum_resource_bytes(AdaptiveResourcePurpose::EncryptionKey)
    {
        return Err(HlsVodOpenError::KeyFetchBoundExceedsAdaptiveLimit);
    }
    Ok(())
}

pub(crate) fn load_top_playlist(
    request: &HlsVodOpenRequest,
) -> Result<(HlsPlaylist, HttpRequestTarget, bool), HlsVodOpenError> {
    match &request.manifest {
        HlsManifestInput::InlineMedia {
            selected_url,
            playlist,
        } => Ok((
            parse_playlist(playlist.as_bytes(), selected_url, request)?,
            selected_url.clone(),
            true,
        )),
        HlsManifestInput::Fetch { selected_url } => {
            let resource = fetch_manifest(selected_url.clone(), request)?;
            let playlist = parse_playlist(resource.bytes(), resource.final_target(), request)?;
            Ok((playlist, resource.final_target().clone(), false))
        }
        HlsManifestInput::FetchedTop(manifest) => {
            if manifest.source_generation() != request.generation {
                return Err(HlsVodOpenError::FetchedManifestGenerationMismatch);
            }
            Ok((
                parse_playlist(manifest.playlist_bytes(), manifest.effective_url(), request)?,
                manifest.effective_url().clone(),
                false,
            ))
        }
    }
}

pub(crate) fn fetch_manifest(
    target: HttpRequestTarget,
    request: &HlsVodOpenRequest,
) -> Result<web_media_adaptive::AdaptiveFetchedResource, HlsVodOpenError> {
    Ok(request.http.fetch_resource_blocking(
        AdaptiveResourceFetchRequest::full(
            request.generation,
            target.clone(),
            request
                .http
                .maximum_resource_bytes(AdaptiveResourcePurpose::Manifest),
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::BypassScopedQuery,
        )
        .with_secret_forwarding(request.http.resource_secret_forwarding_for(&target)),
    )?)
}

pub(crate) fn parse_playlist(
    bytes: &[u8],
    base: &HttpRequestTarget,
    request: &HlsVodOpenRequest,
) -> Result<HlsPlaylist, HlsVodOpenError> {
    Ok(parse_hls_playlist(HlsParseRequest {
        document_bytes: bytes,
        reference_base: Some(base.expose_secret_for_request()),
        limits: request.policy.parser_limits,
    })?)
}

fn select_and_load_master(
    master: MasterPlaylist,
    base: &HttpRequestTarget,
    request: &HlsVodOpenRequest,
    start: HlsVodStartIntent,
) -> Result<SelectedPlans, HlsVodOpenError> {
    let selected = select_master(&master, &request.selection)?;
    let variant_target = base.resolve_reference(selected.variant.uri.expose_for_resolution())?;
    let variant_resource = fetch_manifest(variant_target, request)?;
    let variant_playlist = parse_playlist(
        variant_resource.bytes(),
        variant_resource.final_target(),
        request,
    )?;
    let HlsPlaylist::Media(variant_media) = variant_playlist else {
        return Err(HlsVodOpenError::NestedMasterPlaylist);
    };
    let main = prepare_initial_component(
        variant_media,
        variant_resource.final_target(),
        request,
        request.containers.main,
        HlsInitialComponentRole::Main,
        start,
    )?;
    // Main component единолично решает permissive checkpoint fallback. Separate audio получает
    // уже строгий effective intent, чтобы не открыть несовместимое начало независимо от video.
    let alternate_audio_start = main.start_disposition.strict_component_start();

    let audio = match selected.audio {
        Some(rendition) => {
            let reference = rendition
                .uri
                .as_ref()
                .ok_or(HlsVodOpenError::MissingAudioRendition)?;
            let target = base.resolve_reference(reference.expose_for_resolution())?;
            let resource = fetch_manifest(target, request)?;
            let playlist = parse_playlist(resource.bytes(), resource.final_target(), request)?;
            let HlsPlaylist::Media(media) = playlist else {
                return Err(HlsVodOpenError::NestedMasterPlaylist);
            };
            let evidence = request
                .containers
                .alternate_audio
                .ok_or(HlsVodOpenError::MissingAudioContainerEvidence)?;
            Some(prepare_initial_component(
                media,
                resource.final_target(),
                request,
                evidence,
                HlsInitialComponentRole::AlternateAudio,
                alternate_audio_start,
            )?)
        }
        None => {
            if !matches!(
                request.containers.alternate_audio,
                None | Some(HlsContainerEvidence::ContentProbe)
            ) {
                return Err(HlsVodOpenError::UnexpectedAudioContainerEvidence);
            }
            None
        }
    };
    Ok(SelectedPlans {
        main,
        audio,
        subtitles: selected.subtitles,
        main_track_layout: request.selection.main_track_layout,
    })
}

fn select_and_load_catalog_master(
    master: MasterPlaylist,
    base: &HttpRequestTarget,
    request: &HlsVodOpenRequest,
    selection: &HlsCatalogReopenSelection,
) -> Result<SelectedPlans, HlsVodOpenError> {
    let selected = selection.resolve_master(&master, HlsCatalogMatchMode::Exact)?;
    let main_target = base.resolve_reference(selected.main_reference.expose_for_resolution())?;
    let main_resource = fetch_manifest(main_target, request)?;
    let main_playlist =
        parse_playlist(main_resource.bytes(), main_resource.final_target(), request)?;
    let HlsPlaylist::Media(main_media) = main_playlist else {
        return Err(HlsVodOpenError::NestedMasterPlaylist);
    };
    let main = prepare_initial_component(
        main_media,
        main_resource.final_target(),
        request,
        HlsContainerEvidence::Exact(selected.main_container),
        HlsInitialComponentRole::Main,
        HlsVodStartIntent::Beginning,
    )?;

    let audio = selected
        .audio
        .map(|audio| {
            let target = base.resolve_reference(audio.reference.expose_for_resolution())?;
            let resource = fetch_manifest(target, request)?;
            let playlist = parse_playlist(resource.bytes(), resource.final_target(), request)?;
            let HlsPlaylist::Media(media) = playlist else {
                return Err(HlsVodOpenError::NestedMasterPlaylist);
            };
            prepare_initial_component(
                media,
                resource.final_target(),
                request,
                HlsContainerEvidence::Exact(audio.container),
                HlsInitialComponentRole::AlternateAudio,
                HlsVodStartIntent::Beginning,
            )
        })
        .transpose()?;

    Ok(SelectedPlans {
        main,
        audio,
        subtitles: selected.subtitles,
        main_track_layout: selected.main_shape,
    })
}

pub(crate) fn validate_and_plan_media(
    media: MediaPlaylist,
    container: HlsRequiredContainer,
    base: &HttpRequestTarget,
    request: &HlsVodOpenRequest,
) -> Result<HlsComponentPlan, HlsVodOpenError> {
    let playlist = HlsPlaylist::Media(media);
    validate_vod_profile(&playlist, None)?;
    let HlsPlaylist::Media(media) = playlist else {
        unreachable!("playlist constructed as media");
    };
    let plan = build_component_plan(&media, container, base, &request.overrides)?;
    plan.validate_resource_bound(
        request
            .http
            .maximum_resource_bytes(AdaptiveResourcePurpose::MediaSegment),
    )?;
    validate_vod_profile(
        &HlsPlaylist::Media(media),
        Some(match container {
            HlsRequiredContainer::TransportStream => MediaContainerIntent::TransportStream,
            HlsRequiredContainer::FragmentedMp4 => MediaContainerIntent::FragmentedMp4,
        }),
    )?;
    Ok(plan)
}

struct SelectedPlans {
    main: HlsPreparedInitialComponent,
    audio: Option<HlsPreparedInitialComponent>,
    subtitles: Vec<HlsSubtitleRenditionDescriptor>,
    main_track_layout: HlsMainTrackLayoutIntent,
}

#[derive(Debug)]
pub(crate) struct SelectedMaster {
    pub(crate) variant: VariantStream,
    pub(crate) audio: Option<MediaRendition>,
    pub(crate) subtitles: Vec<HlsSubtitleRenditionDescriptor>,
}

pub(crate) fn select_master(
    master: &MasterPlaylist,
    intent: &HlsVariantSelectionIntent,
) -> Result<SelectedMaster, HlsVodOpenError> {
    let base_matches = master
        .variants
        .iter()
        .filter(|variant| variant_matches(variant, intent))
        .collect::<Vec<_>>();
    if base_matches.is_empty() {
        return Err(HlsVodOpenError::MissingVariant);
    }
    if base_matches.len() > 1 && !intent.has_variant_evidence() {
        return Err(HlsVodOpenError::AmbiguousVariant);
    }

    let mut compatible = Vec::new();
    let mut saw_audio_ambiguity = false;
    for variant in base_matches {
        match select_audio_rendition(master, variant, &intent.audio) {
            Ok(audio) => compatible.push((variant, audio)),
            Err(HlsVodOpenError::AmbiguousAudioRendition) => saw_audio_ambiguity = true,
            Err(HlsVodOpenError::MissingAudioRendition) => {}
            Err(error) => return Err(error),
        }
    }
    let [(variant, audio)] = compatible.as_slice() else {
        return if compatible.is_empty() {
            Err(if saw_audio_ambiguity {
                HlsVodOpenError::AmbiguousAudioRendition
            } else {
                HlsVodOpenError::MissingAudioRendition
            })
        } else {
            Err(HlsVodOpenError::AmbiguousVariant)
        };
    };
    let subtitles = variant
        .subtitle_group
        .as_deref()
        .map(|group| {
            master
                .renditions
                .iter()
                .filter(|rendition| {
                    rendition.rendition_type == MediaRenditionType::Subtitles
                        && rendition.group_id.as_ref() == group
                })
                .filter_map(HlsSubtitleRenditionDescriptor::from_rendition)
                .collect()
        })
        .unwrap_or_default();
    Ok(SelectedMaster {
        variant: (*variant).clone(),
        audio: audio.filter(|rendition| rendition.uri.is_some()).cloned(),
        subtitles,
    })
}

fn variant_matches(variant: &VariantStream, intent: &HlsVariantSelectionIntent) -> bool {
    let resolution_matches = intent
        .resolution
        .is_none_or(|(width, height)| variant.resolution == Some((width.get(), height.get())));
    let codecs_match = intent.codecs.as_deref().is_none_or(|codecs| {
        variant
            .codecs
            .as_deref()
            .is_some_and(|variant_codecs| codec_sets_match(variant_codecs, codecs))
    });
    resolution_matches && codecs_match
}

pub(crate) fn codec_sets_match(variant_codecs: &str, required_codecs: &str) -> bool {
    let mut variant = variant_codecs.split(',').map(str::trim).collect::<Vec<_>>();
    let mut required = required_codecs
        .split(',')
        .map(str::trim)
        .collect::<Vec<_>>();
    variant.sort_unstable();
    required.sort_unstable();
    variant == required
}

fn select_audio_rendition<'master>(
    master: &'master MasterPlaylist,
    variant: &VariantStream,
    intent: &HlsAudioLayoutIntent,
) -> Result<Option<&'master MediaRendition>, HlsVodOpenError> {
    let Some(group) = variant.audio_group.as_deref() else {
        return match intent {
            HlsAudioLayoutIntent::Muxed | HlsAudioLayoutIntent::ManifestResolved(_) => Ok(None),
            HlsAudioLayoutIntent::Separate(_)
            | HlsAudioLayoutIntent::NativeGroupResolved { .. } => {
                Err(HlsVodOpenError::MissingAudioRendition)
            }
        };
    };
    let evidence = match intent {
        HlsAudioLayoutIntent::Muxed => None,
        HlsAudioLayoutIntent::Separate(evidence)
        | HlsAudioLayoutIntent::ManifestResolved(evidence) => Some(evidence),
        HlsAudioLayoutIntent::NativeGroupResolved { group_id, evidence } => {
            if group_id.as_ref() != group {
                return Err(HlsVodOpenError::MissingAudioRendition);
            }
            Some(evidence)
        }
    };
    let candidates = master
        .renditions
        .iter()
        .filter(|rendition| {
            rendition.rendition_type == MediaRenditionType::Audio
                && rendition.group_id.as_ref() == group
                && match intent {
                    HlsAudioLayoutIntent::Muxed => rendition.uri.is_none(),
                    HlsAudioLayoutIntent::Separate(_) => rendition.uri.is_some(),
                    HlsAudioLayoutIntent::ManifestResolved(_)
                    | HlsAudioLayoutIntent::NativeGroupResolved { .. } => true,
                }
                && evidence.is_none_or(|evidence| rendition_matches(rendition, evidence))
        })
        .collect::<Vec<_>>();
    let selected = choose_deterministic_audio(candidates)?;
    Ok(selected.uri.as_ref().map(|_| selected))
}

fn choose_deterministic_audio(
    candidates: Vec<&MediaRendition>,
) -> Result<&MediaRendition, HlsVodOpenError> {
    if candidates.len() == 1 {
        return Ok(candidates[0]);
    }
    if candidates.is_empty() {
        return Err(HlsVodOpenError::MissingAudioRendition);
    }
    Err(HlsVodOpenError::AmbiguousAudioRendition)
}

fn rendition_matches(rendition: &MediaRendition, evidence: &HlsAudioRenditionEvidence) -> bool {
    evidence
        .name
        .as_deref()
        .is_none_or(|name| rendition.name.as_ref() == name)
        && evidence
            .language
            .as_deref()
            .is_none_or(|language| rendition.language.as_deref() == Some(language))
        && evidence.channel_count.is_none_or(|channel_count| {
            rendition
                .channel_count
                .map(|rendition_count| rendition_count.get())
                == Some(u64::from(channel_count.get()))
        })
}

/// Проверяет полную video/audio cardinality, а не только наличие требуемого kind.
pub(crate) fn validate_track_shape(
    tracks: &[media_core::TrackInfo],
    expected: HlsMainTrackLayoutIntent,
    component: &'static str,
) -> Result<(), HlsTrackShapeError> {
    let video_tracks = tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Video)
        .count();
    let audio_tracks = tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .count();
    let matches = match expected {
        HlsMainTrackLayoutIntent::MuxedAv => video_tracks == 1 && audio_tracks == 1,
        HlsMainTrackLayoutIntent::VideoOnly => video_tracks == 1 && audio_tracks == 0,
        HlsMainTrackLayoutIntent::AudioOnly => video_tracks == 0 && audio_tracks == 1,
    };
    if matches {
        Ok(())
    } else {
        Err(HlsTrackShapeError {
            component,
            expected,
            video_tracks,
            audio_tracks,
        })
    }
}
