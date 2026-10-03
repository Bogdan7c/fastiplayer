//! App-owned native HLS admission и ровно один pre-admission extractor fallback boundary.

#[path = "hls/live_refresh.rs"]
mod live_refresh;

#[path = "hls/vod_catalog.rs"]
mod catalog_runtime;

use std::num::NonZeroU8;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use capability_core::SystemCapabilities;
use demux_api::DemuxRegistry;
use fastiplayer_config::{NetworkConfig, PlayerDemuxConfig, VideoCodec, WebMediaConfig};
use hls_playlist_core::HlsParserLimits;
use media_core::{Demuxer, DynamicMediaTimelinePort, TrackInfo};
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

use crate::native_web_source::hls::{NativeHlsSourceState, NativeHlsUrl};

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
pub type NativeHlsAttempt<Prepared> =
    crate::native_web_source::fallback::NativeWebMediaAttempt<Prepared>;

/// Результат settlement сохраняет фактического source owner-а.
#[cfg(test)]
pub(crate) enum NativeHlsResolution<NativePrepared, FallbackPrepared> {
    Native(NativePrepared),
    YtDlpFallback(FallbackPrepared),
}

/// Port позволяет функционально доказать fallback/cancellation policy без HTTP fixture-а.
pub trait NativeHlsAdmissionPort {
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
pub struct PreparedNativeHlsMedia {
    pub demuxer: Box<dyn Demuxer + Send>,
    pub seek_port: Arc<dyn PreparedDemuxSeekPort>,
    pub source_state: NativeHlsSourceState,
    pub lifecycle: PreparedNativeHlsLifecycle,
}

/// Не позволяет случайно смешать VOD restore/recovery с live timeline ownership.
pub enum PreparedNativeHlsLifecycle {
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
    pub fn into_web_attachments(
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
                prepared: crate::prepared_web_media::PreparedWebMediaAttachments {
                    demux_seek: Some(
                        crate::prepared_web_media::PreparedWebMediaSeekAttachment::AuthoritativePostTarget(
                            seek_port,
                        ),
                    ),
                    initial_position: Some(initial_position),
                    ..crate::prepared_web_media::PreparedWebMediaAttachments::default()
                },
                vod_endpoint_recovery: Some(endpoint_recovery),
            },
            Self::Live { timeline_port } => PreparedNativeHlsWebAttachments {
                presentation,
                prepared: crate::prepared_web_media::PreparedWebMediaAttachments {
                    timeline_port: Some(timeline_port),
                    demux_seek: Some(
                        crate::prepared_web_media::PreparedWebMediaSeekAttachment::WorkerReceipted(
                            seek_port,
                        ),
                    ),
                    ..crate::prepared_web_media::PreparedWebMediaAttachments::default()
                },
                vod_endpoint_recovery: None,
            },
        }
    }
}

/// Named composition payload не даёт потерять live timeline или прикрепить VOD recovery к live.
pub struct PreparedNativeHlsWebAttachments {
    pub presentation: web_media_core::WebMediaPresentationKind,
    pub prepared: crate::prepared_web_media::PreparedWebMediaAttachments,
    pub vod_endpoint_recovery: Option<crate::web_media_vod_recovery::VodEndpointRecoveryAttachment>,
}

impl PreparedNativeHlsMedia {
    pub fn tracks(&self) -> &[TrackInfo] {
        self.demuxer.tracks()
    }

    pub fn duration(&self) -> Option<Duration> {
        self.demuxer.duration()
    }

    /// VOD-only proof accessor не позволяет live случайно имитировать persistent restore.
    ///
    /// Только для тестов: им пользуются GPU-вертикали `app-egui`, поэтому он открыт
    /// и через feature `test-fixtures` (в рабочую сборку не попадает).
    #[cfg(any(test, feature = "test-fixtures"))]
    pub const fn vod_initial_position(&self) -> Option<PreparedInitialPosition> {
        match &self.lifecycle {
            PreparedNativeHlsLifecycle::Vod {
                initial_position, ..
            } => Some(*initial_position),
            PreparedNativeHlsLifecycle::Live { .. } => None,
        }
    }
}

/// Все production inputs одного existing-worker native admission-а.
pub struct NativeHlsPreparationRequest<'a> {
    pub source: &'a NativeHlsUrl,
    pub expected_selection: Option<&'a WebMediaSemanticSelectionRequest>,
    pub network_config: &'a NetworkConfig,
    pub web_media_config: &'a WebMediaConfig,
    pub demux_config: &'a PlayerDemuxConfig,
    pub preferred_video_codec_order: &'a [VideoCodec],
    pub system_capabilities: &'a SystemCapabilities,
    pub audio_capabilities: audio_core::AudioDecodeCapabilitySnapshot,
    pub start: HlsVodStartIntent,
    pub cancellation: CancellationToken,
}

/// Production port не создаёт thread: caller уже выполняется на media-open/startup worker-е.
pub struct ProductionNativeHlsAdmissionPort<'a> {
    request: Option<NativeHlsPreparationRequest<'a>>,
}

impl<'a> ProductionNativeHlsAdmissionPort<'a> {
    #[must_use]
    pub fn new(request: NativeHlsPreparationRequest<'a>) -> Self {
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
            const { NonZeroU8::new(3).expect("native HLS retry attempts") },
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
