//! App-owned native HLS admission и ровно один pre-admission extractor fallback boundary.

#[path = "native_hls/live_refresh.rs"]
mod live_refresh;

#[path = "native_hls/vod_catalog.rs"]
mod catalog_runtime;

use std::num::NonZeroU8;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use capability_core::SystemCapabilities;
use demux_api::DemuxRegistry;
use fastiplayer_config::{NetworkConfig, PlayerDemuxConfig, VideoCodec, WebMediaConfig};
use hls_playlist_core::HlsParserLimits;
use media_core::{Demuxer, DynamicMediaTimelinePort, MediaTime, TrackInfo};
use player_core::{PreparedDemuxSeekPort, PreparedInitialPosition};
use source_core::{CancellationToken, HttpPathScope, HttpRequestTarget, SourceRuntimeConfig};
use symphonia_demux::DemuxerOptions;
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveRetryPolicy, AdaptiveTransportError,
    AdaptiveTransportLimits,
};
use web_media_core::{
    CandidateFormatIdentity, CandidateIdentity, CodecFamily, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogIdentity, ExactSelectionIdentity, ExtractionGeneration,
    SemanticIdentity, WebMediaFallbackTrigger, WebMediaSelection, WebMediaSelectionRematchSource,
    WebMediaSelectionShape, WebMediaSemanticSelectionRequest,
};
use web_media_hls::{
    HlsCatalogDiscoveryOutcome, HlsFetchedTopManifest, HlsManifestInput, HlsRequestOverrides,
    HlsVodOpenRequest, HlsVodStartIntent, NativeHlsAdmissionError, NativeHlsSelectionPolicy,
    admit_native_hls_catalog,
};
use web_media_transport_api::{
    MediaComponentIdentity, MediaComponentRole, MediaPresentation, RedirectHopLimit,
    RedirectPolicy, SecretRequestContext, SecretRequestScope, SourceGeneration,
    TransportOpenRequest, TransportProviderId,
};

use crate::app_wake::{
    AppWakePort, CompletionPublishError, OwnerMailboxReceiver, WakeDelivery, owner_mailbox,
};
use crate::media_open::{NativeHlsSourceState, NativeHlsUrl};
use crate::process_shutdown::{FinishedThreadJoin, join_finished_thread};
use crate::startup_media::orchestration::{PreparedStartupMedia, StartupMediaTarget};

/// Bounded redirect policy raw public HLS URL-а без secret forwarding.
const NATIVE_HLS_REDIRECT_HOPS: u8 = 4;

/// Один exact top identity одновременно строит HTTP request и HLS reopen identity.
struct NativeTopManifestFetchIntent {
    selected_url: source_core::HttpRequestTarget,
}

impl NativeTopManifestFetchIntent {
    fn new(selected_url: source_core::HttpRequestTarget) -> Self {
        Self { selected_url }
    }

    fn request(
        &self,
        generation: SourceGeneration,
        maximum_manifest_bytes: std::num::NonZeroUsize,
    ) -> AdaptiveResourceFetchRequest {
        AdaptiveResourceFetchRequest::full(
            generation,
            self.selected_url.clone(),
            maximum_manifest_bytes,
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::BypassScopedQuery,
        )
    }

    fn into_manifest(
        self,
        fetched: web_media_adaptive::AdaptiveFetchedResource,
        http: &AdaptiveHttpContext,
    ) -> HlsManifestInput {
        HlsManifestInput::FetchedTop(HlsFetchedTopManifest::new(self.selected_url, fetched, http))
    }
}

/// HLS alias общего cross-protocol native admission результата.
pub(crate) type NativeHlsAttempt<Prepared> =
    crate::media_open::native_fallback::NativeWebMediaAttempt<Prepared>;

/// Результат settlement сохраняет фактического source owner-а.
#[cfg(test)]
pub(crate) enum NativeHlsResolution<NativePrepared, FallbackPrepared> {
    Native(NativePrepared),
    YtDlpFallback(FallbackPrepared),
}

