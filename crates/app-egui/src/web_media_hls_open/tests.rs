use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::time::Duration;

use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekResult, DemuxSeekability, DemuxTrackListUpdate,
    Demuxer, MediaMetadata, Packet, TrackId, TrackInfo, TrackKind,
};
use web_media_adaptive::AdaptiveTransportLimits;

use super::{
    ContainerFamily, HlsContainerEvidence, HlsInitialReadinessCapability, HlsVodSeekLandingPolicy,
    hls_main_container_evidence, hls_policy, wait_for_initial_hls_tracks,
};

/// Минимальный event-ordered demuxer моделирует deferred HLS publication.
struct ScriptedDemuxer {
    /// Worker events становятся видимыми owner-у строго по одному.
    events: VecDeque<DemuxReadEvent>,
    /// До TracksChanged список пуст, после него содержит authoritative tracks.
    tracks: Vec<TrackInfo>,
    /// До TracksChanged duration неизвестна, после него становится authoritative.
    duration: Option<Duration>,
}

impl Demuxer for ScriptedDemuxer {
    /// Возвращает только уже опубликованный track snapshot.
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    /// Возвращает только уже опубликованную длительность.
    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// HLS VOD seek boundary существует ещё до публикации track metadata.
    fn seekability(&self) -> DemuxSeekability {
        DemuxSeekability::Seekable
    }

    /// Применяет lifecycle snapshot одновременно с возвратом TracksChanged.
    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        let event = self
            .events
            .pop_front()
            .unwrap_or(DemuxReadEvent::EndOfStream);
        if let DemuxReadEvent::TracksChanged(update) = &event {
            self.tracks.clone_from(&update.tracks);
            self.duration = update.duration;
        }
        Ok(event)
    }

    /// Этот fake не выполняет seek: тест проверяет install-ready boundary.
    fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        panic!("scripted demuxer seek не должен вызываться в readiness test")
    }
}

fn track(id: u32, kind: TrackKind) -> TrackInfo {
    TrackInfo {
        id: TrackId::new(id),
        kind,
        codec_id: "test".into(),
        codec_private: None,
        time_base: None,
        duration: None,
        sample_rate: None,
        channels: None,
        video: None,
    }
}

#[test]
fn wait_for_initial_tracks_skips_metadata_before_topology() {
    let published = vec![track(1, TrackKind::Video), track(2, TrackKind::Audio)];
    let mut demuxer = ScriptedDemuxer {
        events: VecDeque::from([
            DemuxReadEvent::MediaMetadataChanged(MediaMetadata::default()),
            DemuxReadEvent::TracksChanged(DemuxTrackListUpdate::new(published.clone(), None)),
        ]),
        tracks: Vec::new(),
        duration: None,
    };

    wait_for_initial_hls_tracks(
        &mut demuxer,
        &HlsInitialReadinessCapability::AlreadySynchronous,
    )
    .expect("TracksChanged after metadata");
    assert_eq!(demuxer.tracks().len(), 2);
}

#[test]
fn wait_for_initial_tracks_rejects_unavailable_synchronous_runtime() {
    let mut demuxer = ScriptedDemuxer {
        events: VecDeque::from([DemuxReadEvent::TemporarilyUnavailable(
            DemuxRetryHint::new(Duration::from_millis(1)).expect("retry hint"),
        )]),
        tracks: Vec::new(),
        duration: None,
    };

    let error = wait_for_initial_hls_tracks(
        &mut demuxer,
        &HlsInitialReadinessCapability::AlreadySynchronous,
    )
    .expect_err("synchronous capability не должна скрывать deferred queue");

    assert!(error.to_string().contains("синхронный HLS runtime"));
}

#[test]
fn wait_for_initial_tracks_accepts_bootstrapped_snapshot_without_consuming_packet() {
    let published = vec![track(1, TrackKind::Video)];
    let mut demuxer = ScriptedDemuxer {
        events: VecDeque::from([DemuxReadEvent::Packet(Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Video,
            Duration::ZERO,
            None,
            true,
            Default::default(),
        ))]),
        tracks: published,
        duration: None,
    };

    wait_for_initial_hls_tracks(
        &mut demuxer,
        &HlsInitialReadinessCapability::AlreadySynchronous,
    )
    .expect("готовый bootstrap snapshot уже является install-ready");

    assert_eq!(demuxer.events.len(), 1);
    assert_eq!(demuxer.tracks().len(), 1);
}

