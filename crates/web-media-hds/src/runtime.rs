//! Worker-owned HDS source construction and transactional VOD seek.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use demux_api::{
    DemuxContainerId, DemuxHints, DemuxInput, DemuxRegistry, OrderedSegmentKind,
    OrderedSegmentSequence, ProgressiveAsyncSeekHandle, ProgressiveDemuxer,
    ProgressiveRuntimeGeneration, ProgressiveSeekController,
};
use hds_manifest_core::HdsFragment;
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer, MediaMetadata,
    TrackInfo,
};
use source_core::SourceRuntimeConfig;
use web_media_adaptive::{
    AdaptiveHttpContext, AdaptiveOrderedSegmentSource, AdaptivePresentation,
    AdaptiveSegmentCompletion, AdaptiveSegmentDescriptor, AdaptiveSegmentSnapshot,
    BlockingOrderedSegmentAdapter, ComponentClockMetadata,
};
use web_media_core::ComponentVariantCatalog;
use web_media_transport_api::{MediaPresentation, TransportOpenRequest};

use crate::policy::HdsVodOpenPolicy;
use crate::resolve::{
    HdsRenditionSelection, ResolvedHdsRendition, fragment_target, resolve_presentation,
    select_rendition, units_to_duration,
};

/// Owned HDS open request из app composition root-а.
pub struct HdsVodOpenRequest {
    /// Secret-scoped root manifest request.
    pub transport_request: TransportOpenRequest,
    /// Cloneable source-core network configuration.
    pub source_config: SourceRuntimeConfig,
    /// Existing app-owned demux registry, где уже зарегистрирован S30 F4F.
    pub demux_registry: Arc<DemuxRegistry>,
    /// Explicit S38 budgets.
    pub policy: HdsVodOpenPolicy,
    /// Global preference или future exact UI selection.
    pub selection: HdsRenditionSelection,
}

/// Абсолютные source clock boundaries одной HDS VOD presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdsVodPresentationWindow {
    /// Первый advertised fragment timestamp.
    start: Duration,
    /// Exclusive end в той же absolute demux timeline.
    end_exclusive: Duration,
}

impl HdsVodPresentationWindow {
    /// Возвращает absolute presentation start.
    #[must_use]
    pub const fn start(self) -> Duration {
        self.start
    }

    /// Возвращает absolute presentation end.
    #[must_use]
    pub const fn end_exclusive(self) -> Duration {
        self.end_exclusive
    }
}

/// Prepared HDS runtime с optional discovered catalog и receipted seek control.
pub struct HdsVodOpenResult {
    /// Player-facing nonblocking demuxer.
    demuxer: ProgressiveDemuxer,
    /// Worker-receipted seek handle.
    seek_handle: ProgressiveAsyncSeekHandle,
    /// Neutral catalog присутствует только у exact discovered-open path.
    catalog: Option<ComponentVariantCatalog>,
    /// Absolute source window, которое app переводит в player-owned boundary.
    presentation_window: HdsVodPresentationWindow,
}

impl HdsVodOpenResult {
    /// Возвращает cloneable async seek handle до type erasure.
    #[must_use]
    pub fn async_seek_handle(&self) -> ProgressiveAsyncSeekHandle {
        self.seek_handle.clone()
    }

    /// Возвращает neutral discovered catalog без URL/authorization fields.
    #[must_use]
    pub fn catalog(&self) -> Option<&ComponentVariantCatalog> {
        self.catalog.as_ref()
    }

    /// Возвращает absolute HDS presentation window без player dependency.
    #[must_use]
    pub const fn presentation_window(&self) -> HdsVodPresentationWindow {
        self.presentation_window
    }

    /// Передаёт demuxer в app/player boundary.
    #[must_use]
    pub fn into_demuxer(self) -> Box<dyn Demuxer + Send> {
        Box::new(self.demuxer)
    }
}

/// Открывает HDS root/hierarchy, выбирает rendition и поднимает VOD demux worker.
pub fn prepare_hds_vod(request: HdsVodOpenRequest) -> Result<HdsVodOpenResult> {
    if request.transport_request.presentation() != MediaPresentation::Vod {
        bail!("S38 HDS base runtime accepts only VOD transport presentation");
    }
    let root_target = request
        .transport_request
        .target()
        .as_http()
        .cloned()
        .context("HDS transport request target is not HTTP")?;
    let http = AdaptiveHttpContext::new(
        request.transport_request,
        &request.source_config,
        request.policy.adaptive_limits,
        request.policy.adaptive_retry,
    )
    .context("HDS adaptive HTTP context creation failed")?;
    let resolved = resolve_presentation(root_target, &http, request.policy)?;
    let selected = select_rendition(resolved, request.selection)?;
    prepare_selected_hds_vod(selected, http, request.demux_registry, request.policy, None)
}