/// Port позволяет функционально доказать fallback/cancellation policy без HTTP fixture-а.
pub(crate) trait NativeHlsAdmissionPort {
    type Prepared;
    type Error;

    fn prepare(&mut self) -> std::result::Result<NativeHlsAttempt<Self::Prepared>, Self::Error>;
}

/// Ровно один раз вызывает fallback только для typed neutral trigger-а.
#[cfg(test)]
pub(crate) fn resolve_native_hls_with_fallback<Port, Fallback, FallbackPrepared, FallbackError>(
    port: &mut Port,
    fallback: Fallback,
) -> std::result::Result<
    NativeHlsResolution<Port::Prepared, FallbackPrepared>,
    NativeHlsResolutionError<Port::Error, FallbackError>,
>
where
    Port: NativeHlsAdmissionPort,
    Fallback: FnOnce(
        web_media_core::ExtractorInvocationReason,
    ) -> std::result::Result<FallbackPrepared, FallbackError>,
{
    match port.prepare().map_err(NativeHlsResolutionError::Native)? {
        NativeHlsAttempt::Prepared(prepared) => Ok(NativeHlsResolution::Native(prepared)),
        NativeHlsAttempt::RequiresExtractorFallback(trigger) => {
            let mut gate = web_media_core::WebMediaFallbackGate::before_installed();
            let web_media_core::WebMediaFallbackOutcome::InvokeExtractor(reason) =
                gate.decide(trigger)
            else {
                return Err(NativeHlsResolutionError::PolicyRejected);
            };
            tracing::info!(
                kind = "native_hls_fallback",
                ?trigger,
                ?reason,
                "Native HLS admission передаёт source единственному extractor fallback"
            );
            fallback(reason)
                .map(NativeHlsResolution::YtDlpFallback)
                .map_err(NativeHlsResolutionError::Fallback)
        }
    }
}

/// Native и fallback failures не смешиваются в bool/string sentinel.
#[cfg(test)]
#[derive(Debug, thiserror::Error)]
pub(crate) enum NativeHlsResolutionError<NativeError, FallbackError> {
    #[error("native HLS admission failed: {0}")]
    Native(NativeError),
    #[error("native HLS extractor fallback failed: {0}")]
    Fallback(FallbackError),
    #[error("native HLS extractor fallback отклонён общим policy gate-ом")]
    PolicyRejected,
}

/// Успешный native runtime до player `PreparedMedia` boundary.
pub(crate) struct PreparedNativeHlsMedia {
    pub(crate) demuxer: Box<dyn Demuxer + Send>,
    pub(crate) seek_port: Arc<dyn PreparedDemuxSeekPort>,
    pub(crate) source_state: NativeHlsSourceState,
    pub(crate) lifecycle: PreparedNativeHlsLifecycle,
}

/// Не позволяет случайно смешать VOD restore/recovery с live timeline ownership.
pub(crate) enum PreparedNativeHlsLifecycle {
    Vod {
        initial_position: PreparedInitialPosition,
        endpoint_recovery: crate::web_media_vod_recovery::VodEndpointRecoveryAttachment,
    },
    Live {
        timeline_port: DynamicMediaTimelinePort,
    },
}

impl PreparedNativeHlsLifecycle {
    /// Возвращает exact provider-neutral lifecycle kind для durable source envelope-а.
    pub(crate) const fn presentation(&self) -> web_media_core::WebMediaPresentationKind {
        match self {
            Self::Vod { .. } => web_media_core::WebMediaPresentationKind::Vod,
            Self::Live { .. } => web_media_core::WebMediaPresentationKind::Live,
        }
    }

