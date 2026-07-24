use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use hls_playlist_core::{
    HlsParseError, HlsParseRequest, HlsPlaylist, HlsProfileError, MediaContainerIntent,
    MediaPlaylist, parse_hls_playlist, validate_initial_profile, validate_live_profile,
    validate_live_refresh_profile,
};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer,
    DynamicMediaTimelinePort, MediaMetadata, TrackInfo,
};
use source_core::HttpRequestTarget;
use web_media_adaptive::{
    AdaptiveFetchedResource, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveTransportError,
};

use super::refresh::{HlsLiveRefreshControl, HlsLiveRefreshOwner, HlsLiveRuntimeFailure};
use super::{
    HlsLiveComponentFactory, HlsLiveComponentKind, HlsLiveComponentSnapshot, HlsLiveRefreshError,
    HlsLiveTimelineCoordinator, TransactionalHlsLiveAvDemuxer,
};
use crate::open::{
    HlsVodOpenError, required_audio_container, required_main_container, select_master,
    validate_key_fetch_bound,
};
use crate::plan::{HlsComponentPlan, HlsPlanError, build_segment_scoped_component_plan};
use crate::{
    HlsAudioLayoutIntent, HlsLiveOpenRequest, HlsManifestInput, HlsRequiredContainer,
    HlsSubtitleRenditionDescriptor, HlsVodOpenRequest,
};

/// Неустановленный live runtime и neutral S31L port.
pub struct HlsLiveOpenResult {
    demuxer: Box<dyn Demuxer + Send>,
    timeline_port: DynamicMediaTimelinePort,
    subtitles: Box<[HlsSubtitleRenditionDescriptor]>,
}

impl HlsLiveOpenResult {
    pub fn into_demuxer(self) -> Box<dyn Demuxer + Send> {
        self.demuxer
    }

    pub fn into_parts(
        self,
    ) -> (
        Box<dyn Demuxer + Send>,
        DynamicMediaTimelinePort,
        Box<[HlsSubtitleRenditionDescriptor]>,
    ) {
        (self.demuxer, self.timeline_port, self.subtitles)
    }

    pub fn subtitle_renditions(&self) -> &[HlsSubtitleRenditionDescriptor] {
        &self.subtitles
    }
}

impl std::fmt::Debug for HlsLiveOpenResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HlsLiveOpenResult")
            .field("tracks", &self.demuxer.tracks().len())
            .field("duration", &Option::<Duration>::None)
            .field("subtitles", &self.subtitles.len())
            .finish_non_exhaustive()
    }
}

