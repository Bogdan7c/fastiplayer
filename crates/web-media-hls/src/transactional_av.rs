//! Transactional separate-A/V seek: active composite меняется только готовой парой replacements.

use std::time::Duration;

use anyhow::{Result, anyhow};
use demux_api::{
    CompositeAvDemuxer, CompositeAvPublicTrackIds, CompositeAvTrackSelection,
    CompositeComponentLeadPolicy,
};
use media_core::{
    DemuxReadEvent, DemuxSeekCancellationCompletion, DemuxSeekCancellationToken, DemuxSeekRequest,
    DemuxSeekResult, DemuxSeekability, Demuxer, MediaDemuxError, MediaMetadata, TrackId, TrackInfo,
    TrackKind,
};

use crate::epoch_demux::{HlsComponentDemuxer, HlsComponentFactory, HlsStagedSelectionCommit};

/// Выбирает подготовку replacement без неочевидного позиционного `bool` у callsite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlsComponentSeekIntent {
    /// Preview уже закрепил точный наблюдавшийся anchor, поэтому менять его нельзя.
    PreviewedExactAnchor,
    /// Worker может доказать near-target anchor до публикации seek receipt.
    ReceiptedManifestCandidate,
}

impl HlsComponentSeekIntent {
    /// Делегирует подготовку владельцу HLS component state с нужной seek-семантикой.
    fn prepare_component(
        self,
        factory: &HlsComponentFactory,
        request: DemuxSeekRequest,
        public_tracks: &[TrackInfo],
    ) -> Result<(HlsComponentDemuxer, DemuxSeekResult)> {
        match self {
            Self::PreviewedExactAnchor => factory.prepare_seek_replacement(request, public_tracks),
            Self::ReceiptedManifestCandidate => {
                factory.prepare_receipted_seek_replacement(request, public_tracks)
            }
        }
    }

    /// Выравнивает alternate audio по уже доказанному video landing-у.
    fn prepare_audio_component(
        self,
        factory: &HlsComponentFactory,
        public_request: DemuxSeekRequest,
        video_result: DemuxSeekResult,
        public_tracks: &[TrackInfo],
    ) -> Result<(HlsComponentDemuxer, DemuxSeekResult)> {
        match self {
            Self::PreviewedExactAnchor => factory.prepare_seek_replacement(
                DemuxSeekRequest::accurate(public_request.timestamp),
                public_tracks,
            ),
            Self::ReceiptedManifestCandidate => factory.prepare_aligned_audio_seek_replacement(
                public_request.timestamp,
                video_result.actual_position,
                public_tracks,
            ),
        }
    }
}

/// HLS-owned composite boundary, запрещающий частично применённый video/audio seek.
pub(crate) struct TransactionalHlsAvDemuxer {
    current: CompositeAvDemuxer,
    video_factory: HlsComponentFactory,
    audio_factory: HlsComponentFactory,
    video_public_tracks: Vec<TrackInfo>,
    audio_public_tracks: Vec<TrackInfo>,
    public_track_ids: CompositeAvPublicTrackIds,
    lead_policy: CompositeComponentLeadPolicy,
}

impl TransactionalHlsAvDemuxer {
    /// Собирает initial active composite и сохраняет stable public topology.
    pub(crate) fn new(
        video_factory: HlsComponentFactory,
        audio_factory: HlsComponentFactory,
        mut video: HlsComponentDemuxer,
        mut audio: HlsComponentDemuxer,
        lead_policy: CompositeComponentLeadPolicy,
    ) -> Result<(Self, [HlsStagedSelectionCommit; 2])> {
        let video_public_tracks = video.tracks().to_vec();
        let audio_public_tracks = audio.tracks().to_vec();
        let video_track = exactly_one_track(&video_public_tracks, TrackKind::Video, "video")?;
        let audio_track = exactly_one_track(&audio_public_tracks, TrackKind::Audio, "audio")?;
        let selection = CompositeAvTrackSelection::new(video_track, audio_track);
        video.activate_committed_read()?;
        audio.activate_committed_read()?;
        let committed_selections = [
            video.take_staged_selection_commit(),
            audio.take_staged_selection_commit(),
        ];
        let current =
            CompositeAvDemuxer::new(Box::new(video), Box::new(audio), selection, lead_policy)?;
        let public_track_ids = CompositeAvPublicTrackIds::new(
            current.public_video_track_id(),
            current.public_audio_track_id(),
        );
        let composite = Self {
            current,
            video_factory,
            audio_factory,
            video_public_tracks,
            audio_public_tracks,
            public_track_ids,
            lead_policy,
        };
        Ok((composite, committed_selections))
    }

