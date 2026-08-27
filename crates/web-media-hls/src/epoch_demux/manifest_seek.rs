//! Manifest-owned подготовка near-target HLS replacement-а для worker-receipted seek.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use demux_api::DemuxRegistry;
use media_core::{
    DemuxReadEvent, DemuxSeekCancellationToken, DemuxSeekRequest, DemuxSeekResult, MediaTime,
    Packet, PacketKeyframe, TrackInfo, TrackKind,
};
use web_media_adaptive::AdaptiveHttpContext;
use web_media_transport_api::SourceGeneration;

use super::{HlsComponentDemuxer, HlsComponentFactory, event_encoded_bytes, packet_matches_anchor};
use crate::plan::{HlsComponentPlan, HlsManifestSeekPoint};
use crate::seek::{HlsSeekAnchor, HlsSeekAnchorKind, HlsSeekIndex, SharedHlsSeekIndex};
use crate::source::{
    HlsResourceAttemptFailure, HlsResourceAttemptObserver, HlsTransientBodyFailureCategory,
    SharedHlsResourceAttemptFailure,
};
use crate::{HlsVodOpenPolicy, HlsVodSeekLandingPolicy};

/// Typed retry decision одного временного manifest candidate-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlsManifestCandidateRetryDecision {
    /// Failure не относится к разрешённой body category либо lifecycle уже terminal.
    DoNotRestart,
    /// Весь parser/source надо уничтожить и ровно один раз открыть candidate с byte zero.
    RestartOnce {
        /// Secret-free причина restart-а для terminal diagnostics.
        category: HlsTransientBodyFailureCategory,
    },
}

/// Lifecycle snapshot, проверяемый после failure и непосредственно перед fresh request-ом.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlsManifestCandidateLifecycle {
    /// Оба cancellation token-а активны, generation совпадает.
    Active,
    /// Source shutdown либо supersede уже сделали restart terminally недопустимым.
    Cancelled,
    /// Immutable HTTP context принадлежит другой source generation.
    StaleGeneration,
}

/// Направление manifest landing-а, которое scanner обязан доказать до replacement commit-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlsManifestLandingIntent {
    /// Legacy bounded fallback начинает с decoder anchor-а не позже target-а.
    DecodeForwardToTarget,
    /// Fast HLS VOD path принимает только настоящий anchor не раньше target-а.
    AuthoritativePostTarget,
}

/// Чистая typed policy отделяет retry evidence от lifecycle fencing.
fn manifest_candidate_retry_decision(
    failure: HlsResourceAttemptFailure,
    lifecycle: HlsManifestCandidateLifecycle,
) -> HlsManifestCandidateRetryDecision {
    if lifecycle != HlsManifestCandidateLifecycle::Active {
        return HlsManifestCandidateRetryDecision::DoNotRestart;
    }
    match failure {
        HlsResourceAttemptFailure::TransientBody(category) => {
            HlsManifestCandidateRetryDecision::RestartOnce { category }
        }
        HlsResourceAttemptFailure::None => HlsManifestCandidateRetryDecision::DoNotRestart,
    }
}

impl HlsComponentFactory {
    /// Применяет source-selected HLS policy до необратимого выбора manifest segment-а.
    pub(crate) fn prepare_receipted_seek_replacement(
        &self,
        request: DemuxSeekRequest,
        stable_public_tracks: &[TrackInfo],
    ) -> Result<(HlsComponentDemuxer, DemuxSeekResult)> {
        let post_target_point = match self.policy.seek_landing_policy {
            HlsVodSeekLandingPolicy::DecodeFromOrBeforeTarget => None,
            HlsVodSeekLandingPolicy::PreferPostTargetRap => {
                self.plan.post_target_manifest_seek_point(request.timestamp)
            }
        };
        if let Some(manifest_point) = post_target_point
            && let Some(prepared) = self.prepare_manifest_seek_candidate(
                request,
                stable_public_tracks,
                manifest_point,
                HlsManifestLandingIntent::AuthoritativePostTarget,
            )?
        {
            return Ok(prepared);
        }
        for manifest_point in self.plan.manifest_seek_candidates(request.timestamp) {
            if Some(manifest_point) == post_target_point {
                continue;
            }
            if let Some(prepared) = self.prepare_manifest_seek_candidate(
                request,
                stable_public_tracks,
                manifest_point,
                HlsManifestLandingIntent::DecodeForwardToTarget,
            )? {
                return Ok(prepared);
            }
        }
        // Near-EOS либо malformed segment без будущего RAP сохраняет прежний bounded path.
        // Player route остаётся source-scoped и выберет target-floor, если actual не post-target.
        self.prepare_seek_replacement(request, stable_public_tracks)
    }