    /// Преобразует lifecycle в mutually-compatible pre-barrier attachments одного web open-а.
    pub(crate) fn into_web_attachments(
        self,
        seek_port: Arc<dyn PreparedDemuxSeekPort>,
    ) -> PreparedNativeHlsWebAttachments {
        let presentation = self.presentation();
        match self {
            Self::Vod {
                initial_position,
                endpoint_recovery,
            } => PreparedNativeHlsWebAttachments {
                presentation,
                prepared: crate::media_open::PreparedWebMediaAttachments {
                    demux_seek: Some(
                        crate::media_open::PreparedWebMediaSeekAttachment::AuthoritativePostTarget(
                            seek_port,
                        ),
                    ),
                    initial_position: Some(initial_position),
                    ..crate::media_open::PreparedWebMediaAttachments::default()
                },
                vod_endpoint_recovery: Some(endpoint_recovery),
            },
            Self::Live { timeline_port } => PreparedNativeHlsWebAttachments {
                presentation,
                prepared: crate::media_open::PreparedWebMediaAttachments {
                    timeline_port: Some(timeline_port),
                    demux_seek: Some(
                        crate::media_open::PreparedWebMediaSeekAttachment::WorkerReceipted(
                            seek_port,
                        ),
                    ),
                    ..crate::media_open::PreparedWebMediaAttachments::default()
                },
                vod_endpoint_recovery: None,
            },
        }
    }
}

/// Named composition payload не даёт потерять live timeline или прикрепить VOD recovery к live.
pub(crate) struct PreparedNativeHlsWebAttachments {
    pub(crate) presentation: web_media_core::WebMediaPresentationKind,
    pub(crate) prepared: crate::media_open::PreparedWebMediaAttachments,
    pub(crate) vod_endpoint_recovery:
        Option<crate::web_media_vod_recovery::VodEndpointRecoveryAttachment>,
}

impl PreparedNativeHlsMedia {
    pub(crate) fn tracks(&self) -> &[TrackInfo] {
        self.demuxer.tracks()
    }

    pub(crate) fn duration(&self) -> Option<Duration> {
        self.demuxer.duration()
    }

    /// VOD-only proof accessor не позволяет live случайно имитировать persistent restore.
    #[cfg(test)]
    pub(crate) const fn vod_initial_position(&self) -> Option<PreparedInitialPosition> {
        match &self.lifecycle {
            PreparedNativeHlsLifecycle::Vod {
                initial_position, ..
            } => Some(*initial_position),
            PreparedNativeHlsLifecycle::Live { .. } => None,
        }
    }
}

/// Все production inputs одного existing-worker native admission-а.
pub(crate) struct NativeHlsPreparationRequest<'a> {
    pub(crate) source: &'a NativeHlsUrl,
    pub(crate) expected_selection: Option<&'a WebMediaSemanticSelectionRequest>,
    pub(crate) network_config: &'a NetworkConfig,
    pub(crate) web_media_config: &'a WebMediaConfig,
    pub(crate) demux_config: &'a PlayerDemuxConfig,
    pub(crate) preferred_video_codec_order: &'a [VideoCodec],
    pub(crate) system_capabilities: &'a SystemCapabilities,
    pub(crate) audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    pub(crate) start: HlsVodStartIntent,
    pub(crate) cancellation: CancellationToken,
}

/// Production port не создаёт thread: caller уже выполняется на media-open/startup worker-е.
pub(crate) struct ProductionNativeHlsAdmissionPort<'a> {
    request: Option<NativeHlsPreparationRequest<'a>>,
}

impl<'a> ProductionNativeHlsAdmissionPort<'a> {
    #[must_use]
    pub(crate) fn new(request: NativeHlsPreparationRequest<'a>) -> Self {
        Self {
            request: Some(request),
        }
    }
}

impl NativeHlsAdmissionPort for ProductionNativeHlsAdmissionPort<'_> {
    type Prepared = PreparedNativeHlsMedia;
    type Error = anyhow::Error;

    fn prepare(&mut self) -> Result<NativeHlsAttempt<Self::Prepared>> {
        let request = self
            .request
            .take()
            .ok_or_else(|| anyhow!("native HLS admission port already consumed"))?;
        catalog_runtime::prepare_native_hls_attempt(request)
    }
}