/// Поднимает unchanged receipted runtime для уже доказанной provider row.
pub(crate) fn prepare_selected_hds_vod(
    selected: ResolvedHdsRendition,
    http: AdaptiveHttpContext,
    demux_registry: Arc<DemuxRegistry>,
    policy: HdsVodOpenPolicy,
    catalog: Option<ComponentVariantCatalog>,
) -> Result<HdsVodOpenResult> {
    let plan = Arc::new(HdsDemuxPlan::new(selected, http, demux_registry, policy)?);
    let preview_plan = Arc::clone(&plan);
    let seek_controller = ProgressiveSeekController::new(move |request| {
        let index = preview_plan.fragment_index_for(request.timestamp);
        let actual_position = preview_plan.fragment_position(index);
        Ok(DemuxSeekResult {
            requested_position: request.timestamp.into(),
            actual_position: actual_position.into(),
            actual_track_timestamp: None,
        })
    });
    let open_plan = Arc::clone(&plan);
    let demuxer = ProgressiveDemuxer::new_deferred_receipted_seekable(
        move || open_transactional_demuxer(open_plan, 0),
        seek_controller,
        plan.http.cancellation().clone(),
        plan.policy.demux_buffer_limits,
        plan.policy.demux_retry_hint,
        ProgressiveRuntimeGeneration::new(plan.http.source_generation().value()),
        plan.policy.async_seek_limits,
    )
    .context("HDS receipted demux worker startup failed")?;
    let seek_handle = demuxer
        .async_seek_handle()
        .context("HDS runtime did not publish seek capability")?;
    let presentation_window = plan.presentation_window;
    Ok(HdsVodOpenResult {
        demuxer,
        seek_handle,
        catalog,
        presentation_window,
    })
}

/// Immutable ingredients shared by initial open and every transactional seek.
pub(crate) struct HdsDemuxPlan {
    http: AdaptiveHttpContext,
    descriptors: Arc<[AdaptiveSegmentDescriptor]>,
    fragments: Arc<[HdsFragment]>,
    timescale: u32,
    presentation_duration: Duration,
    source_duration: Duration,
    presentation_window: HdsVodPresentationWindow,
    registry: Arc<DemuxRegistry>,
    f4f_container: DemuxContainerId,
    policy: HdsVodOpenPolicy,
}

impl HdsDemuxPlan {
    /// Builds all fragment URL descriptors before player commit.
    pub(crate) fn new(
        rendition: ResolvedHdsRendition,
        http: AdaptiveHttpContext,
        registry: Arc<DemuxRegistry>,
        policy: HdsVodOpenPolicy,
    ) -> Result<Self> {
        let fragments = rendition.timeline.fragments().to_vec().into_boxed_slice();
        let presentation_window = presentation_window(
            &fragments,
            rendition.timeline.timescale(),
            rendition.duration,
        )?;
        let mut descriptors = Vec::with_capacity(fragments.len());
        for (index, fragment) in fragments.iter().enumerate() {
            let target = fragment_target(
                &rendition.media_target,
                fragment.segment(),
                fragment.fragment(),
            )?;
            descriptors.push(AdaptiveSegmentDescriptor::full(
                OrderedSegmentSequence::new(
                    u64::try_from(index).map_err(|_| anyhow::anyhow!("HDS sequence overflow"))? + 1,
                ),
                OrderedSegmentKind::Media,
                demux_api::OrderedSegmentDiscontinuity::Continuous,
                target,
            ));
        }
        let f4f_container =
            DemuxContainerId::new("f4f").context("HDS F4F container identity is invalid")?;
        Ok(Self {
            http,
            descriptors: descriptors.into_boxed_slice().into(),
            fragments: fragments.into(),
            timescale: rendition.timeline.timescale(),
            presentation_duration: rendition.duration,
            source_duration: presentation_window.end_exclusive(),
            presentation_window,
            registry,
            f4f_container,
            policy,
        })
    }