    /// Готовит separate-audio replacement не позже уже доказанного video landing-а.
    pub(crate) fn prepare_aligned_audio_seek_replacement(
        &self,
        video_landing: MediaTime,
        stable_public_tracks: &[TrackInfo],
    ) -> Result<(HlsComponentDemuxer, DemuxSeekResult)> {
        let request = DemuxSeekRequest::accurate(video_landing.as_duration());
        for manifest_point in self.plan.manifest_seek_candidates(request.timestamp) {
            if let Some(prepared) = self.prepare_manifest_seek_candidate(
                request,
                stable_public_tracks,
                manifest_point,
                HlsManifestLandingIntent::DecodeForwardToTarget,
            )? {
                return Ok(prepared);
            }
        }
        self.prepare_seek_replacement(request, stable_public_tracks)
    }

    /// Проверяет один immutable manifest candidate offside от active demuxer-а.
    fn prepare_manifest_seek_candidate(
        &self,
        request: DemuxSeekRequest,
        stable_public_tracks: &[TrackInfo],
        manifest_point: HlsManifestSeekPoint,
        landing_intent: HlsManifestLandingIntent,
    ) -> Result<Option<(HlsComponentDemuxer, DemuxSeekResult)>> {
        // Kind выбирается по уже опубликованному index-у: Accurate seek не должен случайно
        // переключиться с audio на video только из-за порядка packets в новом suffix-е.
        let required_kind = match landing_intent {
            HlsManifestLandingIntent::AuthoritativePostTarget
                if stable_public_tracks
                    .iter()
                    .any(|track| track.kind == TrackKind::Video) =>
            {
                HlsSeekAnchorKind::VideoRandomAccessPoint
            }
            HlsManifestLandingIntent::AuthoritativePostTarget => HlsSeekAnchorKind::AudioPacket,
            HlsManifestLandingIntent::DecodeForwardToTarget => {
                self.seek_index.lock().required_kind(request)
            }
        };
        let first_failure = SharedHlsResourceAttemptFailure::default();
        let first_result = self.prepare_manifest_seek_candidate_attempt(
            request,
            stable_public_tracks,
            manifest_point,
            required_kind,
            landing_intent,
            HlsResourceAttemptObserver::capture(first_failure.clone()),
        );
        let first_error = match first_result {
            Ok(prepared) => return Ok(prepared),
            Err(error) => error,
        };
        let HlsManifestCandidateRetryDecision::RestartOnce { category } =
            manifest_candidate_retry_decision(first_failure.snapshot(), self.manifest_lifecycle())
        else {
            return Err(first_error);
        };

        let restarted_failure = SharedHlsResourceAttemptFailure::default();
        self.prepare_manifest_seek_candidate_attempt(
            request,
            stable_public_tracks,
            manifest_point,
            required_kind,
            landing_intent,
            HlsResourceAttemptObserver::capture(restarted_failure.clone()),
        )
        .with_context(|| {
            format!(
                "HLS manifest candidate single fresh restart failed; initial category={category:?}; restart terminal={:?}",
                restarted_failure.snapshot()
            )
        })
    }

    /// Выполняет один полностью изолированный parser/source attempt без изменения active component-а.
    fn prepare_manifest_seek_candidate_attempt(
        &self,
        request: DemuxSeekRequest,
        stable_public_tracks: &[TrackInfo],
        manifest_point: HlsManifestSeekPoint,
        required_kind: HlsSeekAnchorKind,
        landing_intent: HlsManifestLandingIntent,
        resource_attempt_observer: HlsResourceAttemptObserver,
    ) -> Result<Option<(HlsComponentDemuxer, DemuxSeekResult)>> {
        let replacement_index =
            SharedHlsSeekIndex::new(self.policy.maximum_seek_index_entries.get());
        let mut replacement = HlsComponentDemuxer::open_from_manifest_seek_point(
            self.plan.clone(),
            self.http.clone(),
            self.generation,
            self.policy,
            Arc::clone(&self.registry),
            replacement_index,
            self.active_read_control.clone(),
            manifest_point,
            self.seek_cancellation.clone(),
            resource_attempt_observer,
        )?;
        replacement.public_tracks = stable_public_tracks.to_vec();
        let replacement_tracks = replacement.current.tracks().to_vec();
        replacement.refresh_track_mapping(&replacement_tracks)?;
        let anchor = match landing_intent {
            HlsManifestLandingIntent::DecodeForwardToTarget => {
                replacement.position_at_first_manifest_anchor(request, required_kind)?
            }
            HlsManifestLandingIntent::AuthoritativePostTarget => {
                replacement.position_at_first_post_target_manifest_anchor(request, required_kind)?
            }
        };
        let Some(anchor) = anchor else {
            return Ok(None);
        };
        let result = HlsSeekIndex::result_for_anchor(request, anchor);
        self.seek_index.lock().commit_proven_anchor(anchor);
        replacement.seek_index = self.seek_index.clone();
        Ok(Some((replacement, result)))
    }

