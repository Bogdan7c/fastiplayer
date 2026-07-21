use std::time::Duration;

use anyhow::Result;
use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult,
    DemuxSeekability, DemuxTrackListUpdate, Demuxer, MediaMetadata, MediaTime, Packet, TrackId,
    TrackInfo, TrackKind, TrackTimestamp,
};
use tracing::debug;

mod policy;
mod readiness;
mod track_layout;

pub use policy::{CompositeComponentLeadPolicy, CompositeComponentLeadPolicyError};

use readiness::ComponentLeadProgress;
use track_layout::{
    collision_safe_audio_track_id, composite_duration, merge_media_metadata, remapped_tracks,
    selected_track,
};

/// Сторона neutral A/V composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositeComponent {
    /// Video component владеет decode-safe seek anchor-ом.
    Video,
    /// Audio component использует audio-appropriate accurate seek.
    Audio,
}

/// Явные selected track IDs внутри двух independent component demuxer-ов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompositeAvTrackSelection {
    /// Track ID внутри video component-а до public remap.
    pub video_track_id: TrackId,
    /// Track ID внутри audio component-а до public remap.
    pub audio_track_id: TrackId,
}

impl CompositeAvTrackSelection {
    /// Создаёт selection без codec/container assumptions.
    #[must_use]
    pub const fn new(video_track_id: TrackId, audio_track_id: TrackId) -> Self {
        Self {
            video_track_id,
            audio_track_id,
        }
    }
}

/// Явные stable public IDs для compatibility boundary с уже опубликованным mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompositeAvPublicTrackIds {
    /// Public video track ID merged demuxer-а.
    pub video_track_id: TrackId,
    /// Public audio track ID merged demuxer-а.
    pub audio_track_id: TrackId,
}

impl CompositeAvPublicTrackIds {
    /// Создаёт intent value; collision проверяет composite constructor.
    #[must_use]
    pub const fn new(video_track_id: TrackId, audio_track_id: TrackId) -> Self {
        Self {
            video_track_id,
            audio_track_id,
        }
    }
}

/// Construction/track-remap failure до публикации composite runtime handle.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompositeAvDemuxerError {
    /// Explicit compatibility remap обязан оставаться collision-free.
    #[error("public video/audio track IDs сталкиваются на {track_id:?}")]
    PublicTrackIdCollision {
        /// Duplicate public ID.
        track_id: TrackId,
    },
    /// Explicit selected track отсутствует в component snapshot-е.
    #[error("{component:?} component не содержит selected track {track_id:?}")]
    SelectedTrackMissing {
        /// Component, чей snapshot нарушил selection contract.
        component: CompositeComponent,
        /// Exact inner track ID.
        track_id: TrackId,
    },
    /// Selected track имеет неверный media kind.
    #[error(
        "{component:?} selected track {track_id:?} имеет kind {actual_kind:?}, ожидался {expected_kind:?}"
    )]
    SelectedTrackKindMismatch {
        /// Component, чей selected track проверяется.
        component: CompositeComponent,
        /// Exact inner track ID.
        track_id: TrackId,
        /// Kind из component snapshot-а.
        actual_kind: TrackKind,
        /// Required A/V kind.
        expected_kind: TrackKind,
    },
}

/// Typed runtime read failure с сохранённой concrete source chain.
#[derive(Debug, thiserror::Error)]
#[error("{component:?} component demux read failed: {source}")]
pub struct CompositeComponentReadError {
    /// Component, на котором оборвался read.
    pub component: CompositeComponent,
    /// Concrete demuxer error остаётся downcastable.
    #[source]
    pub source: anyhow::Error,
}

/// Typed safety failure не позволяет удержать oversized packet в pending state.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "{component:?} component packet size {packet_bytes} превышает composite pending limit {maximum_bytes}"
)]
pub struct CompositePendingPacketTooLargeError {
    /// Component, который вернул oversized selected packet.
    pub component: CompositeComponent,
    /// Фактический размер encoded payload.
    pub packet_bytes: usize,
    /// Validated byte ceiling из component lead policy.
    pub maximum_bytes: usize,
}

