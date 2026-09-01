//! Blocking staged DASH VOD preparation без app/player mutation.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use dash_mpd_core::{DashMpd, DashMpdError, DashMpdParseRequest, parse_dash_mpd};
use demux_api::{
    DemuxRegistry, ProgressiveAsyncSeekHandle, ProgressiveDemuxStartupError, ProgressiveDemuxer,
    ProgressiveRuntimeGeneration,
};
use media_core::Demuxer;
use source_core::HttpRequestTarget;
use thiserror::Error;
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication, AdaptiveTransportError,
};

use crate::component::DashComponentFactory;
use crate::plan::{
    DashPlanError, DashPresentationPlan, build_manifest_plan, build_serialized_plan,
};
use crate::request::{
    DashFetchedManifestInput, DashManifestInput, DashVodHttpContext, DashVodInput,
    DashVodOpenPolicy, DashVodOpenRequest,
};
use crate::transactional_av::TransactionalDashAvDemuxer;

/// Неустановленный ready DASH runtime.
pub struct DashVodOpenResult {
    /// Nonblocking player-facing wrapper с already-proven initial tracks.
    demuxer: ProgressiveDemuxer,
    /// Exact finite presentation duration.
    duration: Duration,
}

impl DashVodOpenResult {
    /// Возвращает cloneable seek control до type erasure runtime-а.
    #[must_use]
    pub fn async_seek_handle(&self) -> ProgressiveAsyncSeekHandle {
        self.demuxer
            .async_seek_handle()
            .expect("DASH runtime всегда создаётся с receipt capability")
    }

    /// Передаёт runtime staged app composition owner-у.
    #[must_use]
    pub fn into_demuxer(self) -> ProgressiveDemuxer {
        self.demuxer
    }

    /// Возвращает exact finite duration.
    #[must_use]
    pub const fn duration(&self) -> Duration {
        self.duration
    }
}

impl fmt::Debug for DashVodOpenResult {
    /// Не форматирует transport/demux internals.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DashVodOpenResult")
            .field("duration", &self.duration)
            .finish_non_exhaustive()
    }
}