    /// Converts a requested duration to the nearest preceding fragment index.
    fn fragment_index_for(&self, timestamp: Duration) -> usize {
        fragment_index_for(&self.fragments, timestamp, self.timescale)
    }

    /// Returns the exact timeline anchor used by the replacement source.
    fn fragment_position(&self, index: usize) -> Duration {
        units_to_duration(self.fragments[index].timestamp(), self.timescale)
    }
}

/// Opens F4F input and wraps it in a seekable transactional container owner.
pub(crate) fn open_transactional_demuxer(
    plan: Arc<HdsDemuxPlan>,
    start_index: usize,
) -> Result<Box<dyn Demuxer + Send>> {
    if start_index >= plan.descriptors.len() {
        bail!("HDS seek anchor is outside the fragment timeline");
    }
    let mut source = AdaptiveOrderedSegmentSource::new(plan.http.clone())
        .context("HDS ordered segment source creation failed")?;
    let snapshot = AdaptiveSegmentSnapshot::new(
        plan.http.source_generation(),
        AdaptivePresentation::Vod {
            duration: Some(plan.presentation_duration),
        },
        ComponentClockMetadata::new(
            std::num::NonZeroU32::new(plan.timescale).context("HDS bootstrap timescale is zero")?,
            0,
        ),
        plan.descriptors[start_index..].to_vec(),
        AdaptiveSegmentCompletion::EndAfterSnapshot,
    )
    .context("HDS adaptive segment snapshot is invalid")?;
    source
        .install_snapshot(snapshot)
        .context("HDS adaptive segment snapshot installation failed")?;
    let input = DemuxInput::ordered_segments(Box::new(BlockingOrderedSegmentAdapter::new(source)));
    let inner = plan
        .registry
        .open_required_container(
            input,
            DemuxHints::none().with_container(plan.f4f_container.clone()),
            plan.policy.demux_sniff_budget,
            plan.http.cancellation().clone(),
            plan.f4f_container.clone(),
        )
        .context("S30 F4F demux open failed")?;
    Ok(Box::new(HdsTransactionalDemuxer::new(inner, plan)?))
}

/// Owns the seek invariant: every replacement begins at a complete F4F fragment.
struct HdsTransactionalDemuxer {
    current: Box<dyn Demuxer + Send>,
    plan: Arc<HdsDemuxPlan>,
    public_tracks: Vec<TrackInfo>,
    pending_events: VecDeque<DemuxReadEvent>,
}

impl HdsTransactionalDemuxer {
    /// Records stable S30 tracks before publishing the seekable wrapper.
    fn new(mut current: Box<dyn Demuxer + Send>, plan: Arc<HdsDemuxPlan>) -> Result<Self> {
        let mut public_tracks = current.tracks().to_vec();
        let mut pending_events = VecDeque::new();
        for _ in 0..plan.policy.demux_buffer_limits.max_pending_events() {
            if has_exact_av_shape(&public_tracks) {
                break;
            }
            match current.next_event()? {
                DemuxReadEvent::TracksChanged(update) => {
                    public_tracks = update.tracks.clone();
                    if has_exact_av_shape(&public_tracks) {
                        pending_events.push_back(DemuxReadEvent::TracksChanged(update));
                    }
                }
                DemuxReadEvent::EndOfStream => {
                    bail!("S30 F4F demux ended before discovering exact HDS A/V tracks");
                }
                event => pending_events.push_back(event),
            }
        }
        if !has_exact_av_shape(&public_tracks) {
            bail!("S30 F4F demux did not discover exact HDS A/V tracks within the event budget");
        }
        Ok(Self {
            public_tracks,
            current,
            plan,
            pending_events,
        })
    }
}

/// HDS base profile публикует только complete coupled row: один video и один audio.
fn has_exact_av_shape(tracks: &[TrackInfo]) -> bool {
    tracks.len() == 2
        && tracks
            .iter()
            .filter(|track| track.kind == media_core::TrackKind::Video)
            .count()
            == 1
        && tracks
            .iter()
            .filter(|track| track.kind == media_core::TrackKind::Audio)
            .count()
            == 1
}

impl Demuxer for HdsTransactionalDemuxer {
    /// Stable track list remains owned by this transactional boundary.
    fn tracks(&self) -> &[TrackInfo] {
        &self.public_tracks
    }