/// Typed seek failure показывает, успела ли video side изменить cursor.
#[derive(Debug, thiserror::Error)]
#[error(
    "{component:?} component demux seek failed (video_seek_completed={video_seek_completed}): {source}"
)]
pub struct CompositeComponentSeekError {
    /// Component, на котором остановилась ordered seek transaction.
    pub component: CompositeComponent,
    /// `true` означает partial failure после успешного video seek-а.
    pub video_seek_completed: bool,
    /// Concrete demuxer error остаётся downcastable.
    #[source]
    pub source: anyhow::Error,
}

/// Internal pending fill result сохраняет lifecycle event ordering.
enum PendingFillOutcome {
    /// Pending packet/EOF state готов к interleave decision.
    Ready,
    /// Component пока не может выдать новый event и сообщает earliest retry.
    TemporarilyUnavailable(DemuxRetryHint),
    /// Один component обновил track list; composite публикует merged snapshot.
    TracksChanged(DemuxTrackListUpdate),
    /// Один component обновил metadata; composite публикует merged snapshot.
    MediaMetadataChanged(MediaMetadata),
}

/// Одноразовая policy, сохраняющая existing audio-before-long-video-preroll behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostSeekAudioBootstrap {
    /// Обычный timestamp interleave.
    Inactive,
    /// Первый audio packet можно выдать до раннего video decode anchor-а.
    DecodePointBeforePending,
}

impl PostSeekAudioBootstrap {
    /// Arm-ит bootstrap только когда audio landed позже video decode anchor-а.
    fn for_seek_results(
        request: DemuxSeekRequest,
        video_seek: DemuxSeekResult,
        audio_seek: DemuxSeekResult,
    ) -> Self {
        if request.mode == DemuxSeekMode::DecodePointBefore
            && audio_seek.actual_position > video_seek.actual_position
        {
            Self::DecodePointBeforePending
        } else {
            Self::Inactive
        }
    }
}

/// Neutral composite demuxer для independent selected video/audio components.
pub struct CompositeAvDemuxer {
    /// Video component за existing runtime boundary.
    video_demuxer: Box<dyn Demuxer + Send>,
    /// Audio component за existing runtime boundary.
    audio_demuxer: Box<dyn Demuxer + Send>,
    /// Stable inner IDs, которые caller выбрал до composition.
    selection: CompositeAvTrackSelection,
    /// Collision-safe public video ID.
    public_video_track_id: TrackId,
    /// Collision-safe public audio ID.
    public_audio_track_id: TrackId,
    /// Composite visible track snapshot.
    tracks: Vec<TrackInfo>,
    /// Video-primary duration semantics сохраняют existing dual-stream behavior.
    duration: Option<Duration>,
    /// Не больше одного validated pending video packet.
    pending_video_packet: Option<Packet>,
    /// Не больше одного validated pending audio packet.
    pending_audio_packet: Option<Packet>,
    /// Terminal state video component-а.
    video_eof: bool,
    /// Terminal state audio component-а.
    audio_eof: bool,
    /// Одноразовая post-seek audio policy.
    post_seek_audio_bootstrap: PostSeekAudioBootstrap,
    /// Video-primary, audio-fallback metadata snapshot.
    media_metadata: MediaMetadata,
    /// Validated timestamp/bootstrap lead policy.
    lead_policy: CompositeComponentLeadPolicy,
    /// Video-side timestamp/bootstrap progress.
    video_lead_progress: ComponentLeadProgress,
    /// Audio-side timestamp/bootstrap progress.
    audio_lead_progress: ComponentLeadProgress,
}

impl CompositeAvDemuxer {
    /// Создаёт generic A/V composition без codec/container/concrete demux knowledge.
    pub fn new(
        video_demuxer: Box<dyn Demuxer + Send>,
        audio_demuxer: Box<dyn Demuxer + Send>,
        selection: CompositeAvTrackSelection,
        lead_policy: CompositeComponentLeadPolicy,
    ) -> Result<Self, CompositeAvDemuxerError> {
        let public_track_ids = CompositeAvPublicTrackIds::new(
            selection.video_track_id,
            collision_safe_audio_track_id(selection.video_track_id, selection.audio_track_id),
        );
        Self::new_with_public_track_ids(
            video_demuxer,
            audio_demuxer,
            selection,
            public_track_ids,
            lead_policy,
        )
    }