fn native_transport_request(
    parent: &ExactSelectionIdentity,
    source: &NativeHlsUrl,
    presentation: MediaPresentation,
    generation: SourceGeneration,
    cancellation: CancellationToken,
) -> Result<TransportOpenRequest> {
    let component = MediaComponentIdentity::new(
        parent.exact().clone(),
        parent.semantic().clone(),
        MediaComponentRole::PresentationManifest,
    )?;
    let initial_target = source.target().clone();
    // Adaptive HTTP требует реальный scope proof даже у пустого public
    // secret context-а: `SecretRequestContext::empty()` намеренно существует
    // только для non-HTTP transport-ов и привязан к invalid placeholder origin.
    let public_request_context = native_public_request_context(&initial_target);
    Ok(TransportOpenRequest::new(
        TransportProviderId::new("native-hls-http")?,
        component,
        initial_target,
        presentation,
        generation,
        public_request_context,
        RedirectPolicy::cross_origin_without_secrets(RedirectHopLimit::new(
            NATIVE_HLS_REDIRECT_HOPS,
        )?),
        cancellation,
    )?)
}

/// Собирает единую native HLS HTTP policy для initial open и stable-root endpoint refresh.
fn native_adaptive_http_context(
    transport_request: TransportOpenRequest,
    network_config: &NetworkConfig,
    adaptive_limits: AdaptiveTransportLimits,
) -> Result<AdaptiveHttpContext> {
    let source_config = SourceRuntimeConfig::from_network_config(network_config)
        .context("native HLS source config")?;
    AdaptiveHttpContext::new(
        transport_request,
        &source_config,
        adaptive_limits,
        AdaptiveRetryPolicy::new(
            NonZeroU8::new(3).expect("native HLS retry attempts"),
            Duration::from_millis(100),
            Duration::from_secs(2),
            crate::web_media_adaptive_config::maximum_adaptive_retry_after(),
        )?,
    )
    .map_err(anyhow::Error::new)
}

/// Строит пустой по данным, но корректно scoped HTTP context для public HLS.
fn native_public_request_context(initial_target: &HttpRequestTarget) -> SecretRequestContext {
    let path_scope = HttpPathScope::from_target_path(initial_target);
    SecretRequestContext::builder(SecretRequestScope::from_target(initial_target, path_scope))
        .build()
}

fn native_hls_demux_registry(
    demux_config: &PlayerDemuxConfig,
    maximum_segment_bytes: std::num::NonZeroUsize,
) -> Result<Arc<DemuxRegistry>> {
    let options = DemuxerOptions::from_max_consecutive_corrupted_packets(
        demux_config.max_consecutive_corrupted_packets,
    )
    .context("native HLS demux corruption limit must be non-zero")?;
    let mpeg_ts_options = mpeg_ts_demux::MpegTsDemuxOptions::default()
        .with_initial_probe_byte_budget(maximum_segment_bytes);
    let composition =
        crate::web_media_demux_registry::WebDemuxComposition::new_hls(options, mpeg_ts_options)?;
    Ok(Arc::new(composition.registry))
}

const fn native_codec_family(codec: VideoCodec) -> CodecFamily {
    match codec {
        VideoCodec::Vp9 => CodecFamily::Vp9,
        VideoCodec::Av1 => CodecFamily::Av1,
        VideoCodec::H264 => CodecFamily::H264,
        VideoCodec::H265 => CodecFamily::H265,
        VideoCodec::Vp8 => CodecFamily::Vp8,
    }
}

/// Результат одного последовательного native-admission/extractor-fallback job-а.
type NativeHlsStartupResult = std::result::Result<PreparedStartupMedia, String>;

/// Caller-owned identity и start policy одного native HLS startup resolve.
struct NativeHlsStartupResolveRequest {
    source: NativeHlsUrl,
    fallback_locator: service_ytdlp::YtDlpMediaLocator,
    start: HlsVodStartIntent,
}