/// Typed staged preparation failure.
#[derive(Debug, Error)]
pub enum DashVodOpenError {
    /// S31 fetch/generation/cancellation failure.
    #[error("DASH transport failed: {0}")]
    Transport(#[from] AdaptiveTransportError),
    /// S34A XML/schema/profile failure.
    #[error("DASH MPD parsing failed: {0}")]
    Manifest(#[from] DashMpdError),
    /// URL/selection/addressing/alignment planning failure.
    #[error("DASH planning failed: {0}")]
    Plan(#[from] DashPlanError),
    /// Existing demux runtime не смог доказать required initial readiness.
    #[error("DASH component readiness failed")]
    ComponentReadiness(#[source] anyhow::Error),
    /// Progressive worker startup failed после complete component preparation.
    #[error("DASH progressive runtime startup failed: {0}")]
    Progressive(#[from] ProgressiveDemuxStartupError),
    /// HTTP context layout не совпал с authoritative DASH input/layout.
    #[error("DASH HTTP context layout does not match presentation input")]
    ContextLayout,
    /// Fetched MPD принадлежит другой runtime generation.
    #[error("DASH fetched manifest generation does not match open generation")]
    FetchedManifestGenerationMismatch,
    /// Fetched MPD body превышает policy текущей open attempt.
    #[error("DASH fetched manifest exceeds current manifest body policy")]
    FetchedManifestExceedsPolicy,
}

/// Contexts, уже сопоставленные exact planned component roles.
enum PlannedHttpContexts {
    /// Один component.
    Single(Box<AdaptiveHttpContext>),
    /// Exact video/audio pair.
    Separate {
        /// Video context.
        video: Box<AdaptiveHttpContext>,
        /// Audio context.
        audio: Box<AdaptiveHttpContext>,
    },
}

/// Готовит static DASH runtime на media-open worker-е.
pub fn prepare_dash_vod(
    request: DashVodOpenRequest,
) -> Result<DashVodOpenResult, DashVodOpenError> {
    let DashVodOpenRequest {
        http,
        generation,
        input,
        selection,
        demux_registry,
        policy,
    } = request;
    let (plan, contexts) = match (input, http) {
        (DashVodInput::Manifest(manifest), DashVodHttpContext::Manifest(http)) => {
            let (mpd, manifest_base) = fetch_dash_manifest(&http, generation, manifest, policy)?;
            let plan = build_manifest_plan(
                &mpd,
                &manifest_base,
                &selection,
                policy.maximum_planned_segments,
            )?;
            let contexts = match &plan {
                DashPresentationPlan::Single(_) => PlannedHttpContexts::Single(http),
                DashPresentationPlan::Separate { .. } => PlannedHttpContexts::Separate {
                    video: http.clone(),
                    audio: http,
                },
            };
            (plan, contexts)
        }
        (DashVodInput::FetchedManifest(manifest), DashVodHttpContext::Manifest(http)) => {
            let (mpd, manifest_base) =
                parse_fetched_dash_manifest(&http, generation, manifest, policy)?;
            let plan = build_manifest_plan(
                &mpd,
                &manifest_base,
                &selection,
                policy.maximum_planned_segments,
            )?;
            let contexts = match &plan {
                DashPresentationPlan::Single(_) => PlannedHttpContexts::Single(http),
                DashPresentationPlan::Separate { .. } => PlannedHttpContexts::Separate {
                    video: http.clone(),
                    audio: http,
                },
            };
            (plan, contexts)
        }
        (DashVodInput::Serialized(serialized), DashVodHttpContext::SerializedSingle(http)) => {
            ensure_context_ready(&http, generation)?;
            let plan =
                build_serialized_plan(&serialized, &selection, policy.maximum_planned_segments)?;
            if !matches!(plan, DashPresentationPlan::Single(_)) {
                return Err(DashVodOpenError::ContextLayout);
            }
            (plan, PlannedHttpContexts::Single(http))
        }
        (
            DashVodInput::Serialized(serialized),
            DashVodHttpContext::SerializedSeparate { video, audio },
        ) => {
            ensure_context_ready(&video, generation)?;
            ensure_context_ready(&audio, generation)?;
            let plan =
                build_serialized_plan(&serialized, &selection, policy.maximum_planned_segments)?;
            if !matches!(plan, DashPresentationPlan::Separate { .. }) {
                return Err(DashVodOpenError::ContextLayout);
            }
            (plan, PlannedHttpContexts::Separate { video, audio })
        }
        _ => return Err(DashVodOpenError::ContextLayout),
    };
    prepare_planned_dash_vod(plan, contexts, generation, demux_registry, policy)
}

pub(crate) fn fetch_dash_manifest(
    http: &AdaptiveHttpContext,
    generation: web_media_transport_api::SourceGeneration,
    manifest: DashManifestInput,
    policy: DashVodOpenPolicy,
) -> Result<(DashMpd, HttpRequestTarget), DashVodOpenError> {
    ensure_context_ready(http, generation)?;
    let fetched = http.fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
        generation,
        manifest.target,
        policy.maximum_manifest_bytes,
        AdaptiveResourcePurpose::Manifest,
        AdaptiveResourceQueryApplication::ApplyScopedReplacement,
    ))?;
    let mpd = parse_dash_mpd(DashMpdParseRequest {
        document_bytes: fetched.bytes(),
        xml_budgets: manifest.xml_budgets,
        limits: manifest.mpd_limits,
    })?;
    Ok((mpd, fetched.final_target().clone()))
}

/// Парсит уже fetched MPD, сохраняя generation/body-policy boundary текущей попытки.
pub(crate) fn parse_fetched_dash_manifest(
    http: &AdaptiveHttpContext,
    generation: web_media_transport_api::SourceGeneration,
    manifest: DashFetchedManifestInput,
    policy: DashVodOpenPolicy,
) -> Result<(DashMpd, HttpRequestTarget), DashVodOpenError> {
    ensure_context_ready(http, generation)?;
    if manifest.source_generation() != generation {
        return Err(DashVodOpenError::FetchedManifestGenerationMismatch);
    }
    let (effective_target, document_bytes, xml_budgets, mpd_limits) = manifest.into_parse_parts();
    if document_bytes.len() > policy.maximum_manifest_bytes.get() {
        return Err(DashVodOpenError::FetchedManifestExceedsPolicy);
    }
    let mpd = parse_dash_mpd(DashMpdParseRequest {
        document_bytes: &document_bytes,
        xml_budgets,
        limits: mpd_limits,
    })?;
    Ok((mpd, effective_target))
}

pub(crate) fn prepare_planned_manifest_vod(
    plan: DashPresentationPlan,
    http: AdaptiveHttpContext,
    generation: web_media_transport_api::SourceGeneration,
    demux_registry: Arc<DemuxRegistry>,
    policy: DashVodOpenPolicy,
) -> Result<DashVodOpenResult, DashVodOpenError> {
    let contexts = match &plan {
        DashPresentationPlan::Single(_) => PlannedHttpContexts::Single(Box::new(http)),
        DashPresentationPlan::Separate { .. } => PlannedHttpContexts::Separate {
            video: Box::new(http.clone()),
            audio: Box::new(http),
        },
    };
    prepare_planned_dash_vod(plan, contexts, generation, demux_registry, policy)
}

fn prepare_planned_dash_vod(
    plan: DashPresentationPlan,
    contexts: PlannedHttpContexts,
    generation: web_media_transport_api::SourceGeneration,
    demux_registry: Arc<DemuxRegistry>,
    policy: DashVodOpenPolicy,
) -> Result<DashVodOpenResult, DashVodOpenError> {
    let (inner, duration, cancellation): (
        Box<dyn Demuxer + Send>,
        Duration,
        source_core::CancellationToken,
    ) = match (plan, contexts) {
        (DashPresentationPlan::Single(component), PlannedHttpContexts::Single(http)) => {
            let duration = component.duration;
            let cancellation = http.cancellation().clone();
            let factory =
                DashComponentFactory::new(component, *http, generation, policy, demux_registry);
            let component = factory
                .open()
                .map_err(DashVodOpenError::ComponentReadiness)?;
            (Box::new(component), duration, cancellation)
        }
        (
            DashPresentationPlan::Separate { video, audio },
            PlannedHttpContexts::Separate {
                video: video_http,
                audio: audio_http,
            },
        ) => {
            let duration = video.duration;
            let cancellation = video_http.cancellation().clone();
            let video_factory = DashComponentFactory::new(
                video,
                *video_http,
                generation,
                policy,
                demux_registry.clone(),
            );
            let audio_factory =
                DashComponentFactory::new(audio, *audio_http, generation, policy, demux_registry);
            let video = video_factory
                .open()
                .map_err(DashVodOpenError::ComponentReadiness)?;
            let audio = audio_factory
                .open()
                .map_err(DashVodOpenError::ComponentReadiness)?;
            let composite = TransactionalDashAvDemuxer::new(
                video_factory,
                audio_factory,
                video,
                audio,
                policy.composite_lead_policy,
            )
            .map_err(DashVodOpenError::ComponentReadiness)?;
            (Box::new(composite), duration, cancellation)
        }
        _ => return Err(DashVodOpenError::ContextLayout),
    };
    let demuxer = ProgressiveDemuxer::new_receipted_seekable(
        inner,
        cancellation,
        policy.progressive_limits,
        policy.retry_hint,
        ProgressiveRuntimeGeneration::new(generation.value()),
        policy.asynchronous_seek_limits,
    )?;
    Ok(DashVodOpenResult { demuxer, duration })
}

/// Проверяет generation/cancellation каждого component-scoped context-а до I/O.
fn ensure_context_ready(
    http: &AdaptiveHttpContext,
    generation: web_media_transport_api::SourceGeneration,
) -> Result<(), DashVodOpenError> {
    if generation != http.source_generation() {
        return Err(DashVodOpenError::Transport(
            AdaptiveTransportError::StaleGeneration {
                current: http.source_generation(),
                received: generation,
            },
        ));
    }
    if http.cancellation().is_cancelled() {
        return Err(DashVodOpenError::Transport(
            AdaptiveTransportError::Cancelled,
        ));
    }
    Ok(())
}