    /// Создаёт composition с explicit stable public IDs для legacy compatibility adapter-а.
    pub fn new_with_public_track_ids(
        video_demuxer: Box<dyn Demuxer + Send>,
        audio_demuxer: Box<dyn Demuxer + Send>,
        selection: CompositeAvTrackSelection,
        public_track_ids: CompositeAvPublicTrackIds,
        lead_policy: CompositeComponentLeadPolicy,
    ) -> Result<Self, CompositeAvDemuxerError> {
        if public_track_ids.video_track_id == public_track_ids.audio_track_id {
            return Err(CompositeAvDemuxerError::PublicTrackIdCollision {
                track_id: public_track_ids.video_track_id,
            });
        }
        let video_track = selected_track(
            video_demuxer.as_ref(),
            CompositeComponent::Video,
            selection.video_track_id,
            TrackKind::Video,
        )?;
        let audio_track = selected_track(
            audio_demuxer.as_ref(),
            CompositeComponent::Audio,
            selection.audio_track_id,
            TrackKind::Audio,
        )?;
        let public_video_track_id = public_track_ids.video_track_id;
        let public_audio_track_id = public_track_ids.audio_track_id;
        let duration = composite_duration(
            &video_track,
            &audio_track,
            video_demuxer.duration(),
            audio_demuxer.duration(),
        );
        let tracks = remapped_tracks(
            video_track,
            audio_track,
            public_video_track_id,
            public_audio_track_id,
        );
        let media_metadata = merge_media_metadata(
            video_demuxer.media_metadata(),
            audio_demuxer.media_metadata(),
        );
        Ok(Self {
            video_demuxer,
            audio_demuxer,
            selection,
            public_video_track_id,
            public_audio_track_id,
            tracks,
            duration,
            pending_video_packet: None,
            pending_audio_packet: None,
            video_eof: false,
            audio_eof: false,
            post_seek_audio_bootstrap: PostSeekAudioBootstrap::Inactive,
            media_metadata,
            lead_policy,
            video_lead_progress: ComponentLeadProgress::default(),
            audio_lead_progress: ComponentLeadProgress::default(),
        })
    }

    /// Возвращает stable remapped video ID для decoder selection/diagnostics.
    #[must_use]
    pub const fn public_video_track_id(&self) -> TrackId {
        self.public_video_track_id
    }

    /// Возвращает stable remapped audio ID для decoder selection/diagnostics.
    #[must_use]
    pub const fn public_audio_track_id(&self) -> TrackId {
        self.public_audio_track_id
    }

    /// Возвращает validated future-compatible lead/read-ahead policy.
    #[must_use]
    pub const fn lead_policy(&self) -> CompositeComponentLeadPolicy {
        self.lead_policy
    }

    /// Сбрасывает только composite-owned read state перед любой seek attempt.
    fn clear_post_seek_read_state(&mut self) {
        self.pending_video_packet = None;
        self.pending_audio_packet = None;
        self.video_eof = false;
        self.audio_eof = false;
        self.post_seek_audio_bootstrap = PostSeekAudioBootstrap::Inactive;
        self.video_lead_progress = ComponentLeadProgress::default();
        self.audio_lead_progress = ComponentLeadProgress::default();
    }

    /// Читает exact selected video packet или один lifecycle event.
    fn fill_pending_video_event(&mut self) -> Result<PendingFillOutcome> {
        if self.pending_video_packet.is_some() || self.video_eof {
            return Ok(PendingFillOutcome::Ready);
        }
        loop {
            let event =
                self.video_demuxer
                    .next_event()
                    .map_err(|source| CompositeComponentReadError {
                        component: CompositeComponent::Video,
                        source,
                    })?;
            match event {
                DemuxReadEvent::Packet(packet)
                    if packet.track_id == self.selection.video_track_id =>
                {
                    self.store_pending_packet(
                        CompositeComponent::Video,
                        packet.with_track_id(self.public_video_track_id),
                    )?;
                    return Ok(PendingFillOutcome::Ready);
                }
                DemuxReadEvent::Packet(_) => continue,
                DemuxReadEvent::EndOfStream => {
                    self.video_eof = true;
                    return Ok(PendingFillOutcome::Ready);
                }
                DemuxReadEvent::TemporarilyUnavailable(hint) => {
                    return Ok(PendingFillOutcome::TemporarilyUnavailable(hint));
                }
                DemuxReadEvent::TracksChanged(_) => return self.refresh_tracks_after_inner_reset(),
                DemuxReadEvent::MediaMetadataChanged(_) => {
                    return Ok(self.refresh_media_metadata());
                }
            }
        }
    }

