//! Target-aware initial component construction и content-probed demux handoff.

use std::sync::Arc;

use anyhow::Result;
use demux_api::DemuxRegistry;
use media_core::{
    DemuxReadEvent, DemuxSeekCancellationToken, DemuxSeekRequest, DemuxSeekResult, Demuxer,
    MediaTime, TrackKind,
};
use web_media_adaptive::AdaptiveHttpContext;
use web_media_transport_api::SourceGeneration;

use super::HlsComponentDemuxer;
use crate::active_read::{HlsComponentActiveReadControl, HlsEpochActiveReadLifecycle};
use crate::diagnostics::{
    HlsManifestComponentRole, HlsManifestSeekDiagnosticPhase, HlsManifestSegmentSeekMarker,
};
use crate::plan::{HlsComponentPlan, HlsManifestSeekPoint};
use crate::seek::HlsSeekAnchorKind;
use crate::source::{HlsResourceAttemptObserver, SharedHlsMediaSpanIndex};
use crate::start::HlsResolvedVodStartIntent;
use crate::{HlsVodOpenPolicy, HlsVodSeekLandingPolicy};

/// Initial component open всегда либо создаёт fresh source, либо продолжает ровно тот demux,
/// который уже доказал container на том же bounded resource body.
pub(crate) enum HlsInitialComponentOpen {
    /// Container был известен из authoritative evidence; worker сам открывает выбранный suffix.
    Fresh(HlsResolvedVodStartIntent),
    /// Content probe уже открыл exact initial source, поэтому повторный GET запрещён.
    Probed(HlsProbedInitialComponent),
}

/// Initial proof живёт рядом с parser cursor и покидает component ровно один раз.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HlsInitialPositionEvidence {
    Beginning,
    Positioned(DemuxSeekResult),
}

/// HLS-private handoff одного уже открытого initial demux-а вместе с byte provenance.
pub(crate) struct HlsProbedInitialComponent {
    position: HlsProbedInitialPosition,
    current: Box<dyn Demuxer + Send>,
    media_spans: SharedHlsMediaSpanIndex,
    active_read_lifecycle: HlsEpochActiveReadLifecycle,
}

#[derive(Clone, Copy)]
enum HlsProbedInitialPosition {
    Beginning,
    Restore {
        target: MediaTime,
        point: HlsManifestSeekPoint,
    },
}

enum HlsInitialRestoreSource {
    Fresh,
    Probed {
        point: HlsManifestSeekPoint,
        current: Box<dyn Demuxer + Send>,
        media_spans: SharedHlsMediaSpanIndex,
        active_read_lifecycle: HlsEpochActiveReadLifecycle,
    },
}

/// Направление initial landing-а остаётся явным через probe/open handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlsInitialRestoreLandingIntent {
    /// Быстрый VOD path требует доказанный anchor не раньше checkpoint-а.
    AuthoritativePostTarget,
    /// Near-EOS fallback сохраняет прежнее decode-forward поведение.
    DecodeForwardToTarget,
}

/// Manifest point вместе с обязательной scanner-семантикой initial restore-а.
#[derive(Clone, Copy)]
struct HlsInitialRestoreCandidate {
    point: HlsManifestSeekPoint,
    landing_intent: HlsInitialRestoreLandingIntent,
}

impl HlsProbedInitialComponent {
    /// Передаёт content-proven demuxer обычному beginning-open без повторного resource request.
    pub(crate) fn beginning(
        current: Box<dyn Demuxer + Send>,
        media_spans: SharedHlsMediaSpanIndex,
        active_read_lifecycle: HlsEpochActiveReadLifecycle,
    ) -> Self {
        Self {
            position: HlsProbedInitialPosition::Beginning,
            current,
            media_spans,
            active_read_lifecycle,
        }
    }

    /// Передаёт content-proven demuxer exact containing manifest candidate-а restore path-у.
    pub(crate) fn restore(
        target: MediaTime,
        point: HlsManifestSeekPoint,
        current: Box<dyn Demuxer + Send>,
        media_spans: SharedHlsMediaSpanIndex,
        active_read_lifecycle: HlsEpochActiveReadLifecycle,
    ) -> Self {
        Self {
            position: HlsProbedInitialPosition::Restore { target, point },
            current,
            media_spans,
            active_read_lifecycle,
        }
    }
}