    /// Absolute source end нужен player-у для validation bounded playback window.
    fn duration(&self) -> Option<Duration> {
        Some(self.plan.source_duration)
    }

    /// Container metadata belongs to the current S30 parser.
    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.current.media_metadata()
    }

    /// VOD timeline is seekable through atomic fragment-source replacement.
    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    /// Delegates packet/readiness events to the current F4F demuxer.
    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        let event = match self.pending_events.pop_front() {
            Some(event) => event,
            None => self.current.next_event()?,
        };
        if let DemuxReadEvent::TracksChanged(update) = &event
            && update.tracks != self.public_tracks
        {
            bail!("HDS F4F track layout changed after exact A/V publication");
        }
        Ok(event)
    }

    /// Legacy seek uses the same exact-fragment replacement contract.
    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    /// Opens replacement offside, validates stable tracks, then swaps once.
    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        let target_index = self.plan.fragment_index_for(request.timestamp);
        let replacement = open_transactional_demuxer(Arc::clone(&self.plan), target_index)?;
        if replacement.tracks() != self.public_tracks {
            bail!("HDS replacement changed the public F4F track layout");
        }
        self.current = replacement;
        Ok(DemuxSeekResult {
            requested_position: request.timestamp.into(),
            actual_position: self.plan.fragment_position(target_index).into(),
            actual_track_timestamp: None,
        })
    }
}

/// Строит absolute source window и отклоняет пустую/overflowed presentation.
fn presentation_window(
    fragments: &[HdsFragment],
    timescale: u32,
    presentation_duration: Duration,
) -> Result<HdsVodPresentationWindow> {
    if timescale == 0 {
        bail!("HDS bootstrap timescale is zero");
    }
    if presentation_duration.is_zero() {
        bail!("HDS VOD presentation duration is empty");
    }
    let first_fragment = fragments
        .first()
        .context("HDS VOD fragment timeline is empty")?;
    let start = units_to_duration(first_fragment.timestamp(), timescale);
    let end_exclusive = start
        .checked_add(presentation_duration)
        .context("HDS VOD presentation window overflows duration")?;
    Ok(HdsVodPresentationWindow {
        start,
        end_exclusive,
    })
}

/// Converts duration to timeline units with saturation instead of overflow.
fn duration_to_units(duration: Duration, timescale: u32) -> u64 {
    let seconds = u128::from(duration.as_secs()) * u128::from(timescale);
    let fractional = u128::from(duration.subsec_nanos()) * u128::from(timescale) / 1_000_000_000;
    u64::try_from(seconds.saturating_add(fractional)).unwrap_or(u64::MAX)
}

/// Находит ближайший preceding fragment anchor для transactional VOD seek.
fn fragment_index_for(fragments: &[HdsFragment], timestamp: Duration, timescale: u32) -> usize {
    let requested_units = duration_to_units(timestamp, timescale);
    fragments
        .iter()
        .enumerate()
        .rfind(|(_, fragment)| fragment.timestamp() <= requested_units)
        .map_or(0, |(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::{fragment_index_for, presentation_window};
    use hds_manifest_core::HdsFragment;
    use std::time::Duration;

    /// Проверяет seek до начала, между anchors и после последнего anchor.
    #[test]
    fn vod_seek_uses_preceding_fragment_anchor() {
        let fragments = [
            HdsFragment::new(1, 1, 0, 1_000),
            HdsFragment::new(1, 2, 1_000, 1_000),
            HdsFragment::new(2, 3, 2_000, 1_000),
        ];

        assert_eq!(fragment_index_for(&fragments, Duration::ZERO, 1_000), 0);
        assert_eq!(
            fragment_index_for(&fragments, Duration::from_millis(1_500), 1_000),
            1
        );
        assert_eq!(
            fragment_index_for(&fragments, Duration::from_secs(9), 1_000),
            2
        );
    }

    /// Ненулевой bootstrap origin сохраняется absolute и даёт bounded public window.
    #[test]
    fn vod_presentation_window_preserves_absolute_origin() {
        let fragments = [HdsFragment::new(1, 9, 5_000, 1_000)];

        let window = presentation_window(&fragments, 1_000, Duration::from_secs(2))
            .expect("valid presentation window");

        assert_eq!(window.start(), Duration::from_secs(5));
        assert_eq!(window.end_exclusive(), Duration::from_secs(7));
    }
}
