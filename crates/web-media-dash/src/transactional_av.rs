//! Atomic separate-A/V seek поверх offside prepared component replacements.

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

use crate::component::{DashComponentDemuxer, DashComponentFactory};

/// DASH-owned wrapper, который не допускает partially committed video/audio seek.
pub(crate) struct TransactionalDashAvDemuxer {
    /// Active composed pair.
    current: CompositeAvDemuxer,
    /// Immutable video reconstruction recipe.
    video_factory: DashComponentFactory,
    /// Immutable audio reconstruction recipe.
    audio_factory: DashComponentFactory,
    /// Stable video public topology.
    video_public_tracks: Vec<TrackInfo>,
    /// Stable audio public topology.
    audio_public_tracks: Vec<TrackInfo>,
    /// Collision-free public ids.
    public_track_ids: CompositeAvPublicTrackIds,
    /// Existing bounded interleave policy.
    lead_policy: CompositeComponentLeadPolicy,
}

impl TransactionalDashAvDemuxer {
    /// Собирает initial active composite после readiness обеих required components.
    pub(crate) fn new(
        video_factory: DashComponentFactory,
        audio_factory: DashComponentFactory,
        video: DashComponentDemuxer,
        audio: DashComponentDemuxer,
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
}

impl Demuxer for TransactionalDashAvDemuxer {
    /// Возвращает current composed tracks.
    fn tracks(&self) -> &[TrackInfo] {
        self.current.tracks()
    }

    /// Возвращает exact aligned duration.
    fn duration(&self) -> Option<Duration> {
        self.current.duration()
    }

    /// Возвращает merged metadata.
    fn media_metadata(&self) -> Option<MediaMetadata> {
        self.current.media_metadata()
    }

    /// Pair seekable только потому, что обе component factories transactional.
    fn seekability(&self) -> DemuxSeekability {
        self.current.seekability()
    }

    /// Делегирует bounded interleaving current pair-у.
    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        self.current.next_event()
    }

    /// Accurate convenience seek.
    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    /// Готовит video и audio offside, затем делает единственный active swap.
    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        let video_factory = &self.video_factory;
        let audio_factory = &self.audio_factory;
        let video_public_tracks = &self.video_public_tracks;
        let audio_public_tracks = &self.audio_public_tracks;
        let public_track_ids = self.public_track_ids;
        let lead_policy = self.lead_policy;
        transact_component_pair(
            &mut self.current,
            || video_factory.prepare_seek_replacement(request, video_public_tracks),
            || {
                audio_factory.prepare_seek_replacement(
                    DemuxSeekRequest::accurate(request.timestamp),
                    audio_public_tracks,
                )
            },
            |(video, mut video_result), (audio, _audio_result)| {
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

/// Выполняет обе подготовки/composition до единственной mutation active state.
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

/// Требует ровно один selected track required kind-а.
fn exactly_one_track(tracks: &[TrackInfo], kind: TrackKind, label: &str) -> Result<TrackId> {
    let matches = tracks
        .iter()
        .filter(|track| track.kind == kind)
        .map(|track| track.id)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [track_id] => Ok(*track_id),
        _ => Err(anyhow!(
            "DASH {label} component требует ровно один {kind:?} track"
        )),
    }
}