impl HlsComponentDemuxer {
    /// Сохраняет старый beginning path и добавляет fail-closed target-aware restore.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn open_initial(
        plan: HlsComponentPlan,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        policy: HlsVodOpenPolicy,
        registry: Arc<DemuxRegistry>,
        seek_index: crate::seek::SharedHlsSeekIndex,
        active_read_control: HlsComponentActiveReadControl,
        initial_open: HlsInitialComponentOpen,
    ) -> Result<Self> {
        match initial_open {
            HlsInitialComponentOpen::Fresh(HlsResolvedVodStartIntent::Beginning) => {
                let mut component = Self::open_from_epoch(
                    plan,
                    http,
                    generation,
                    policy,
                    registry,
                    seek_index,
                    active_read_control.clone(),
                    0,
                    DemuxSeekCancellationToken::new(),
                )?;
                component.prime_initial_seek_anchor()?;
                component.stage_initial_open_marker()?;
                Ok(component)
            }
            HlsInitialComponentOpen::Fresh(HlsResolvedVodStartIntent::Restore(target)) => {
                Self::open_initial_restore(
                    plan,
                    http,
                    generation,
                    policy,
                    registry,
                    seek_index,
                    active_read_control,
                    target,
                    HlsInitialRestoreSource::Fresh,
                )
            }
            HlsInitialComponentOpen::Probed(probed) => match probed.position {
                HlsProbedInitialPosition::Beginning => {
                    let timeline_start = plan
                        .epochs
                        .first()
                        .ok_or_else(|| anyhow::anyhow!("HLS initial epoch отсутствует в plan"))?
                        .timeline_start;
                    let mut component = Self::from_opened_epoch(
                        plan,
                        http,
                        generation,
                        policy,
                        registry,
                        seek_index,
                        active_read_control,
                        0,
                        timeline_start,
                        None,
                        probed.current,
                        probed.media_spans,
                        probed.active_read_lifecycle,
                    );
                    component.prime_initial_seek_anchor()?;
                    component.stage_initial_open_marker()?;
                    Ok(component)
                }
                HlsProbedInitialPosition::Restore { target, point } => Self::open_initial_restore(
                    plan,
                    http,
                    generation,
                    policy,
                    registry,
                    seek_index,
                    active_read_control,
                    target,
                    HlsInitialRestoreSource::Probed {
                        point,
                        current: probed.current,
                        media_spans: probed.media_spans,
                        active_read_lifecycle: probed.active_read_lifecycle,
                    },
                ),
            },
        }
    }

    /// Строит candidates по HLS-owned policy до первого fresh segment request-а.
    #[allow(clippy::too_many_arguments)]
    fn open_initial_restore(
        plan: HlsComponentPlan,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        policy: HlsVodOpenPolicy,
        registry: Arc<DemuxRegistry>,
        seek_index: crate::seek::SharedHlsSeekIndex,
        active_read_control: HlsComponentActiveReadControl,
        target: MediaTime,
        initial_source: HlsInitialRestoreSource,
    ) -> Result<Self> {
        if target.as_duration() > plan.duration {
            anyhow::bail!("HLS initial restore target находится за VOD duration");
        }
        let post_target_point = match policy.seek_landing_policy {
            HlsVodSeekLandingPolicy::DecodeFromOrBeforeTarget => None,
            HlsVodSeekLandingPolicy::PreferPostTargetRap => {
                plan.post_target_manifest_seek_point(target.as_duration())
            }
        };
        let mut candidates = post_target_point
            .map(|point| HlsInitialRestoreCandidate {
                point,
                landing_intent: HlsInitialRestoreLandingIntent::AuthoritativePostTarget,
            })
            .into_iter()
            .collect::<Vec<_>>();
        for point in plan.manifest_seek_candidates(target.as_duration()) {
            if candidates.iter().any(|candidate| candidate.point == point) {
                continue;
            }
            candidates.push(HlsInitialRestoreCandidate {
                point,
                landing_intent: HlsInitialRestoreLandingIntent::DecodeForwardToTarget,
            });
        }
        if candidates.is_empty() {
            anyhow::bail!("HLS initial restore не нашёл manifest candidate");
        }

        let try_fresh_candidates =
            |candidate_points: &[HlsInitialRestoreCandidate]| -> Result<Self> {
                for candidate in candidate_points {
                    let component = Self::open_from_manifest_seek_point(
                        plan.clone(),
                        http.clone(),
                        generation,
                        policy,
                        Arc::clone(&registry),
                        seek_index.clone(),
                        active_read_control.clone(),
                        candidate.point,
                        DemuxSeekCancellationToken::new(),
                        HlsResourceAttemptObserver::disabled(),
                    )?;
                    if let Some((positioned, result)) = Self::position_initial_restore_candidate(
                        component,
                        target,
                        candidate.landing_intent,
                    )? {
                        return Ok(Self::with_initial_position_result(positioned, result));
                    }
                }
                anyhow::bail!("HLS initial restore candidates не доказали decode anchor")
            };

        match initial_source {
            HlsInitialRestoreSource::Fresh => try_fresh_candidates(&candidates),
            HlsInitialRestoreSource::Probed {
                point,
                current,
                media_spans,
                active_read_lifecycle,
            } => {
                if candidates.first().map(|candidate| candidate.point) != Some(point) {
                    anyhow::bail!("HLS probed restore source не совпадает с containing candidate");
                }
                let component = Self::from_opened_epoch(
                    plan.clone(),
                    http.clone(),
                    generation,
                    policy,
                    Arc::clone(&registry),
                    seek_index.clone(),
                    active_read_control.clone(),
                    point.epoch_index,
                    point.timeline_start,
                    None,
                    current,
                    media_spans,
                    active_read_lifecycle,
                );
                if let Some((positioned, result)) = Self::position_initial_restore_candidate(
                    component,
                    target,
                    candidates[0].landing_intent,
                )? {
                    return Ok(Self::with_initial_position_result(positioned, result));
                }
                try_fresh_candidates(&candidates[1..])
            }
        }
    }

    /// Доказывает topology и настоящий RAP/audio anchor, не публикуя ложный startup commit.
    fn position_initial_restore_candidate(
        mut component: Self,
        target: MediaTime,
        landing_intent: HlsInitialRestoreLandingIntent,
    ) -> Result<Option<(Self, DemuxSeekResult)>> {
        let required_kind = if component
            .public_tracks
            .iter()
            .any(|track| track.kind == TrackKind::Video)
        {
            HlsSeekAnchorKind::VideoRandomAccessPoint
        } else {
            HlsSeekAnchorKind::AudioPacket
        };
        let request = match required_kind {
            HlsSeekAnchorKind::VideoRandomAccessPoint => {
                DemuxSeekRequest::decode_point_before(target.as_duration())
            }
            HlsSeekAnchorKind::AudioPacket => DemuxSeekRequest::accurate(target.as_duration()),
        };
        let anchor = match landing_intent {
            HlsInitialRestoreLandingIntent::AuthoritativePostTarget => {
                component.position_at_first_post_target_manifest_anchor(request, required_kind)?
            }
            HlsInitialRestoreLandingIntent::DecodeForwardToTarget => {
                component.position_at_first_manifest_anchor(request, required_kind)?
            }
        };
        if let Some(anchor) = anchor {
            component.suppress_initial_restore_tracks_changed()?;
            let result = crate::seek::HlsSeekIndex::result_for_anchor(request, anchor);
            let component_role = HlsManifestComponentRole::from_tracks(&component.public_tracks)?;
            component.stage_committed_selection_marker(HlsManifestSegmentSeekMarker::new(
                HlsManifestSeekDiagnosticPhase::InitialRestore,
                component_role,
                component.policy.seek_landing_policy,
                component.generation,
                target.as_duration(),
                anchor,
            ));
            Ok(Some((component, result)))
        } else {
            Ok(None)
        }
    }

    /// Связывает доказанный result с тем же parser cursor без manifest-start догадки.
    fn with_initial_position_result(mut component: Self, result: DemuxSeekResult) -> Self {
        component.initial_position_evidence = HlsInitialPositionEvidence::Positioned(result);
        component
    }

    /// Beginning open также публикует только packet-derived, а не manifest-start anchor.
    fn stage_initial_open_marker(&mut self) -> Result<()> {
        let has_video = self
            .public_tracks
            .iter()
            .any(|track| track.kind == TrackKind::Video);
        let anchor = self
            .seek_index
            .lock()
            .initial_anchor(has_video)
            .ok_or_else(|| anyhow::anyhow!("HLS initial open не сохранил packet-derived anchor"))?;
        let component_role = HlsManifestComponentRole::from_tracks(&self.public_tracks)?;
        self.stage_committed_selection_marker(HlsManifestSegmentSeekMarker::new(
            HlsManifestSeekDiagnosticPhase::InitialOpen,
            component_role,
            self.policy.seek_landing_policy,
            self.generation,
            std::time::Duration::ZERO,
            anchor,
        ));
        Ok(())
    }

    /// Progressive startup сам публикует initial topology; candidate replay не должен делать второй reset.
    fn suppress_initial_restore_tracks_changed(&mut self) -> Result<()> {
        match self.replay_events.pop_front() {
            Some(DemuxReadEvent::TracksChanged(_)) => Ok(()),
            Some(unexpected) => {
                self.replay_events.push_front(unexpected);
                anyhow::bail!("HLS initial restore replay не началcя с TracksChanged")
            }
            None => anyhow::bail!("HLS initial restore не содержит positioned replay lifecycle"),
        }
    }
}