    /// Готовит video/audio replacements целиком и только затем меняет active pair.
    fn seek_component_pair(
        &mut self,
        request: DemuxSeekRequest,
        intent: HlsComponentSeekIntent,
    ) -> Result<DemuxSeekResult> {
        self.seek_component_pair_with_factories(
            request,
            intent,
            self.video_factory.clone(),
            self.audio_factory.clone(),
            || Ok(()),
        )
    }

    /// Проводит один request token через обе offside подготовки и завершает его до commit-а.
    fn seek_cancellable_component_pair(
        &mut self,
        request: DemuxSeekRequest,
        intent: HlsComponentSeekIntent,
        cancellation: DemuxSeekCancellationToken,
    ) -> Result<DemuxSeekResult> {
        if cancellation.is_cancelled() {
            return Err(MediaDemuxError::SeekCancelled.into());
        }
        let video_factory = self
            .video_factory
            .clone()
            .with_seek_cancellation(cancellation.clone());
        let audio_factory = self
            .audio_factory
            .clone()
            .with_seek_cancellation(cancellation.clone());
        self.seek_component_pair_with_factories(
            request,
            intent,
            video_factory,
            audio_factory,
            || match cancellation.complete() {
                DemuxSeekCancellationCompletion::Completed => Ok(()),
                DemuxSeekCancellationCompletion::CancellationWon => {
                    Err(MediaDemuxError::SeekCancelled.into())
                }
            },
        )
    }

    /// Собирает replacement из явно выбранных factories и отделяет prepare от commit authority.
    fn seek_component_pair_with_factories(
        &mut self,
        request: DemuxSeekRequest,
        intent: HlsComponentSeekIntent,
        video_factory: HlsComponentFactory,
        audio_factory: HlsComponentFactory,
        authorize_commit: impl FnOnce() -> Result<()>,
    ) -> Result<DemuxSeekResult> {
        let video_public_tracks = &self.video_public_tracks;
        let audio_public_tracks = &self.audio_public_tracks;
        let public_track_ids = self.public_track_ids;
        let lead_policy = self.lead_policy;
        let (result, _) = transact_component_pair(
            &mut self.current,
            || intent.prepare_component(&video_factory, request, video_public_tracks),
            |(_, video_result)| {
                intent.prepare_audio_component(
                    &audio_factory,
                    request,
                    *video_result,
                    audio_public_tracks,
                )
            },
            |(mut video, mut video_result), (mut audio, _)| {
                let committed_selections = [
                    Some(video.take_staged_selection_commit()),
                    Some(audio.take_staged_selection_commit()),
                ];
                video.activate_committed_read()?;
                audio.activate_committed_read()?;
                audio.suppress_redundant_composite_tracks_changed()?;
                let selection = CompositeAvTrackSelection::new(
                    exactly_one_track(video.tracks(), TrackKind::Video, "replacement video")?,
                    exactly_one_track(audio.tracks(), TrackKind::Audio, "replacement audio")?,
                );
                let replacement = CompositeAvDemuxer::new_with_public_track_ids(
                    Box::new(video),
                    Box::new(audio),
                    selection,
                    public_track_ids,
                    lead_policy,
                )?;
                if let Some(timestamp) = &mut video_result.actual_track_timestamp {
                    timestamp.track_id = public_track_ids.video_track_id();
                }
                Ok((replacement, (video_result, committed_selections)))
            },
            authorize_commit,
            |(_, selections)| {
                for selection in selections.iter_mut().filter_map(Option::take) {
                    selection.commit();
                }
            },
        )?;
        Ok(result)
    }
}

