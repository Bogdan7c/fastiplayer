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

use super::{HlsLiveComponentDemuxer, HlsLiveComponentFactory};

/// Separate-A/V live seek коммитит только полностью готовую пару replacements.
pub(crate) struct TransactionalHlsLiveAvDemuxer {
    current: CompositeAvDemuxer,
    video_factory: HlsLiveComponentFactory,
    audio_factory: HlsLiveComponentFactory,
    video_public_tracks: Vec<TrackInfo>,
    audio_public_tracks: Vec<TrackInfo>,
    public_track_ids: CompositeAvPublicTrackIds,
    lead_policy: CompositeComponentLeadPolicy,
}

impl TransactionalHlsLiveAvDemuxer {
    pub fn new(
        video_factory: HlsLiveComponentFactory,
        audio_factory: HlsLiveComponentFactory,
        video: HlsLiveComponentDemuxer,
        audio: HlsLiveComponentDemuxer,
        lead_policy: CompositeComponentLeadPolicy,
    ) -> Result<Self> {
        let video_public_tracks = video.tracks().to_vec();
        let audio_public_tracks = audio.tracks().to_vec();
        let selection = CompositeAvTrackSelection::new(
            exactly_one_track(&video_public_tracks, TrackKind::Video, "video")?,
            exactly_one_track(&audio_public_tracks, TrackKind::Audio, "audio")?,
        );
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
}

impl Demuxer for TransactionalHlsLiveAvDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        self.current.tracks()
    }

    fn duration(&self) -> Option<Duration> {
        None
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
        let (video, mut video_result) = self
            .video_factory
            .prepare_seek_replacement(request, &self.video_public_tracks)?;
        let (audio, _) = self.audio_factory.prepare_seek_replacement(
            DemuxSeekRequest::accurate(request.timestamp),
            &self.audio_public_tracks,
        )?;
        let selection = CompositeAvTrackSelection::new(
            exactly_one_track(video.tracks(), TrackKind::Video, "replacement video")?,
            exactly_one_track(audio.tracks(), TrackKind::Audio, "replacement audio")?,
        );
        let replacement = CompositeAvDemuxer::new_with_public_track_ids(
            Box::new(video),
            Box::new(audio),
            selection,
            self.public_track_ids,
            self.lead_policy,
        )?;
        if let Some(timestamp) = &mut video_result.actual_track_timestamp {
            timestamp.track_id = self.public_track_ids.video_track_id();
        }
        self.current = replacement;
        Ok(video_result)
    }
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
            "HLS live {label} component требует ровно один {kind:?} track"
        )),
    }
}
