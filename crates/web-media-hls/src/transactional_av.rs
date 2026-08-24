//! Transactional separate-A/V seek: active composite меняется только готовой парой replacements.

use std::time::Duration;

use anyhow::{Result, anyhow};
use demux_api::{
    CompositeAvDemuxer, CompositeAvPublicTrackIds, CompositeAvTrackSelection,
    CompositeComponentLeadPolicy,
};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer, MediaMetadata,
    TrackId, TrackInfo, TrackKind,
};

use crate::epoch_demux::{HlsComponentDemuxer, HlsComponentFactory};

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
        video: HlsComponentDemuxer,
        audio: HlsComponentDemuxer,
        lead_policy: CompositeComponentLeadPolicy,
    ) -> Result<Self> {
        let video_public_tracks = video.tracks().to_vec();
        let audio_public_tracks = audio.tracks().to_vec();
        let video_track = exactly_one_track(&video_public_tracks, TrackKind::Video, "video")?;
        let audio_track = exactly_one_track(&audio_public_tracks, TrackKind::Audio, "audio")?;
        let selection = CompositeAvTrackSelection::new(video_track, audio_track);
        let current =
            CompositeAvDemuxer::new(Box::new(video), Box::new(audio), selection, lead_policy)?;
        let public_track_ids = CompositeAvPublicTrackIds::new(
            current.public_video_track_id(),
            current.public_audio_track_id(),
        );
        Ok(Self {
            current,
            video_factory,
            audio_factory,
            video_public_tracks,
            audio_public_tracks,
            public_track_ids,
            lead_policy,
        })
    }

    /// Готовит video/audio replacements целиком и только затем меняет active pair.
    fn seek_component_pair(
        &mut self,
        request: DemuxSeekRequest,
        intent: HlsComponentSeekIntent,
    ) -> Result<DemuxSeekResult> {
        let video_factory = &self.video_factory;
        let audio_factory = &self.audio_factory;
        let video_public_tracks = &self.video_public_tracks;
        let audio_public_tracks = &self.audio_public_tracks;
        let public_track_ids = self.public_track_ids;
        let lead_policy = self.lead_policy;
        transact_component_pair(
            &mut self.current,
            || intent.prepare_component(video_factory, request, video_public_tracks),
            || {
                intent.prepare_component(
                    audio_factory,
                    DemuxSeekRequest::accurate(request.timestamp),
                    audio_public_tracks,
                )
            },
            |(video, mut video_result), (mut audio, _)| {
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
                Ok((replacement, video_result))
            },
        )
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

    fn seek_with_receipted_request(
        &mut self,
        request: DemuxSeekRequest,
    ) -> Result<DemuxSeekResult> {
        self.seek_component_pair(request, HlsComponentSeekIntent::ReceiptedManifestCandidate)
    }
}

/// Выполняет обе подготовки и composition до единственной mutation active state.
fn transact_component_pair<Active, Video, Audio, Output>(
    active: &mut Active,
    prepare_video: impl FnOnce() -> Result<Video>,
    prepare_audio: impl FnOnce() -> Result<Audio>,
    compose: impl FnOnce(Video, Audio) -> Result<(Active, Output)>,
) -> Result<Output> {
    let video = prepare_video()?;
    let audio = prepare_audio()?;
    let (replacement, result) = compose(video, audio)?;
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
            || {
                audio_prepared.set(true);
                Err(anyhow::anyhow!("audio replacement failed"))
            },
            |_, _| {
                compose_calls.set(compose_calls.get() + 1);
                unreachable!("failed audio must prevent composition")
            },
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
        let result = transact_component_pair(
            &mut active,
            || Ok(1_800_u64),
            || Ok(1_760_u64),
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
        )
        .expect("transaction commits");
        assert_eq!(result, 7);
        assert_eq!(compose_calls.get(), 1);
        assert_eq!(active.public_track_ids, (11, 12));
        assert_eq!(active.next_packet_pts, 1_760);
    }
}