    /// Читает exact selected audio packet или один lifecycle event.
    fn fill_pending_audio_event(&mut self) -> Result<PendingFillOutcome> {
        if self.pending_audio_packet.is_some() || self.audio_eof {
            return Ok(PendingFillOutcome::Ready);
        }
        loop {
            let event =
                self.audio_demuxer
                    .next_event()
                    .map_err(|source| CompositeComponentReadError {
                        component: CompositeComponent::Audio,
                        source,
                    })?;
            match event {
                DemuxReadEvent::Packet(packet)
                    if packet.track_id == self.selection.audio_track_id =>
                {
                    self.store_pending_packet(
                        CompositeComponent::Audio,
                        packet.with_track_id(self.public_audio_track_id),
                    )?;
                    return Ok(PendingFillOutcome::Ready);
                }
                DemuxReadEvent::Packet(_) => continue,
                DemuxReadEvent::EndOfStream => {
                    self.audio_eof = true;
                    return Ok(PendingFillOutcome::Ready);
                }
                DemuxReadEvent::TemporarilyUnavailable(hint) => {
                    return Ok(PendingFillOutcome::TemporarilyUnavailable(hint));
                }
                DemuxReadEvent::TracksChanged(_) => return self.refresh_tracks_after_inner_reset(),
                DemuxReadEvent::MediaMetadataChanged(_) => {
                    return Ok(self.refresh_media_metadata());
                }
            }
        }
    }

    /// Revalidates explicit inner IDs и сохраняет stable public remap.
    fn refresh_tracks_after_inner_reset(&mut self) -> Result<PendingFillOutcome> {
        let video_track = selected_track(
            self.video_demuxer.as_ref(),
            CompositeComponent::Video,
            self.selection.video_track_id,
            TrackKind::Video,
        )?;
        let audio_track = selected_track(
            self.audio_demuxer.as_ref(),
            CompositeComponent::Audio,
            self.selection.audio_track_id,
            TrackKind::Audio,
        )?;
        self.duration = composite_duration(
            &video_track,
            &audio_track,
            self.video_demuxer.duration(),
            self.audio_demuxer.duration(),
        );
        self.tracks = remapped_tracks(
            video_track,
            audio_track,
            self.public_video_track_id,
            self.public_audio_track_id,
        );
        self.pending_video_packet = None;
        self.pending_audio_packet = None;
        self.post_seek_audio_bootstrap = PostSeekAudioBootstrap::Inactive;
        self.video_lead_progress = ComponentLeadProgress::default();
        self.audio_lead_progress = ComponentLeadProgress::default();
        Ok(PendingFillOutcome::TracksChanged(
            DemuxTrackListUpdate::new(self.tracks.clone(), self.duration),
        ))
    }

    /// Rebuilds video-primary/audio-fallback metadata snapshot.
    fn refresh_media_metadata(&mut self) -> PendingFillOutcome {
        self.media_metadata = merge_media_metadata(
            self.video_demuxer.media_metadata(),
            self.audio_demuxer.media_metadata(),
        );
        PendingFillOutcome::MediaMetadataChanged(self.media_metadata.clone())
    }

    /// Выдаёт первый post-seek audio packet до раннего video preroll ровно один раз.
    fn take_post_seek_audio_bootstrap_packet(&mut self) -> Option<Packet> {
        if self.post_seek_audio_bootstrap == PostSeekAudioBootstrap::Inactive {
            return None;
        }
        let packet = self.pending_audio_packet.take()?;
        self.post_seek_audio_bootstrap = PostSeekAudioBootstrap::Inactive;
        Some(packet)
    }