/// Фоновый CLI job выполняет native admission и только затем возможный extractor fallback.
pub(super) struct NativeHlsStartupJob {
    pending_message: String,
    result_receiver: OwnerMailboxReceiver<(), NativeHlsStartupResult>,
    pub(super) join_handle: Option<JoinHandle<()>>,
    pending_result: Option<NativeHlsStartupResult>,
    pub(super) cancellation_requested: Arc<AtomicBool>,
    pub(super) source_cancellation: source_core::CancellationToken,
}

impl NativeHlsStartupJob {
    pub(super) fn spawn(
        source: NativeHlsUrl,
        fallback_locator: service_ytdlp::YtDlpMediaLocator,
        start: HlsVodStartIntent,
        app_config: fastiplayer_config::AppConfig,
        system_capabilities: SystemCapabilities,
        audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
        wake_port: AppWakePort,
    ) -> std::result::Result<Self, String> {
        let (result_publisher, result_receiver) = owner_mailbox(wake_port);
        let cancellation_requested = Arc::new(AtomicBool::new(false));
        let worker_cancellation_requested = Arc::clone(&cancellation_requested);
        let source_cancellation = source_core::CancellationToken::new();
        let worker_source_cancellation = source_cancellation.clone();
        let join_handle = thread::Builder::new()
            .name("native-hls-startup-opener".to_string())
            .spawn(move || {
                let result = resolve_native_hls_startup_media(
                    NativeHlsStartupResolveRequest {
                        source,
                        fallback_locator,
                        start,
                    },
                    &app_config,
                    &system_capabilities,
                    audio_capabilities,
                    worker_source_cancellation,
                    || worker_cancellation_requested.load(Ordering::Acquire),
                )
                .map_err(|error| format!("{error:#}"));
                if worker_cancellation_requested.load(Ordering::Acquire) {
                    return;
                }
                match result_publisher.publish_completion(result) {
                    Ok(WakeDelivery::EventLoopClosed) => tracing::debug!(
                        "Event loop закрыт; native HLS terminal оставлен без wake retry"
                    ),
                    Ok(WakeDelivery::Armed | WakeDelivery::Coalesced) => {}
                    Err(CompletionPublishError::AlreadyPublished) => tracing::warn!(
                        "Native HLS startup opener попытался опубликовать второй terminal"
                    ),
                }
            })
            .map_err(|error| format!("Не удалось запустить native HLS startup opener: {error}"))?;
        Ok(Self {
            pending_message: "Проверка native HLS...".to_owned(),
            result_receiver,
            join_handle: Some(join_handle),
            pending_result: None,
            cancellation_requested,
            source_cancellation,
        })
    }

    pub(super) fn pending_message(&self) -> &str {
        &self.pending_message
    }

    pub(super) fn try_take_result(&mut self) -> Option<NativeHlsStartupResult> {
        let drain = self.result_receiver.drain();
        if drain.completion.is_some() {
            self.pending_result = drain.completion;
        }
        match join_finished_thread(&mut self.join_handle) {
            FinishedThreadJoin::Joined | FinishedThreadJoin::AlreadyJoined => {
                self.pending_result.take().or_else(|| {
                    drain.producer_disconnected_without_completion.then(|| {
                        Err("Native HLS startup opener завершился без результата".to_owned())
                    })
                })
            }
            FinishedThreadJoin::Panicked => {
                self.pending_result = None;
                Some(Err("Native HLS startup opener завершился panic".to_owned()))
            }
            FinishedThreadJoin::StillRunning => None,
        }
    }
}