    /// Разрешает restart только активному generation/token и только по typed body evidence.
    fn manifest_lifecycle(&self) -> HlsManifestCandidateLifecycle {
        if self.http.cancellation().is_cancelled() || self.seek_cancellation.is_cancelled() {
            return HlsManifestCandidateLifecycle::Cancelled;
        }
        if self.http.source_generation() != self.generation {
            return HlsManifestCandidateLifecycle::StaleGeneration;
        }
        HlsManifestCandidateLifecycle::Active
    }
}

impl HlsComponentDemuxer {
    /// Открывает manifest suffix без изменения active component и без fabricated packet evidence.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn open_from_manifest_seek_point(
        plan: HlsComponentPlan,
        http: AdaptiveHttpContext,
        generation: SourceGeneration,
        policy: HlsVodOpenPolicy,
        registry: Arc<DemuxRegistry>,
        seek_index: SharedHlsSeekIndex,
        active_read_control: crate::active_read::HlsComponentActiveReadControl,
        point: HlsManifestSeekPoint,
        seek_cancellation: DemuxSeekCancellationToken,
        resource_attempt_observer: HlsResourceAttemptObserver,
    ) -> Result<Self> {
        let epoch = plan.manifest_restart_tail(point).ok_or_else(|| {
            anyhow::anyhow!("HLS manifest seek point отсутствует в immutable plan")
        })?;
        Self::open_from_epoch_plan(
            plan,
            http,
            generation,
            policy,
            registry,
            seek_index,
            active_read_control,
            point.epoch_index,
            epoch,
            None,
            seek_cancellation,
            resource_attempt_observer,
        )
    }

    /// Находит первый настоящий RAP/audio anchor не позже target внутри near-target suffix-а.
    pub(super) fn position_at_first_manifest_anchor(
        &mut self,
        request: DemuxSeekRequest,
        required_kind: HlsSeekAnchorKind,
    ) -> Result<Option<HlsSeekAnchor>> {
        let mut inspected_events = 0_usize;
        let mut inspected_bytes = 0_usize;
        let mut retained_audio_packets = VecDeque::<Packet>::new();
        loop {
            let event = self.read_next_inner_event()?;
            inspected_events = inspected_events.saturating_add(1);
            inspected_bytes = inspected_bytes.saturating_add(event_encoded_bytes(&event));
            if inspected_events > self.policy.maximum_seek_replay_events.get()
                || inspected_bytes > self.policy.maximum_seek_replay_bytes.get()
            {
                return Ok(None);
            }
            match event {
                DemuxReadEvent::Packet(packet) => {
                    let matching_anchor = {
                        let seek_index = self.seek_index.lock();
                        seek_index
                            .anchor_of_kind_before(required_kind, request.timestamp)
                            .ok()
                            .filter(|anchor| packet_matches_anchor(&packet, *anchor))
                    };
                    if let Some(anchor) = matching_anchor {
                        self.replay_events.push_back(self.tracks_changed_event());
                        self.replay_events.push_back(DemuxReadEvent::Packet(packet));
                        self.replay_events.extend(
                            retained_audio_packets
                                .into_iter()
                                .filter(|audio| {
                                    MediaTime::from_duration(audio.pts) >= anchor.position
                                })
                                .map(DemuxReadEvent::Packet),
                        );
                        return Ok(Some(anchor));
                    }
                    if packet_passed_manifest_target(&packet, required_kind, request.timestamp) {
                        return Ok(None);
                    }
                    if required_kind == HlsSeekAnchorKind::VideoRandomAccessPoint
                        && packet.kind == TrackKind::Audio
                    {
                        retained_audio_packets.push_back(packet);
                    }
                }
                DemuxReadEvent::EndOfStream => return Ok(None),
                DemuxReadEvent::TracksChanged(_)
                | DemuxReadEvent::MediaMetadataChanged(_)
                | DemuxReadEvent::TemporarilyUnavailable(_) => {}
            }
        }
    }

    /// Находит первый настоящий RAP/audio anchor не раньше target-а в выбранном suffix-е.
    pub(super) fn position_at_first_post_target_manifest_anchor(
        &mut self,
        request: DemuxSeekRequest,
        required_kind: HlsSeekAnchorKind,
    ) -> Result<Option<HlsSeekAnchor>> {
        let mut inspected_events = 0_usize;
        let mut inspected_bytes = 0_usize;
        let mut retained_audio_packets = VecDeque::<Packet>::new();
        loop {
            let event = self.read_next_inner_event()?;
            inspected_events = inspected_events.saturating_add(1);
            inspected_bytes = inspected_bytes.saturating_add(event_encoded_bytes(&event));
            if inspected_events > self.policy.maximum_seek_replay_events.get()
                || inspected_bytes > self.policy.maximum_seek_replay_bytes.get()
            {
                return Ok(None);
            }
            match event {
                DemuxReadEvent::Packet(packet) => {
                    let matching_anchor = {
                        let seek_index = self.seek_index.lock();
                        seek_index
                            .anchor_of_kind_at_or_after(required_kind, request.timestamp)
                            .ok()
                            .filter(|anchor| packet_matches_anchor(&packet, *anchor))
                    };
                    if let Some(anchor) = matching_anchor {
                        self.replay_events.push_back(self.tracks_changed_event());
                        self.replay_events.push_back(DemuxReadEvent::Packet(packet));
                        self.replay_events.extend(
                            retained_audio_packets
                                .into_iter()
                                .filter(|audio| {
                                    MediaTime::from_duration(audio.pts) >= anchor.position
                                })
                                .map(DemuxReadEvent::Packet),
                        );
                        return Ok(Some(anchor));
                    }
                    if required_kind == HlsSeekAnchorKind::VideoRandomAccessPoint
                        && packet.kind == TrackKind::Audio
                    {
                        retained_audio_packets.push_back(packet);
                    }
                }
                DemuxReadEvent::EndOfStream => return Ok(None),
                DemuxReadEvent::TracksChanged(_)
                | DemuxReadEvent::MediaMetadataChanged(_)
                | DemuxReadEvent::TemporarilyUnavailable(_) => {}
            }
        }
    }
}