impl Demuxer for TransactionalHlsAvDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        self.current.tracks()
    }

    fn duration(&self) -> Option<Duration> {
        self.current.duration()
    }

    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.current.media_metadata()
    }

    fn seekability(&self) -> DemuxSeekability {
        self.current.seekability()
    }

    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        self.current.next_event()
    }

    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        self.seek_component_pair(request, HlsComponentSeekIntent::PreviewedExactAnchor)
    }

    fn seek_with_cancellable_preview_request(
        &mut self,
        request: DemuxSeekRequest,
        cancellation: DemuxSeekCancellationToken,
    ) -> Result<DemuxSeekResult> {
        let intent = if request.mode == media_core::DemuxSeekMode::DecodePointBefore {
            HlsComponentSeekIntent::ReceiptedManifestCandidate
        } else {
            HlsComponentSeekIntent::PreviewedExactAnchor
        };
        self.seek_cancellable_component_pair(request, intent, cancellation)
    }

    fn seek_with_receipted_request(
        &mut self,
        request: DemuxSeekRequest,
    ) -> Result<DemuxSeekResult> {
        self.seek_component_pair(request, HlsComponentSeekIntent::ReceiptedManifestCandidate)
    }

    fn seek_with_cancellable_receipted_request(
        &mut self,
        request: DemuxSeekRequest,
        cancellation: DemuxSeekCancellationToken,
    ) -> Result<DemuxSeekResult> {
        self.seek_cancellable_component_pair(
            request,
            HlsComponentSeekIntent::ReceiptedManifestCandidate,
            cancellation,
        )
    }
}

/// Выполняет обе подготовки и composition до единственной mutation active state.
fn transact_component_pair<Active, Video, Audio, Output>(
    active: &mut Active,
    prepare_video: impl FnOnce() -> Result<Video>,
    prepare_audio: impl FnOnce(&Video) -> Result<Audio>,
    compose: impl FnOnce(Video, Audio) -> Result<(Active, Output)>,
    authorize_commit: impl FnOnce() -> Result<()>,
    before_commit: impl FnOnce(&mut Output),
) -> Result<Output> {
    let video = prepare_video()?;
    let audio = prepare_audio(&video)?;
    authorize_commit()?;
    let (replacement, mut result) = compose(video, audio)?;
    before_commit(&mut result);
    *active = replacement;
    Ok(result)
}