impl super::StartupMediaController {
    /// Запускает один sequential CLI native admission job без параллельного extractor probe-а.
    pub(crate) fn start_native_hls_startup_job(
        &mut self,
        source: NativeHlsUrl,
        fallback_locator: service_ytdlp::YtDlpMediaLocator,
        app_state: &mut crate::state::AppState,
        app_config: &fastiplayer_config::AppConfig,
        system_capabilities: &SystemCapabilities,
    ) {
        if let Some(error) = self.startup_job_admission_error() {
            self.orchestration.preparation_failed();
            self.startup_error = Some(error.clone());
            app_state.set_startup_error(error);
            return;
        }
        app_state.set_startup_pending("Проверка native HLS...".to_owned());
        let start = match self.orchestration.target.as_ref() {
            Some(StartupMediaTarget::RestoredCurrent(target)) => match target.position() {
                crate::playlist_runtime::StartupPosition::KeepStart => HlsVodStartIntent::Beginning,
                crate::playlist_runtime::StartupPosition::Restore(position) => {
                    HlsVodStartIntent::RestoreOrBeginning(MediaTime::from_duration(position))
                }
            },
            Some(StartupMediaTarget::CliReplacement) | None => HlsVodStartIntent::Beginning,
        };
        match NativeHlsStartupJob::spawn(
            source,
            fallback_locator,
            start,
            app_config.clone(),
            system_capabilities.clone(),
            app_state.audio_decode_capability_snapshot(),
            self.wake_port.clone(),
        ) {
            Ok(job) => {
                self.startup_error = None;
                self.native_hls_startup_job = Some(job);
            }
            Err(error) => {
                self.orchestration.preparation_failed();
                tracing::warn!(error = %error, "Не удалось запустить native HLS startup opener");
                self.startup_error = Some(error.clone());
                app_state.set_startup_error(error);
            }
        }
    }
}