    /// Собирает один seek result merged timeline с stable public track IDs.
    fn composite_seek_result(
        &self,
        request: DemuxSeekRequest,
        video_seek: DemuxSeekResult,
        audio_seek: DemuxSeekResult,
    ) -> DemuxSeekResult {
        let video_seek = remap_seek_result_track(video_seek, self.public_video_track_id);
        let audio_seek = remap_seek_result_track(audio_seek, self.public_audio_track_id);
        let composite_seek = match request.mode {
            DemuxSeekMode::DecodePointBefore => video_seek,
            DemuxSeekMode::Accurate | DemuxSeekMode::Preview => {
                earliest_stream_seek_result(video_seek, audio_seek)
            }
        };
        if video_seek.actual_position != audio_seek.actual_position {
            debug!(
                mode = ?request.mode,
                requested_ms = request.timestamp.as_millis(),
                video_actual_ms = video_seek.actual_position.as_duration().as_millis(),
                audio_actual_ms = audio_seek.actual_position.as_duration().as_millis(),
                composite_actual_ms = composite_seek.actual_position.as_duration().as_millis(),
                "Composite A/V seek вернул разные component positions"
            );
        }
        DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: composite_seek.actual_position,
            actual_track_timestamp: composite_seek.actual_track_timestamp,
        }
    }
}

impl Demuxer for CompositeAvDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn media_metadata(&self) -> Option<MediaMetadata> {
        Some(self.media_metadata.clone())
    }

    fn seekability(&self) -> DemuxSeekability {
        match (
            self.video_demuxer.seekability(),
            self.audio_demuxer.seekability(),
        ) {
            (DemuxSeekability::Seekable, DemuxSeekability::Seekable) => DemuxSeekability::Seekable,
            (DemuxSeekability::NotSeekable { reason }, _) => {
                DemuxSeekability::NotSeekable { reason }
            }
            (_, DemuxSeekability::NotSeekable { reason }) => {
                DemuxSeekability::NotSeekable { reason }
            }
        }
    }

    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        self.read_next_composite_event()
    }

    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        self.clear_post_seek_read_state();
        let video_seek = self
            .video_demuxer
            .seek_with_request(request)
            .map_err(|source| CompositeComponentSeekError {
                component: CompositeComponent::Video,
                video_seek_completed: false,
                source,
            })?;
        let audio_request = audio_seek_request(request);
        let audio_seek = self
            .audio_demuxer
            .seek_with_request(audio_request)
            .map_err(|source| CompositeComponentSeekError {
                component: CompositeComponent::Audio,
                video_seek_completed: true,
                source,
            })?;
        self.post_seek_audio_bootstrap =
            PostSeekAudioBootstrap::for_seek_results(request, video_seek, audio_seek);
        Ok(self.composite_seek_result(request, video_seek, audio_seek))
    }
}

/// Remap-ит raw seek timestamp на stable public track ID.
fn remap_seek_result_track(
    mut seek_result: DemuxSeekResult,
    public_track_id: TrackId,
) -> DemuxSeekResult {
    seek_result.actual_track_timestamp =
        seek_result
            .actual_track_timestamp
            .map(|actual_track_timestamp| TrackTimestamp {
                track_id: public_track_id,
                ..actual_track_timestamp
            });
    seek_result
}

/// Accurate/preview composite начинает timeline с earliest actual component position.
fn earliest_stream_seek_result(
    video_seek: DemuxSeekResult,
    audio_seek: DemuxSeekResult,
) -> DemuxSeekResult {
    if audio_seek.actual_position < video_seek.actual_position {
        audio_seek
    } else {
        video_seek
    }
}

/// Decode-point strictness принадлежит video; audio сохраняет packet granularity.
fn audio_seek_request(request: DemuxSeekRequest) -> DemuxSeekRequest {
    match request.mode {
        DemuxSeekMode::DecodePointBefore => DemuxSeekRequest::accurate(request.timestamp),
        DemuxSeekMode::Accurate | DemuxSeekMode::Preview => request,
    }
}

#[cfg(test)]
mod tests;
