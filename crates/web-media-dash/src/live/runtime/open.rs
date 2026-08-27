//! Initial open, continuation assembly и endpoint remap для DASH live runtime.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dash_mpd_core::{DashMediaKind, DashMpdParseRequest, parse_dynamic_dash_mpd};
use demux_api::ProgressiveRuntimeGeneration;
use media_core::{DemuxSeekRequest, DemuxTrackListUpdate, Demuxer};
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveResourceFetchRequest, AdaptiveResourcePurpose,
    AdaptiveResourceQueryApplication,
};
use web_media_transport_api::SourceGeneration;

use super::{
    DashLiveDemuxer, DashLiveOpenError, DashLiveOpenRequest, DashLiveOpenResult,
    DashLiveSessionTimeline, DashLiveShared, DashLiveSharedState, DashLiveTimelineCoordinator,
    DashLiveTrackPublication, refresh,
};
use crate::catalog::DashLogicalRepresentationSelection;
use crate::component::DashComponentFactory;
use crate::live::{
    DashClockFetchObservation, DashLiveRefreshError, DashLiveSelection,
    build_dash_live_snapshot_with_selection, resolve_dash_live_clock,
};
use crate::plan::{
    DashComponentPlan, DashPeriodInputPlan, DashPlannedResource, DashPresentationContinuationPoint,
    DashPresentationPlan,
};
use crate::request::DashVodOpenPolicy;
use crate::selection::DashPresentationSelection;
use crate::source::DashLiveTransportProvider;
use crate::transactional_av::TransactionalDashAvDemuxer;