#[test]
fn wait_for_initial_tracks_rejects_packet_before_topology() {
    let mut demuxer = ScriptedDemuxer {
        events: VecDeque::from([DemuxReadEvent::Packet(Packet::new_unbounded(
            TrackId::new(1),
            TrackKind::Video,
            Duration::ZERO,
            None,
            true,
            Default::default(),
        ))]),
        tracks: Vec::new(),
        duration: None,
    };

    let error = wait_for_initial_hls_tracks(
        &mut demuxer,
        &HlsInitialReadinessCapability::AlreadySynchronous,
    )
    .expect_err("packet before track topology должен быть отвергнут");

    assert!(error.to_string().contains("packet"));
    assert!(demuxer.tracks().is_empty());
}

#[test]
fn wait_for_initial_tracks_rejects_eos_before_tracks() {
    let mut demuxer = ScriptedDemuxer {
        events: VecDeque::from([DemuxReadEvent::EndOfStream]),
        tracks: Vec::new(),
        duration: None,
    };
    let error = wait_for_initial_hls_tracks(
        &mut demuxer,
        &HlsInitialReadinessCapability::AlreadySynchronous,
    )
    .expect_err("EOS before tracks");
    assert!(error.to_string().contains("EOS"));
}

/// Regression: restart restore идёт сразу после Installed и не ждёт player tick.
#[test]
fn hls_vod_is_seekable_with_duration_at_prepared_media_boundary() {
    // Acceptance row 01 имеет конечную десятиминутную timeline и declared codecs.
    let published_duration = Duration::from_secs(634);
    // До worker event-а metadata намеренно ещё не видна app owner-у.
    let mut demuxer = ScriptedDemuxer {
        events: VecDeque::from([DemuxReadEvent::TracksChanged(DemuxTrackListUpdate::new(
            vec![track(1, TrackKind::Video), track(2, TrackKind::Audio)],
            Some(published_duration),
        ))]),
        tracks: Vec::new(),
        duration: None,
    };

    // Production HLS VOD preparation обязана пересечь readiness до PreparedMedia.
    wait_for_initial_hls_tracks(
        &mut demuxer,
        &HlsInitialReadinessCapability::AlreadySynchronous,
    )
    .expect("HLS VOD install readiness");
    // PreparedMedia снимает immutable snapshot, который немедленно увидит player install.
    let prepared =
        player_core::PreparedMedia::from_external_label("HLS VOD acceptance", Box::new(demuxer));

    // Restore получает tracks без ожидания дополнительного player tick.
    assert_eq!(prepared.tracks().len(), 2);
    // Известная duration создаёт static seekable timeline при atomic install.
    assert_eq!(prepared.duration(), Some(published_duration));
    // Demux seekability snapshot не деградирует во время metadata publication.
    assert_eq!(prepared.seekability(), DemuxSeekability::Seekable);
}

/// Generic MP4 output hint не должен подменять content proof реального HLS segment container.
#[test]
fn generic_iso_bmff_hls_hint_requires_segment_content_probe() {
    let evidence = hls_main_container_evidence(ContainerFamily::IsoBmff)
        .expect("generic ISO-BMFF входит в поддержанный HLS profile");

    assert!(matches!(evidence, HlsContainerEvidence::ContentProbe));
}

/// Общая yt-dlp/live composition policy не должна получать native-only landing opt-in.
#[test]
fn shared_ytdlp_and_live_hls_policy_keeps_decode_forward_default() {
    let limits = AdaptiveTransportLimits::new(
        NonZeroUsize::new(64 * 1_024).expect("manifest byte bound"),
        NonZeroUsize::new(2 * 1_024 * 1_024).expect("segment byte bound"),
        NonZeroUsize::new(64).expect("descriptor byte bound"),
    );
    let policy = hls_policy(limits).expect("production shared HLS policy");
    assert_eq!(
        policy.seek_landing_policy,
        HlsVodSeekLandingPolicy::DecodeFromOrBeforeTarget
    );
}
