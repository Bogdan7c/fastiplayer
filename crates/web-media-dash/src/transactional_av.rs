//! Atomic separate-A/V seek поверх offside prepared component replacements.

use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use demux_api::{
    CompositeAvDemuxer, CompositeAvPublicTrackIds, CompositeAvTrackSelection,
    CompositeComponentLeadPolicy,
};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer, MediaMetadata,
    TrackId, TrackInfo, TrackKind,
};

use crate::component::{DashComponentDemuxer, DashComponentFactory};

/// Параллельно готовит независимые DASH video/audio components и возвращает их только парой.
///
/// Оба scoped worker-а всегда join-ятся до возврата. Ошибка или panic любой ветки не публикует
/// частично подготовленный компонент вызывающему коду.
pub(crate) fn prepare_dash_component_pair<Video, Audio>(
    prepare_video: impl FnOnce() -> Result<Video> + Send,
    prepare_audio: impl FnOnce() -> Result<Audio> + Send,
) -> Result<(Video, Audio)>
where
    Video: Send,
    Audio: Send,
{
    thread::scope(|scope| {
        let video_worker = scope.spawn(prepare_video);
        let audio_worker = scope.spawn(prepare_audio);

        // Join выполняем для обеих веток до propagation ошибки: иначе panic второй ветки
        // автоматически всплыл бы из thread::scope и обошёл наш typed error boundary.
        let video_outcome = video_worker.join();
        let audio_outcome = audio_worker.join();
        let video = video_outcome
            .map_err(|_| anyhow!("DASH video component preparation worker panicked"))?
            .context("DASH video component preparation failed")?;
        let audio = audio_outcome
            .map_err(|_| anyhow!("DASH audio component preparation worker panicked"))?
            .context("DASH audio component preparation failed")?;
        Ok((video, audio))
    })
}

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

    /// Готовит обе component напрямую у seek target-а без bootstrap старого DVR head-а.
    pub(crate) fn prepare_at(
        video_factory: DashComponentFactory,
        audio_factory: DashComponentFactory,
        stable_public_tracks: &[TrackInfo],
        request: DemuxSeekRequest,
        lead_policy: CompositeComponentLeadPolicy,
    ) -> Result<(Self, DemuxSeekResult)> {
        let video_public_tracks = exact_component_tracks(
            stable_public_tracks,
            TrackKind::Video,
            "stable public video",
        )?;
        let audio_public_tracks = exact_component_tracks(
            stable_public_tracks,
            TrackKind::Audio,
            "stable public audio",
        )?;
        let public_track_ids = CompositeAvPublicTrackIds::new(
            exactly_one_track(
                &video_public_tracks,
                TrackKind::Video,
                "stable public video",
            )?,
            exactly_one_track(
                &audio_public_tracks,
                TrackKind::Audio,
                "stable public audio",
            )?,
        );
        let ((video, mut video_result), (audio, _audio_result)) = prepare_dash_component_pair(
            || video_factory.prepare_seek_replacement(request, &video_public_tracks),
            || {
                audio_factory.prepare_seek_replacement(
                    DemuxSeekRequest::accurate(request.timestamp),
                    &audio_public_tracks,
                )
            },
        )?;
        let selection = CompositeAvTrackSelection::new(
            exactly_one_track(video.tracks(), TrackKind::Video, "prepared video")?,
            exactly_one_track(audio.tracks(), TrackKind::Audio, "prepared audio")?,
        );
        let current = CompositeAvDemuxer::new_with_public_track_ids(
            Box::new(video),
            Box::new(audio),
            selection,
            public_track_ids,
            lead_policy,
        )?;
        if let Some(timestamp) = &mut video_result.actual_track_timestamp {
            timestamp.track_id = public_track_ids.video_track_id();
        }
        Ok((
            Self {
                current,
                video_factory,
                audio_factory,
                video_public_tracks,
                audio_public_tracks,
                public_track_ids,
                lead_policy,
            },
            video_result,
        ))
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
    prepare_video: impl FnOnce() -> Result<Video> + Send,
    prepare_audio: impl FnOnce() -> Result<Audio> + Send,
    compose: impl FnOnce(Video, Audio) -> Result<(Active, Output)>,
) -> Result<Output>
where
    Video: Send,
    Audio: Send,
{
    let (video, audio) = prepare_dash_component_pair(prepare_video, prepare_audio)?;
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

/// Копирует stable topology одной component и одновременно проверяет её cardinality.
fn exact_component_tracks(
    tracks: &[TrackInfo],
    kind: TrackKind,
    label: &str,
) -> Result<Vec<TrackInfo>> {
    let component_tracks = tracks
        .iter()
        .filter(|track| track.kind == kind)
        .cloned()
        .collect::<Vec<_>>();
    exactly_one_track(&component_tracks, kind, label)?;
    Ok(component_tracks)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use super::*;

    /// Настоящая transactional boundary обязана overlap-ить обе подготовки и commit-ить один раз.
    #[test]
    fn component_pair_prepares_concurrently_before_single_commit() {
        let active_preparations = Arc::new(AtomicUsize::new(0));
        let maximum_active_preparations = Arc::new(AtomicUsize::new(0));
        let prepare = |value| {
            let active_preparations = Arc::clone(&active_preparations);
            let maximum_active_preparations = Arc::clone(&maximum_active_preparations);
            move || {
                let active = active_preparations.fetch_add(1, Ordering::SeqCst) + 1;
                maximum_active_preparations.fetch_max(active, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(25));
                active_preparations.fetch_sub(1, Ordering::SeqCst);
                Ok(value)
            }
        };
        let mut active_state = 7_u8;

        let output = transact_component_pair(
            &mut active_state,
            prepare(11_u8),
            prepare(13_u8),
            |video, audio| Ok((video + audio, "ready")),
        )
        .expect("parallel pair transaction");

        assert_eq!(maximum_active_preparations.load(Ordering::SeqCst), 2);
        assert_eq!(active_state, 24);
        assert_eq!(output, "ready");
    }
}