pub(super) fn prepare_dash_live_with_selection(
    request: DashLiveOpenRequest,
    selection: DashLiveSelection,
) -> std::result::Result<DashLiveOpenResult, DashLiveOpenError> {
    let fetch_started = Instant::now();
    let local_before_fetch = request.wall_clock.now_utc();
    let fetched = request
        .http
        .fetch_resource_blocking(AdaptiveResourceFetchRequest::full(
            request.generation,
            request.manifest.target.clone(),
            request.policy.maximum_manifest_bytes,
            AdaptiveResourcePurpose::Manifest,
            AdaptiveResourceQueryApplication::ApplyScopedReplacement,
        ))?;
    let local_after_fetch = request.wall_clock.now_utc();
    let mpd = parse_dynamic_dash_mpd(DashMpdParseRequest {
        document_bytes: fetched.bytes(),
        xml_budgets: request.manifest.xml_budgets,
        limits: request.manifest.mpd_limits,
    })?;
    let clock = resolve_dash_live_clock(
        &mpd.utc_timing,
        fetched.final_target(),
        &request.http,
        request.generation,
        Arc::clone(&request.wall_clock),
        DashClockFetchObservation {
            local_before_fetch,
            local_after_fetch,
        },
    )
    .map_err(DashLiveRefreshError::Clock)?;
    let snapshot = build_dash_live_snapshot_with_selection(
        mpd,
        fetched.final_target(),
        &selection,
        request.policy.maximum_planned_segments,
        &clock,
    )?;
    let accepted_refresh_deadline = refresh::refresh_deadline(
        fetch_started,
        snapshot.mpd.minimum_update_period_milliseconds,
    )
    .ok_or_else(|| anyhow::anyhow!("DASH initial refresh deadline overflow"))?;
    let session_timeline =
        DashLiveSessionTimeline::from_initial_snapshot(&snapshot).map_err(anyhow::Error::new)?;
    let session_availability = session_timeline
        .availability_to_session(&snapshot.availability)
        .map_err(anyhow::Error::new)?;
    let has_video = selection_has_video(&selection);
    let has_audio = selection_has_audio(&selection);
    let (coordinator, timeline_port) = DashLiveTimelineCoordinator::new(
        session_availability,
        has_video,
        has_audio,
        request.timeline_port_generation,
        request.initial_source_epoch,
    )?;
    let cancellation = request.http.cancellation().clone();
    let refresh_request = request.clone();
    let shared = Arc::new(DashLiveShared {
        state: Mutex::new(DashLiveSharedState {
            snapshot,
            http: (*request.http).clone(),
            generation: request.generation,
            manifest: request.manifest.clone(),
            revision: 1,
            accepted_refresh_deadline,
        }),
        session_timeline,
        coordinator,
        endpoint_refresh: Arc::clone(&request.endpoint_refresh),
        endpoint_refresh_lock: Mutex::new(()),
        refresh_request: refresh_request.clone(),
        refresh_selection: selection.clone(),
    });
    // Open не наследует snapshot guard: source синхронно возвращается в
    // `current_transport()` и повторно берёт тот же mutex.
    let initial_plan = {
        shared
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("DASH live snapshot mutex poisoned"))?
            .snapshot
            .plan
            .clone()
    };
    let initial_continuation_point = initial_plan
        .continuation_point()
        .map_err(anyhow::Error::new)?;
    let mut current = open_plan(
        initial_plan,
        (*request.http).clone(),
        request.generation,
        request.policy,
        Arc::clone(&request.demux_registry),
        Some(Arc::clone(&shared) as Arc<dyn DashLiveTransportProvider>),
    )?;
    // Initial resource open мог принять fresh endpoint snapshot, поэтому edge
    // читается после open, но guard освобождается до re-entrant seek replacement.
    let initial_live_edge = {
        shared
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("DASH live snapshot mutex poisoned"))?
            .snapshot
            .availability
            .live_edge
            .as_duration()
    };
    current
        .seek_with_request(DemuxSeekRequest {
            timestamp: initial_live_edge,
            mode: media_core::DemuxSeekMode::DecodePointBefore,
        })
        .context("DASH live initial edge seek failed")?;
    let initial_tracks = session_timeline
        .track_list_update_to_session(DemuxTrackListUpdate::new(
            current.tracks().to_vec(),
            current.duration(),
        ))
        .tracks;
    let fatal = Arc::new(Mutex::new(None));
    let refresh_shared = Arc::clone(&shared);
    let refresh_fatal = Arc::clone(&fatal);
    let inner: Box<dyn Demuxer + Send> = Box::new(DashLiveDemuxer {
        current,
        continuation_point: initial_continuation_point,
        published_tracks: DashLiveTrackPublication::new(initial_tracks),
        pending_track_update: None,
        shared,
        observed_revision: 1,
        last_packet_end: None,
        fatal,
        policy: request.policy,
        registry: request.demux_registry,
    });
    let demuxer = demux_api::ProgressiveDemuxer::new_receipted_seekable(
        inner,
        cancellation,
        request.policy.progressive_limits,
        request.policy.retry_hint,
        ProgressiveRuntimeGeneration::new(request.generation.value()),
        request.policy.asynchronous_seek_limits,
    )?;
    refresh::spawn_refresh_worker(refresh_request, selection, refresh_shared, refresh_fatal)?;
    Ok(DashLiveOpenResult {
        demuxer,
        timeline_port,
    })
}

/// Открывает immutable selected plan теми же S34 component factories.
fn open_plan(
    plan: DashPresentationPlan,
    http: AdaptiveHttpContext,
    generation: SourceGeneration,
    policy: DashVodOpenPolicy,
    registry: Arc<demux_api::DemuxRegistry>,
    live_transport: Option<Arc<dyn DashLiveTransportProvider>>,
) -> Result<Box<dyn Demuxer + Send>> {
    match plan {
        DashPresentationPlan::Single(component) => {
            let factory = DashComponentFactory::new_live(
                component,
                http,
                generation,
                policy,
                registry,
                live_transport.context("DASH live transport provider отсутствует")?,
            );
            Ok(Box::new(factory.open()?))
        }
        DashPresentationPlan::Separate { video, audio } => {
            let live_transport =
                live_transport.context("DASH live transport provider отсутствует")?;
            let video_factory = DashComponentFactory::new_live(
                video,
                http.clone(),
                generation,
                policy,
                Arc::clone(&registry),
                Arc::clone(&live_transport),
            );
            let audio_factory = DashComponentFactory::new_live(
                audio,
                http,
                generation,
                policy,
                registry,
                live_transport,
            );
            let video = video_factory.open()?;
            let audio = audio_factory.open()?;
            Ok(Box::new(TransactionalDashAvDemuxer::new(
                video_factory,
                audio_factory,
                video,
                audio,
                policy.composite_lead_policy,
            )?))
        }
    }
}