fn exactly_one_track(tracks: &[TrackInfo], kind: TrackKind, label: &str) -> Result<TrackId> {
    let matches = tracks
        .iter()
        .filter(|track| track.kind == kind)
        .map(|track| track.id)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [track_id] => Ok(*track_id),
        _ => Err(anyhow!(
            "HLS {label} component требует ровно один {kind:?} track"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use media_core::{
        DemuxSeekCancellationCompletion, DemuxSeekCancellationToken, MediaDemuxError,
    };

    use super::transact_component_pair;

    #[derive(Debug, PartialEq, Eq)]
    struct ObservableComposite {
        public_track_ids: (u32, u32),
        next_packet_pts: u64,
    }

    #[test]
    fn failed_audio_preparation_preserves_active_tracks_and_next_packet() {
        let mut active = ObservableComposite {
            public_track_ids: (11, 12),
            next_packet_pts: 900,
        };
        let video_prepared = Cell::new(false);
        let audio_prepared = Cell::new(false);
        let compose_calls = Cell::new(0);
        let error = transact_component_pair::<_, u64, u64, ()>(
            &mut active,
            || {
                video_prepared.set(true);
                Ok(1_800)
            },
            |_| {
                audio_prepared.set(true);
                Err(anyhow::anyhow!("audio replacement failed"))
            },
            |_, _| {
                compose_calls.set(compose_calls.get() + 1);
                unreachable!("failed audio must prevent composition")
            },
            || Ok(()),
            |_| {},
        )
        .expect_err("transaction must fail");
        assert_eq!(error.to_string(), "audio replacement failed");
        assert!(video_prepared.get());
        assert!(audio_prepared.get());
        assert_eq!(compose_calls.get(), 0);
        assert_eq!(active.public_track_ids, (11, 12));
        assert_eq!(active.next_packet_pts, 900);
    }

    #[test]
    fn successful_pair_composes_and_commits_exactly_once() {
        let mut active = ObservableComposite {
            public_track_ids: (11, 12),
            next_packet_pts: 900,
        };
        let compose_calls = Cell::new(0);
        let commit_evidence_calls = Cell::new(0);
        let result = transact_component_pair(
            &mut active,
            || Ok(1_800_u64),
            |_| Ok(1_760_u64),
            |video_pts, audio_pts| {
                compose_calls.set(compose_calls.get() + 1);
                Ok((
                    ObservableComposite {
                        public_track_ids: (11, 12),
                        next_packet_pts: video_pts.min(audio_pts),
                    },
                    7_u8,
                ))
            },
            || Ok(()),
            |result| {
                assert_eq!(*result, 7);
                commit_evidence_calls.set(commit_evidence_calls.get() + 1);
            },
        )
        .expect("transaction commits");
        assert_eq!(result, 7);
        assert_eq!(compose_calls.get(), 1);
        assert_eq!(commit_evidence_calls.get(), 1);
        assert_eq!(active.public_track_ids, (11, 12));
        assert_eq!(active.next_packet_pts, 1_760);
    }

    #[test]
    fn cancellation_observed_by_audio_after_video_phase_preserves_active_pair() {
        let mut active = ObservableComposite {
            public_track_ids: (11, 12),
            next_packet_pts: 900,
        };
        let cancellation = DemuxSeekCancellationToken::new();
        let video_cancellation = cancellation.clone();
        let audio_cancellation = cancellation.clone();
        let compose_calls = Cell::new(0);

        let error = transact_component_pair::<_, u64, u64, ()>(
            &mut active,
            || {
                video_cancellation.cancel();
                Ok(1_800)
            },
            |_| {
                if audio_cancellation.is_cancelled() {
                    return Err(MediaDemuxError::SeekCancelled.into());
                }
                Ok(1_760)
            },
            |_, _| {
                compose_calls.set(compose_calls.get() + 1);
                unreachable!("cancelled shared token не должен допустить composition")
            },
            || unreachable!("failed audio preparation не должна доходить до commit authority"),
            |_| unreachable!("failed audio preparation не должна публиковать commit evidence"),
        )
        .expect_err("audio phase должна увидеть отмену shared token");

        assert!(matches!(
            error.downcast_ref::<MediaDemuxError>(),
            Some(MediaDemuxError::SeekCancelled)
        ));
        assert_eq!(compose_calls.get(), 0);
        assert_eq!(active.next_packet_pts, 900);
    }

    #[test]
    fn cancellation_during_audio_phase_wins_before_composite_commit() {
        let mut active = ObservableComposite {
            public_track_ids: (11, 12),
            next_packet_pts: 900,
        };
        let cancellation = DemuxSeekCancellationToken::new();
        let audio_cancellation = cancellation.clone();
        let commit_cancellation = cancellation.clone();
        let commit_checks = Cell::new(0);

        let error = transact_component_pair(
            &mut active,
            || Ok(1_800_u64),
            |_| {
                audio_cancellation.cancel();
                Ok(1_760_u64)
            },
            |video_pts, audio_pts| {
                Ok((
                    ObservableComposite {
                        public_track_ids: (11, 12),
                        next_packet_pts: video_pts.min(audio_pts),
                    },
                    (),
                ))
            },
            || {
                commit_checks.set(commit_checks.get() + 1);
                match commit_cancellation.complete() {
                    DemuxSeekCancellationCompletion::Completed => Ok(()),
                    DemuxSeekCancellationCompletion::CancellationWon => {
                        Err(MediaDemuxError::SeekCancelled.into())
                    }
                }
            },
            |_| unreachable!("cancellation должна победить до commit evidence"),
        )
        .expect_err("отмена должна победить до единственной mutation active pair");

        assert!(matches!(
            error.downcast_ref::<MediaDemuxError>(),
            Some(MediaDemuxError::SeekCancelled)
        ));
        assert_eq!(commit_checks.get(), 1);
        assert_eq!(active.next_packet_pts, 900);
    }
}