/// Выполняет native HLS admission и ровно один fallback внутри одного startup worker-а.
fn resolve_native_hls_startup_media(
    request: NativeHlsStartupResolveRequest,
    app_config: &fastiplayer_config::AppConfig,
    system_capabilities: &SystemCapabilities,
    audio_capabilities: audio::AudioDecodeCapabilitySnapshot,
    cancellation: source_core::CancellationToken,
    is_cancelled: impl Fn() -> bool,
) -> Result<PreparedStartupMedia> {
    let NativeHlsStartupResolveRequest {
        source,
        fallback_locator,
        start,
    } = request;
    let mut port = ProductionNativeHlsAdmissionPort::new(NativeHlsPreparationRequest {
        source: &source,
        expected_selection: None,
        network_config: &app_config.network,
        web_media_config: &app_config.web_media,
        demux_config: &app_config.player.demux,
        preferred_video_codec_order: &app_config.player.preferred_video_codec_order,
        system_capabilities,
        audio_capabilities,
        start,
        cancellation: cancellation.clone(),
    });
    match NativeHlsAdmissionPort::prepare(&mut port)? {
        NativeHlsAttempt::Prepared(prepared) => Ok(PreparedStartupMedia::NativeHls {
            source,
            prepared: Box::new(prepared),
        }),
        NativeHlsAttempt::RequiresExtractorFallback(trigger) => {
            let mut fallback_owner =
                crate::media_open::native_fallback::NativeWebFallbackOwner::before_installed(
                    fallback_locator,
                );
            let fallback = fallback_owner
                .claim(trigger)
                .map_err(|rejection| anyhow!("native HLS fallback rejected: {rejection:?}"))?;
            let (fallback_locator, invocation_reason) = fallback.into_parts();
            if !app_config.yt_dlp.enabled {
                return Err(anyhow!(
                    "native HLS admission requires extractor fallback ({invocation_reason:?}), но YtDlp отключён"
                ));
            }
            tracing::info!(
                ?invocation_reason,
                "CLI native HLS admission передан единственному YtDlp fallback"
            );
            let prepared = super::resolve_yt_dlp_startup_media(
                &fallback_locator,
                app_config,
                system_capabilities,
                audio_capabilities,
                invocation_reason,
                cancellation,
                is_cancelled,
            )?;
            Ok(PreparedStartupMedia::Extractor {
                source_locator: fallback_locator,
                prepared: Box::new(prepared),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use super::*;

    struct FakeAdmissionPort {
        result: Option<std::result::Result<NativeHlsAttempt<u8>, &'static str>>,
        calls: usize,
    }

    #[test]
    fn top_fetch_intent_keeps_exact_requested_identity_for_fetched_manifest() {
        let exact = "https://media.example.test/master.m3u8?signature=keep-this-exact";
        let intent = NativeTopManifestFetchIntent::new(
            source_core::HttpRequestTarget::parse_exact(exact).expect("valid target"),
        );

        assert_eq!(intent.selected_url.expose_secret_for_request(), exact);
        let request = intent.request(
            crate::web_media_adaptive_config::initial_adaptive_source_generation(),
            std::num::NonZeroUsize::new(64 * 1024).expect("non-zero manifest bound"),
        );
        let debug = format!("{request:?}");
        assert!(!debug.contains("keep-this-exact"));
    }

    #[test]
    fn public_native_http_context_is_empty_but_scoped_to_the_real_manifest() {
        let target = source_core::HttpRequestTarget::parse_exact(
            "https://media.example.test/hls/master.m3u8",
        )
        .expect("valid target");
        let context = native_public_request_context(&target);

        assert!(context.is_empty());
        assert!(
            context
                .material_for(
                    &target,
                    web_media_transport_api::SecretRequestPurpose::Manifest,
                )
                .is_some(),
            "public context должен дать adaptive HTTP scope proof для exact top manifest",
        );
    }

    impl NativeHlsAdmissionPort for FakeAdmissionPort {
        type Prepared = u8;
        type Error = &'static str;

        fn prepare(
            &mut self,
        ) -> std::result::Result<NativeHlsAttempt<Self::Prepared>, Self::Error> {
            self.calls += 1;
            self.result.take().expect("fake admission called once")
        }
    }

    #[test]
    fn proven_native_never_calls_extractor_fallback() {
        let mut port = FakeAdmissionPort {
            result: Some(Ok(NativeHlsAttempt::Prepared(7))),
            calls: 0,
        };
        let mut fallback_calls = 0;
        let resolution = resolve_native_hls_with_fallback(&mut port, |_| {
            fallback_calls += 1;
            Ok::<_, Infallible>(9)
        })
        .expect("native resolution");
        assert!(matches!(resolution, NativeHlsResolution::Native(7)));
        assert_eq!(port.calls, 1);
        assert_eq!(fallback_calls, 0);
    }

    #[test]
    fn typed_fallback_is_called_exactly_once() {
        let mut port = FakeAdmissionPort {
            result: Some(Ok(NativeHlsAttempt::RequiresExtractorFallback(
                WebMediaFallbackTrigger::ExtractorOwnedAuthorizationMaterial,
            ))),
            calls: 0,
        };
        let mut fallback_calls = 0;
        let mut observed_reason = None;
        let resolution = resolve_native_hls_with_fallback(&mut port, |reason| {
            observed_reason = Some(reason);
            fallback_calls += 1;
            Ok::<_, Infallible>(9)
        })
        .expect("fallback resolution");
        assert!(matches!(resolution, NativeHlsResolution::YtDlpFallback(9)));
        assert_eq!(port.calls, 1);
        assert_eq!(fallback_calls, 1);
        assert_eq!(
            observed_reason,
            Some(web_media_core::ExtractorInvocationReason::ExtractorOwnedAuthorizationMaterial)
        );
    }

    #[test]
    fn fatal_native_error_never_calls_fallback() {
        let mut port = FakeAdmissionPort {
            result: Some(Err("fatal")),
            calls: 0,
        };
        let mut fallback_calls = 0;
        let result = resolve_native_hls_with_fallback(&mut port, |_| {
            fallback_calls += 1;
            Ok::<_, Infallible>(9)
        });
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("fatal native error unexpectedly resolved"),
        };
        assert!(matches!(error, NativeHlsResolutionError::Native("fatal")));
        assert_eq!(port.calls, 1);
        assert_eq!(fallback_calls, 0);
    }
}