/// Открывает fresh plan с первого media fragment-а после consumed snapshot boundary.
///
/// В отличие от `open_plan` этот путь не ищет decode anchor и не выполняет seek:
/// decoder продолжает ту же elementary stream reference chain.
#[allow(clippy::too_many_arguments)]
pub(super) fn open_plan_continuation(
    plan: DashPresentationPlan,
    point: DashPresentationContinuationPoint,
    http: AdaptiveHttpContext,
    generation: SourceGeneration,
    policy: DashVodOpenPolicy,
    registry: Arc<demux_api::DemuxRegistry>,
    live_transport: Arc<dyn DashLiveTransportProvider>,
) -> Result<Option<Box<dyn Demuxer + Send>>> {
    match (plan, point) {
        (
            DashPresentationPlan::Single(component),
            DashPresentationContinuationPoint::Single(component_point),
        ) => {
            let factory = DashComponentFactory::new_live(
                component,
                http,
                generation,
                policy,
                registry,
                live_transport,
            );
            Ok(factory
                .open_continuation_after(component_point)?
                .map(|component| Box::new(component) as Box<dyn Demuxer + Send>))
        }
        (
            DashPresentationPlan::Separate { video, audio },
            DashPresentationContinuationPoint::Separate {
                video: video_point,
                audio: audio_point,
            },
        ) => {
            let video_factory = DashComponentFactory::new_live(
                video,
                http.clone(),
                generation,
                policy,
                Arc::clone(&registry),
                Arc::clone(&live_transport),
            );
            let audio_factory = DashComponentFactory::new_live(
                audio,
                http,
                generation,
                policy,
                registry,
                live_transport,
            );
            let Some(video) = video_factory.open_continuation_after(video_point)? else {
                return Ok(None);
            };
            let Some(audio) = audio_factory.open_continuation_after(audio_point)? else {
                return Ok(None);
            };
            Ok(Some(Box::new(TransactionalDashAvDemuxer::new(
                video_factory,
                audio_factory,
                video,
                audio,
                policy.composite_lead_policy,
            )?)))
        }
        _ => anyhow::bail!("DASH live continuation plan shape changed across refresh"),
    }
}

/// Находит fresh URL того же component/Period/resource без сравнения secret target.
pub(super) fn remap_resource(
    plan: &DashPresentationPlan,
    media_kind: DashMediaKind,
    period_timeline_start: Duration,
    failed_resource: &DashPlannedResource,
) -> Option<DashPlannedResource> {
    let component = match plan {
        DashPresentationPlan::Single(component) if component.media_kind == media_kind => component,
        DashPresentationPlan::Separate { video, .. } if media_kind == DashMediaKind::Video => video,
        DashPresentationPlan::Separate { audio, .. } if media_kind == DashMediaKind::Audio => audio,
        _ => return None,
    };
    remap_component_resource(component, period_timeline_start, failed_resource)
}

/// Resource identity включает timeline/role/range, но намеренно исключает endpoint.
fn remap_component_resource(
    component: &DashComponentPlan,
    period_timeline_start: Duration,
    failed_resource: &DashPlannedResource,
) -> Option<DashPlannedResource> {
    let period = component
        .periods
        .iter()
        .find(|period| period.timeline_start == period_timeline_start)?;
    let DashPeriodInputPlan::Ordered { resources, .. } = &period.input else {
        return None;
    };
    let mut matches = resources.iter().filter(|candidate| {
        candidate.kind == failed_resource.kind
            && candidate.byte_range == failed_resource.byte_range
            && candidate.timeline_start == failed_resource.timeline_start
            && candidate.duration == failed_resource.duration
    });
    let replacement = matches.next()?.clone();
    matches.next().is_none().then_some(replacement)
}