/// Определяет, что qualifying decode point уже прошёл target.
///
/// PTS может быть позже target из-за нормального frame reordering, поэтому
/// `DecodePointBefore` обязан сравнивать DTS, а receipt сохраняет настоящий PTS.
fn packet_passed_manifest_target(
    packet: &Packet,
    required_kind: HlsSeekAnchorKind,
    target: Duration,
) -> bool {
    let kind_matches = match required_kind {
        HlsSeekAnchorKind::VideoRandomAccessPoint => {
            packet.kind == TrackKind::Video && packet.keyframe == PacketKeyframe::Keyframe
        }
        HlsSeekAnchorKind::AudioPacket => packet.kind == TrackKind::Audio,
    };
    kind_matches && packet.dts.unwrap_or(packet.pts) > target
}

#[cfg(test)]
mod tests {
    use super::{
        HlsManifestCandidateLifecycle, HlsManifestCandidateRetryDecision,
        manifest_candidate_retry_decision,
    };
    use crate::source::{HlsResourceAttemptFailure, HlsTransientBodyFailureCategory};

    #[test]
    fn cancellation_after_transient_failure_prevents_restart() {
        assert_eq!(
            manifest_candidate_retry_decision(
                HlsResourceAttemptFailure::TransientBody(HlsTransientBodyFailureCategory::Read),
                HlsManifestCandidateLifecycle::Cancelled,
            ),
            HlsManifestCandidateRetryDecision::DoNotRestart
        );
    }

    #[test]
    fn stale_generation_after_transient_failure_prevents_restart() {
        assert_eq!(
            manifest_candidate_retry_decision(
                HlsResourceAttemptFailure::TransientBody(
                    HlsTransientBodyFailureCategory::UnexpectedEof,
                ),
                HlsManifestCandidateLifecycle::StaleGeneration,
            ),
            HlsManifestCandidateRetryDecision::DoNotRestart
        );
    }

    #[test]
    fn active_transient_failure_allows_exactly_one_restart_decision() {
        assert_eq!(
            manifest_candidate_retry_decision(
                HlsResourceAttemptFailure::TransientBody(HlsTransientBodyFailureCategory::Timeout,),
                HlsManifestCandidateLifecycle::Active,
            ),
            HlsManifestCandidateRetryDecision::RestartOnce {
                category: HlsTransientBodyFailureCategory::Timeout,
            }
        );
        assert_eq!(
            manifest_candidate_retry_decision(
                HlsResourceAttemptFailure::None,
                HlsManifestCandidateLifecycle::Active,
            ),
            HlsManifestCandidateRetryDecision::DoNotRestart
        );
    }
}