/// Secret-safe S33 prepare/runtime error.
#[derive(Debug, thiserror::Error)]
pub enum HlsLiveOpenError {
    #[error("inline HLS manifest нельзя безопасно refresh-ить как live")]
    InlineManifestCannotRefresh,
    #[error("HLS manifest fetch failed: {0}")]
    Transport(#[from] AdaptiveTransportError),
    #[error("HLS live manifest invalid: {0}")]
    Parse(#[from] HlsParseError),
    #[error("HLS live profile rejected: {0}")]
    Profile(#[from] HlsProfileError),
    #[error("HLS live selection failed: {0}")]
    Selection(#[from] HlsVodOpenError),
    #[error("HLS live child target resolution failed: {0}")]
    Target(#[from] source_core::HttpRequestTargetError),
    #[error("HLS live plan invalid: {0}")]
    Plan(#[from] HlsPlanError),
    #[error("HLS live refresh continuity rejected")]
    RefreshContinuity,
    #[error("HLS live refresh worker не запущен: {0}")]
    RefreshWorkerSpawn(#[source] std::io::Error),
    #[error("HLS live runtime failed: {0}")]
    Runtime(#[source] anyhow::Error),
}

impl From<HlsLiveRefreshError> for HlsLiveOpenError {
    fn from(_: HlsLiveRefreshError) -> Self {
        Self::RefreshContinuity
    }
}

/// Открывает explicit live candidate; отсутствие ENDLIST само по себе сюда не маршрутизирует.
pub fn prepare_hls_live(
    request: HlsLiveOpenRequest,
) -> Result<HlsLiveOpenResult, HlsLiveOpenError> {
    validate_key_fetch_bound(&request.common)?;
    let initial = load_selected_live(&request.common, false)?;
    let shared_edge = initial.main_plan.duration.max(
        initial
            .audio_plan
            .as_ref()
            .map_or(Duration::ZERO, |plan| plan.duration),
    );
    let main_snapshot = HlsLiveComponentSnapshot::initial(
        HlsLiveComponentKind::Main,
        &initial.main_media,
        initial.main_plan.clone(),
        shared_edge,
    )?;
    let audio_snapshot = initial
        .audio_media
        .as_ref()
        .zip(initial.audio_plan.clone())
        .map(|(media, plan)| {
            HlsLiveComponentSnapshot::initial(
                HlsLiveComponentKind::AlternateAudio,
                media,
                plan,
                shared_edge,
            )
        })
        .transpose()?;
    let main_has_video = !matches!(
        request.common.selection.main_track_layout,
        crate::HlsMainTrackLayoutIntent::AudioOnly
    );
    let (coordinator, timeline_port) = HlsLiveTimelineCoordinator::new(
        main_snapshot,
        audio_snapshot,
        request.timeline_port_generation,
        request.initial_source_epoch,
        main_has_video,
        request.common.http.clone(),
        request.common.generation,
    );
    let refresh_control = HlsLiveRefreshControl::new();
    let main_factory = HlsLiveComponentFactory::new(
        HlsLiveComponentKind::Main,
        initial.main_container,
        request.common.policy,
        Arc::clone(&request.common.demux_registry),
        Arc::clone(&coordinator),
        Arc::clone(&refresh_control),
    );
    let main = main_factory.open().map_err(HlsLiveOpenError::Runtime)?;
    let demuxer: Box<dyn Demuxer + Send> =
        match (initial.audio_container, initial.audio_media.as_ref()) {
            (Some(audio_container), Some(_)) => {
                let audio_factory = HlsLiveComponentFactory::new(
                    HlsLiveComponentKind::AlternateAudio,
                    audio_container,
                    request.common.policy,
                    Arc::clone(&request.common.demux_registry),
                    Arc::clone(&coordinator),
                    Arc::clone(&refresh_control),
                );
                let audio = audio_factory.open().map_err(HlsLiveOpenError::Runtime)?;
                Box::new(
                    TransactionalHlsLiveAvDemuxer::new(
                        main_factory,
                        audio_factory,
                        main,
                        audio,
                        request.common.policy.composite_lead_policy,
                    )
                    .map_err(HlsLiveOpenError::Runtime)?,
                )
            }
            (None, None) => Box::new(main),
            _ => {
                return Err(HlsLiveOpenError::Runtime(anyhow::anyhow!(
                    "HLS live audio selection lost component pairing"
                )));
            }
        };

    let fatal = Arc::new(Mutex::new(None));
    let subtitles = initial.subtitles.clone().into_boxed_slice();
    let refresh_owner = HlsLiveRefreshOwner::spawn(
        request,
        initial,
        Arc::clone(&coordinator),
        Arc::clone(&fatal),
        refresh_control,
    )?;
    Ok(HlsLiveOpenResult {
        demuxer: Box::new(HlsLiveDemuxRuntime {
            inner: demuxer,
            _refresh_owner: refresh_owner,
            fatal,
        }),
        timeline_port,
        subtitles,
    })
}

#[derive(Clone)]
pub(super) struct SelectedLiveResources {
    pub(super) main_media: MediaPlaylist,
    pub(super) main_plan: HlsComponentPlan,
    pub(super) main_reload_target: HttpRequestTarget,
    pub(super) main_container: HlsRequiredContainer,
    pub(super) audio_media: Option<MediaPlaylist>,
    pub(super) audio_plan: Option<HlsComponentPlan>,
    pub(super) audio_reload_target: Option<HttpRequestTarget>,
    pub(super) audio_container: Option<HlsRequiredContainer>,
    pub(super) subtitles: Vec<HlsSubtitleRenditionDescriptor>,
}

pub(super) fn load_selected_live(
    request: &HlsVodOpenRequest,
    refresh_profile: bool,
) -> Result<SelectedLiveResources, HlsLiveOpenError> {
    let HlsManifestInput::Fetch { selected_url } = &request.manifest else {
        return Err(HlsLiveOpenError::InlineManifestCannotRefresh);
    };
    let top_resource = fetch_manifest(&request.http, request.generation, selected_url.clone())?;
    let top = parse_playlist(&top_resource, request)?;
    validate_initial_profile(&top)?;
    match top {
        HlsPlaylist::Media(media) => {
            if matches!(
                request.selection.audio,
                HlsAudioLayoutIntent::Separate(_) | HlsAudioLayoutIntent::ManifestResolved(_)
            ) {
                return Err(HlsVodOpenError::SeparateAudioRequiresMaster.into());
            }
            let main_container = required_main_container(request)?;
            validate_live_media(&media, main_container, refresh_profile)?;
            let plan = build_segment_scoped_component_plan(
                &media,
                main_container,
                top_resource.final_target(),
                &request.overrides,
            )?;
            plan.validate_resource_bound(
                request
                    .http
                    .maximum_resource_bytes(AdaptiveResourcePurpose::MediaSegment),
            )?;
            Ok(SelectedLiveResources {
                main_media: media,
                main_plan: plan,
                main_reload_target: selected_url.clone(),
                main_container,
                audio_media: None,
                audio_plan: None,
                audio_reload_target: None,
                audio_container: None,
                subtitles: Vec::new(),
            })
        }
        HlsPlaylist::Master(master) => {
            let selected = select_master(&master, &request.selection)?;
            let main_container = required_main_container(request)?;
            let main_reload_target = top_resource
                .final_target()
                .resolve_reference(selected.variant.uri.expose_for_resolution())?;
            let main_resource = fetch_manifest(
                &request.http,
                request.generation,
                main_reload_target.clone(),
            )?;
            let HlsPlaylist::Media(main_media) = parse_playlist(&main_resource, request)? else {
                return Err(HlsVodOpenError::NestedMasterPlaylist.into());
            };
            validate_live_media(&main_media, main_container, refresh_profile)?;
            let main_plan = build_segment_scoped_component_plan(
                &main_media,
                main_container,
                main_resource.final_target(),
                &request.overrides,
            )?;
            let (audio_media, audio_plan, audio_reload_target, audio_container) =
                if let Some(rendition) = selected.audio {
                    let reference = rendition
                        .uri
                        .as_ref()
                        .ok_or(HlsVodOpenError::MissingAudioRendition)?;
                    let target = top_resource
                        .final_target()
                        .resolve_reference(reference.expose_for_resolution())?;
                    let resource =
                        fetch_manifest(&request.http, request.generation, target.clone())?;
                    let HlsPlaylist::Media(media) = parse_playlist(&resource, request)? else {
                        return Err(HlsVodOpenError::NestedMasterPlaylist.into());
                    };
                    let container =
                        required_audio_container(&media, resource.final_target(), request)?;
                    validate_live_media(&media, container, refresh_profile)?;
                    let plan = build_segment_scoped_component_plan(
                        &media,
                        container,
                        resource.final_target(),
                        &request.overrides,
                    )?;
                    (Some(media), Some(plan), Some(target), Some(container))
                } else {
                    (None, None, None, None)
                };
            Ok(SelectedLiveResources {
                main_media,
                main_plan,
                main_reload_target,
                main_container,
                audio_media,
                audio_plan,
                audio_reload_target,
                audio_container,
                subtitles: selected.subtitles,
            })
        }
    }
}

pub(super) fn validate_live_media(
    media: &MediaPlaylist,
    container: HlsRequiredContainer,
    refresh_profile: bool,
) -> Result<(), HlsProfileError> {
    let playlist = HlsPlaylist::Media(media.clone());
    let intent = Some(match container {
        HlsRequiredContainer::TransportStream => MediaContainerIntent::TransportStream,
        HlsRequiredContainer::FragmentedMp4 => MediaContainerIntent::FragmentedMp4,
    });
    if refresh_profile {
        validate_live_refresh_profile(&playlist, intent)
    } else {
        validate_live_profile(&playlist, intent)
    }
}

pub(super) fn fetch_manifest(
    http: &web_media_adaptive::AdaptiveHttpContext,
    generation: web_media_transport_api::SourceGeneration,
    target: HttpRequestTarget,
) -> Result<AdaptiveFetchedResource, AdaptiveTransportError> {
    http.fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
        generation,
        target,
        http.maximum_resource_bytes(AdaptiveResourcePurpose::Manifest),
        AdaptiveResourcePurpose::Manifest,
        AdaptiveResourceQueryApplication::BypassScopedQuery,
    ))
}

pub(super) fn parse_playlist(
    resource: &AdaptiveFetchedResource,
    request: &HlsVodOpenRequest,
) -> Result<HlsPlaylist, HlsParseError> {
    parse_hls_playlist(HlsParseRequest {
        document_bytes: resource.bytes(),
        reference_base: Some(resource.final_target().expose_secret_for_request()),
        limits: request.policy.parser_limits,
    })
}

struct HlsLiveDemuxRuntime {
    inner: Box<dyn Demuxer + Send>,
    _refresh_owner: HlsLiveRefreshOwner,
    fatal: Arc<Mutex<Option<HlsLiveRuntimeFailure>>>,
}

impl HlsLiveDemuxRuntime {
    fn check_fatal(&self) -> Result<()> {
        let failure = self
            .fatal
            .lock()
            .map_err(|_| anyhow::anyhow!("HLS live fatal-state mutex poisoned"))?
            .as_ref()
            .copied();
        failure.map_or(Ok(()), |failure| Err(anyhow::Error::new(failure)))
    }
}

impl Demuxer for HlsLiveDemuxRuntime {
    fn tracks(&self) -> &[TrackInfo] {
        self.inner.tracks()
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.inner.media_metadata()
    }

    fn seekability(&self) -> DemuxSeekability {
        self.inner.seekability()
    }

    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        self.check_fatal()?;
        self.inner.next_event()
    }

    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.check_fatal()?;
        self.inner.seek(timestamp)
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        self.check_fatal()?;
        self.inner.seek_with_request(request)
    }
}