fn selection_has_video(selection: &DashLiveSelection) -> bool {
    match selection {
        DashLiveSelection::Evidence(DashPresentationSelection::Single { main }) => {
            matches!(main.media_kind, DashMediaKind::Video | DashMediaKind::Muxed)
        }
        DashLiveSelection::Evidence(DashPresentationSelection::Separate { .. }) => true,
        DashLiveSelection::Logical(selection) => match selection.as_ref() {
            DashLogicalRepresentationSelection::Single(lane) => matches!(
                lane.contract.kind,
                DashMediaKind::Video | DashMediaKind::Muxed
            ),
            DashLogicalRepresentationSelection::Separate { .. } => true,
        },
    }
}

fn selection_has_audio(selection: &DashLiveSelection) -> bool {
    match selection {
        DashLiveSelection::Evidence(DashPresentationSelection::Single { main }) => {
            matches!(main.media_kind, DashMediaKind::Audio | DashMediaKind::Muxed)
        }
        DashLiveSelection::Evidence(DashPresentationSelection::Separate { .. }) => true,
        DashLiveSelection::Logical(selection) => match selection.as_ref() {
            DashLogicalRepresentationSelection::Single(lane) => matches!(
                lane.contract.kind,
                DashMediaKind::Audio | DashMediaKind::Muxed
            ),
            DashLogicalRepresentationSelection::Separate { .. } => true,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dash_mpd_core::{DashContainer, DashMediaKind};
    use source_core::HttpRequestTarget;
    use web_media_adaptive::AdaptiveResourceQueryApplication;

    use super::{DashPresentationPlan, remap_resource};
    use crate::plan::{
        DashComponentPeriodPlan, DashComponentPlan, DashPeriodInputPlan, DashPeriodLifecycle,
        DashPlannedResource,
    };
    use crate::request::DashSerializedFragmentKind;

    /// Создаёт один media resource без отражения URL в assertion diagnostics.
    fn resource(target: &str) -> DashPlannedResource {
        DashPlannedResource {
            kind: DashSerializedFragmentKind::Media,
            target: HttpRequestTarget::parse_exact(target).expect("valid test target"),
            byte_range: None,
            timeline_start: Some(Duration::from_secs(4)),
            duration: Some(Duration::from_secs(2)),
        }
    }

    /// Формирует minimal strict ordered component plan.
    fn plan(resource: DashPlannedResource) -> DashPresentationPlan {
        DashPresentationPlan::Single(DashComponentPlan {
            media_kind: DashMediaKind::Video,
            periods: vec![DashComponentPeriodPlan {
                container: DashContainer::IsoBmff,
                timeline_start: Duration::from_secs(10),
                declared_lifecycle: DashPeriodLifecycle::Finite(Duration::from_secs(20)),
                duration: Duration::from_secs(20),
                timestamp_mapping: crate::plan::DashTimestampMapping::MediaTimeOrigin(
                    Duration::ZERO,
                ),
                input: DashPeriodInputPlan::Ordered {
                    resources: vec![resource],
                    query_application: AdaptiveResourceQueryApplication::MergeScopedAddition,
                },
            }],
            duration: Duration::from_secs(20),
        })
    }

    #[test]
    fn endpoint_remap_uses_component_period_and_timeline_identity_not_old_target() {
        let failed = resource("https://old.example.test/video/segment.m4s?token=old");
        let fresh = resource("https://fresh.example.test/new-path/segment.m4s?token=fresh");
        let replacement = remap_resource(
            &plan(fresh.clone()),
            DashMediaKind::Video,
            Duration::from_secs(10),
            &failed,
        )
        .expect("same semantic resource is remapped");

        assert!(replacement == fresh);
        assert!(
            remap_resource(
                &plan(resource("https://fresh.example.test/segment.m4s")),
                DashMediaKind::Video,
                Duration::from_secs(11),
                &failed,
            )
            .is_none()
        );
    }
}
