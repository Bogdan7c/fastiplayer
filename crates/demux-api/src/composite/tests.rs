use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult,
    DemuxSeekability, DemuxTrackListUpdate, Demuxer, DiscNumber, MediaContainerMetadata,
    MediaMetadata, MediaTagMetadata, MediaTime, Packet, TrackId, TrackInfo, TrackKind, TrackNumber,
    TvEpisodeNumber, TvSeasonNumber,
};

use super::{
    CompositeAvDemuxer, CompositeAvTrackSelection, CompositeComponent,
    CompositeComponentLeadPolicy, CompositeComponentLeadPolicyError, CompositeComponentSeekError,
    CompositePendingPacketTooLargeError,
};

/// Scripted component поддерживает lifecycle/seek tests без concrete container backend-а.
struct ScriptedDemuxer {
    tracks: Vec<TrackInfo>,
    duration: Option<Duration>,
    events: VecDeque<DemuxReadEvent>,
    seek_behavior: SeekBehavior,
    seek_log: Arc<Mutex<Vec<DemuxSeekRequest>>>,
}

/// Seek outcome позволяет моделировать partial composite failure.
enum SeekBehavior {
    /// Успешное приземление на заданную actual position.
    Success(MediaTime),
    /// Typed backend error через existing anyhow runtime signature.
    Fail(&'static str),
}

/// Shared seek call log сохраняет читаемые сигнатуры test fixtures.
type SeekRequestLog = Arc<Mutex<Vec<DemuxSeekRequest>>>;

/// Composite fixture возвращает demuxer и отдельные component seek logs.
type CompositeFixture = (CompositeAvDemuxer, SeekRequestLog, SeekRequestLog);

impl ScriptedDemuxer {
    fn new(
        tracks: Vec<TrackInfo>,
        duration: Option<Duration>,
        events: VecDeque<DemuxReadEvent>,
        seek_behavior: SeekBehavior,
        seek_log: Arc<Mutex<Vec<DemuxSeekRequest>>>,
    ) -> Self {
        Self {
            tracks,
            duration,
            events,
            seek_behavior,
            seek_log,
        }
    }
}

impl Demuxer for ScriptedDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        let event = self
            .events
            .pop_front()
            .unwrap_or(DemuxReadEvent::EndOfStream);
        if let DemuxReadEvent::TracksChanged(update) = &event {
            self.tracks = update.tracks.clone();
            self.duration = update.duration;
        }
        Ok(event)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.seek_log.lock().expect("seek log").push(request);
        match self.seek_behavior {
            SeekBehavior::Success(actual_position) => Ok(DemuxSeekResult {
                requested_position: MediaTime::from_duration(request.timestamp),
                actual_position,
                actual_track_timestamp: None,
            }),
            SeekBehavior::Fail(reason) => anyhow::bail!(reason),
        }
    }
}

fn track(id: u32, kind: TrackKind, codec_id: &str, duration_ms: u64) -> TrackInfo {
    TrackInfo {
        id: TrackId::new(id),
        kind,
        codec_id: codec_id.to_owned(),
        codec_private: None,
        time_base: None,
        duration: Some(Duration::from_millis(duration_ms)),
        sample_rate: (kind == TrackKind::Audio).then_some(48_000),
        channels: (kind == TrackKind::Audio).then_some(2),
        video: None,
    }
}

fn packet(id: u32, kind: TrackKind, pts_ms: u64, marker: &'static [u8]) -> Packet {
    Packet::new_unbounded(
        TrackId::new(id),
        kind,
        Duration::from_millis(pts_ms),
        None,
        kind == TrackKind::Video,
        Bytes::from_static(marker),
    )
}

/// Строит validated retry hint без повторения bounds boilerplate в сценариях.
fn retry_hint(retry_after_ms: u64) -> DemuxRetryHint {
    DemuxRetryHint::new(Duration::from_millis(retry_after_ms))
        .expect("focused retry delay должен быть допустим")
}

fn lead_policy() -> CompositeComponentLeadPolicy {
    CompositeComponentLeadPolicy::single_pending_packet(
        Duration::from_secs(5),
        NonZeroUsize::new(1024 * 1024).expect("non-zero byte cap"),
    )
    .expect("valid lead policy")
}

fn composite(
    video_tracks: Vec<TrackInfo>,
    audio_tracks: Vec<TrackInfo>,
    video_events: VecDeque<DemuxReadEvent>,
    audio_events: VecDeque<DemuxReadEvent>,
    video_seek: SeekBehavior,
    audio_seek: SeekBehavior,
) -> CompositeFixture {
    let video_track_id = video_tracks[0].id;
    let audio_track_id = audio_tracks[0].id;
    let video_seek_log = Arc::new(Mutex::new(Vec::new()));
    let audio_seek_log = Arc::new(Mutex::new(Vec::new()));
    let video_demuxer = ScriptedDemuxer::new(
        video_tracks,
        Some(Duration::from_secs(10)),
        video_events,
        video_seek,
        Arc::clone(&video_seek_log),
    );
    let audio_demuxer = ScriptedDemuxer::new(
        audio_tracks,
        Some(Duration::from_secs(10)),
        audio_events,
        audio_seek,
        Arc::clone(&audio_seek_log),
    );
    let composite = CompositeAvDemuxer::new(
        Box::new(video_demuxer),
        Box::new(audio_demuxer),
        CompositeAvTrackSelection::new(video_track_id, audio_track_id),
        lead_policy(),
    )
    .expect("composite opens");
    (composite, video_seek_log, audio_seek_log)
}

/// Typed policy отвергает unbounded lead и хранит exact named caps.
#[test]
fn lead_policy_is_typed_and_bounded() {
    let policy = lead_policy();
    assert_eq!(policy.max_timestamp_lead(), Duration::from_secs(5));
    assert_eq!(policy.bootstrap_packet_limit(), 1);
    assert_eq!(policy.bootstrap_byte_limit(), 1024 * 1024);
    let zero_lead = CompositeComponentLeadPolicy::single_pending_packet(
        Duration::ZERO,
        NonZeroUsize::new(1).expect("non-zero byte cap"),
    )
    .expect_err("zero lead");
    assert_eq!(
        zero_lead,
        CompositeComponentLeadPolicyError::ZeroTimestampLead
    );
}

/// Separate MP4/M4A-like H.264+AAC components не зависят от VP9/WebM knowledge.
#[test]
fn separate_h264_aac_components_interleave_with_collision_safe_ids() {
    let video_events = VecDeque::from([
        DemuxReadEvent::Packet(packet(7, TrackKind::Video, 40, b"video")),
        DemuxReadEvent::EndOfStream,
    ]);
    let audio_events = VecDeque::from([
        DemuxReadEvent::Packet(packet(7, TrackKind::Audio, 20, b"audio")),
        DemuxReadEvent::EndOfStream,
    ]);
    let (mut composite, _, _) = composite(
        vec![track(7, TrackKind::Video, "V_MPEG4/ISO/AVC", 10_000)],
        vec![track(7, TrackKind::Audio, "mp4a", 9_500)],
        video_events,
        audio_events,
        SeekBehavior::Success(MediaTime::ZERO),
        SeekBehavior::Success(MediaTime::ZERO),
    );
    assert_eq!(composite.public_video_track_id(), TrackId::new(7));
    assert_eq!(composite.public_audio_track_id(), TrackId::new(8));
    assert_eq!(composite.tracks()[0].codec_id, "V_MPEG4/ISO/AVC");
    assert_eq!(composite.tracks()[1].codec_id, "mp4a");

    let first = composite.next_event().expect("first event");
    let second = composite.next_event().expect("second event");
    assert!(matches!(
        first,
        DemuxReadEvent::Packet(ref packet)
            if packet.kind == TrackKind::Audio && packet.track_id == TrackId::new(8)
    ));
    assert!(matches!(
        second,
        DemuxReadEvent::Packet(ref packet)
            if packet.kind == TrackKind::Video && packet.track_id == TrackId::new(7)
    ));
}

/// EOF одной стороны не завершает composite, пока другая сторона выдаёт packets.
#[test]
fn one_side_eof_keeps_other_component_readable() {
    let video_events = VecDeque::from([DemuxReadEvent::EndOfStream]);
    let audio_events = VecDeque::from([
        DemuxReadEvent::Packet(packet(2, TrackKind::Audio, 10, b"a1")),
        DemuxReadEvent::Packet(packet(2, TrackKind::Audio, 20, b"a2")),
        DemuxReadEvent::EndOfStream,
    ]);
    let (mut composite, _, _) = composite(
        vec![track(1, TrackKind::Video, "vp09", 10_000)],
        vec![track(2, TrackKind::Audio, "opus", 10_000)],
        video_events,
        audio_events,
        SeekBehavior::Success(MediaTime::ZERO),
        SeekBehavior::Success(MediaTime::ZERO),
    );
    assert!(matches!(
        composite.next_event().expect("audio packet one"),
        DemuxReadEvent::Packet(_)
    ));
    assert!(matches!(
        composite.next_event().expect("audio packet two"),
        DemuxReadEvent::Packet(_)
    ));
    assert!(matches!(
        composite.next_event().expect("terminal EOF"),
        DemuxReadEvent::EndOfStream
    ));
}

/// DecodePointBefore остаётся video-only strict, audio получает Accurate request.
#[test]
fn decode_point_seek_preserves_video_anchor_and_audio_accuracy() {
    let video_actual = MediaTime::from_duration(Duration::from_secs(4));
    let audio_actual = MediaTime::from_duration(Duration::from_secs(5));
    let (mut composite, video_seek_log, audio_seek_log) = composite(
        vec![track(1, TrackKind::Video, "vp09", 10_000)],
        vec![track(2, TrackKind::Audio, "opus", 10_000)],
        VecDeque::new(),
        VecDeque::new(),
        SeekBehavior::Success(video_actual),
        SeekBehavior::Success(audio_actual),
    );
    let request = DemuxSeekRequest::decode_point_before(Duration::from_secs(5));
    let result = composite
        .seek_with_request(request)
        .expect("composite seek");
    assert_eq!(result.actual_position, video_actual);
    assert_eq!(
        video_seek_log.lock().expect("video log")[0].mode,
        DemuxSeekMode::DecodePointBefore
    );
    assert_eq!(
        audio_seek_log.lock().expect("audio log")[0].mode,
        DemuxSeekMode::Accurate
    );
}

/// Audio failure после successful video seek остаётся typed partial failure.
#[test]
fn seek_partial_failure_reports_component_and_completed_video() {
    let (mut composite, _, _) = composite(
        vec![track(1, TrackKind::Video, "avc1", 10_000)],
        vec![track(2, TrackKind::Audio, "mp4a", 10_000)],
        VecDeque::new(),
        VecDeque::new(),
        SeekBehavior::Success(MediaTime::ZERO),
        SeekBehavior::Fail("audio seek failed"),
    );
    let error = composite
        .seek(Duration::from_secs(3))
        .expect_err("partial seek must fail");
    let typed = error
        .downcast_ref::<CompositeComponentSeekError>()
        .expect("typed component seek error");
    assert_eq!(typed.component, CompositeComponent::Audio);
    assert!(typed.video_seek_completed);
}

/// Inner TracksChanged перестраивает snapshot, но public collision remap остаётся stable.
#[test]
fn tracks_changed_keeps_public_track_mapping_stable() {
    let changed_video_tracks = vec![track(9, TrackKind::Video, "avc1-updated", 12_000)];
    let video_events = VecDeque::from([DemuxReadEvent::TracksChanged(DemuxTrackListUpdate::new(
        changed_video_tracks,
        Some(Duration::from_secs(12)),
    ))]);
    let audio_events = VecDeque::from([DemuxReadEvent::EndOfStream]);
    let (mut composite, _, _) = composite(
        vec![track(9, TrackKind::Video, "avc1", 10_000)],
        vec![track(9, TrackKind::Audio, "mp4a", 10_000)],
        video_events,
        audio_events,
        SeekBehavior::Success(MediaTime::ZERO),
        SeekBehavior::Success(MediaTime::ZERO),
    );
    let event = composite.next_event().expect("tracks changed event");
    let DemuxReadEvent::TracksChanged(update) = event else {
        panic!("expected TracksChanged");
    };
    assert_eq!(update.tracks[0].id, TrackId::new(9));
    assert_eq!(update.tracks[1].id, TrackId::new(10));
    assert_eq!(update.tracks[0].codec_id, "avc1-updated");
}

/// Composite seekability требует seekability обеих обязательных component-ов.
#[test]
fn component_seekability_is_conjunctive() {
    struct UnseekableScriptedDemuxer(ScriptedDemuxer);
    impl Demuxer for UnseekableScriptedDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            self.0.tracks()
        }
        fn duration(&self) -> Option<Duration> {
            self.0.duration()
        }
        fn seekability(&self) -> DemuxSeekability {
            DemuxSeekability::NotSeekable {
                reason: media_core::TimelineNotSeekableReason::SourceNotSeekable,
            }
        }
        fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
            self.0.next_event()
        }
        fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            self.0.seek(timestamp)
        }
    }

    let log = Arc::new(Mutex::new(Vec::new()));
    let video = ScriptedDemuxer::new(
        vec![track(1, TrackKind::Video, "vp09", 10_000)],
        Some(Duration::from_secs(10)),
        VecDeque::new(),
        SeekBehavior::Success(MediaTime::ZERO),
        Arc::clone(&log),
    );
    let audio = UnseekableScriptedDemuxer(ScriptedDemuxer::new(
        vec![track(2, TrackKind::Audio, "opus", 10_000)],
        Some(Duration::from_secs(10)),
        VecDeque::new(),
        SeekBehavior::Success(MediaTime::ZERO),
        log,
    ));
    let composite = CompositeAvDemuxer::new(
        Box::new(video),
        Box::new(audio),
        CompositeAvTrackSelection::new(TrackId::new(1), TrackId::new(2)),
        lead_policy(),
    )
    .expect("composite opens");
    assert!(matches!(
        composite.seekability(),
        DemuxSeekability::NotSeekable { .. }
    ));
}

/// Video metadata остаётся primary, а audio заполняет только отсутствующие поля.
#[test]
fn metadata_merge_preserves_video_precedence_and_audio_fallbacks() {
    let video_metadata = MediaMetadata {
        container: Some(MediaContainerMetadata {
            format_name: Some("video/webm".into()),
        }),
        tags: MediaTagMetadata {
            title: Some("Video title".into()),
            disc_number: Some(DiscNumber::new(1)),
            tv_season_number: Some(TvSeasonNumber::new(2)),
            ..Default::default()
        },
    };
    let audio_metadata = MediaMetadata {
        container: Some(MediaContainerMetadata {
            format_name: Some("audio/webm".into()),
        }),
        tags: MediaTagMetadata {
            title: Some("Audio title".into()),
            artists: vec!["Audio artist".into()],
            album: Some("Audio album".into()),
            disc_number: Some(DiscNumber::new(99)),
            track_number: Some(TrackNumber::new(7)),
            tv_season_number: Some(TvSeasonNumber::new(99)),
            tv_episode_number: Some(TvEpisodeNumber::new(5)),
        },
    };

    let merged_metadata = super::merge_media_metadata(Some(video_metadata), Some(audio_metadata));

    assert_eq!(
        merged_metadata
            .container
            .and_then(|container| container.format_name),
        Some("video/webm".into())
    );
    assert_eq!(merged_metadata.tags.title.as_deref(), Some("Video title"));
    assert_eq!(merged_metadata.tags.artists, ["Audio artist"]);
    assert_eq!(merged_metadata.tags.album.as_deref(), Some("Audio album"));
    assert_eq!(merged_metadata.tags.disc_number, Some(DiscNumber::new(1)));
    assert_eq!(merged_metadata.tags.track_number, Some(TrackNumber::new(7)));
    assert_eq!(
        merged_metadata.tags.tv_season_number,
        Some(TvSeasonNumber::new(2))
    );
    assert_eq!(
        merged_metadata.tags.tv_episode_number,
        Some(TvEpisodeNumber::new(5))
    );
}

#[test]
fn both_unavailable_components_return_minimum_earliest_retry() {
    let video_events = VecDeque::from([
        DemuxReadEvent::TemporarilyUnavailable(retry_hint(50)),
        DemuxReadEvent::EndOfStream,
    ]);
    let audio_events = VecDeque::from([
        DemuxReadEvent::TemporarilyUnavailable(retry_hint(20)),
        DemuxReadEvent::EndOfStream,
    ]);
    let (mut composite, _, _) = composite(
        vec![track(11, TrackKind::Video, "V_H264", 10_000)],
        vec![track(22, TrackKind::Audio, "A_AAC", 10_000)],
        video_events,
        audio_events,
        SeekBehavior::Success(MediaTime::ZERO),
        SeekBehavior::Success(MediaTime::ZERO),
    );

    let event = composite
        .next_event()
        .expect("temporary readiness не должна становиться error");

    assert_eq!(
        event,
        DemuxReadEvent::TemporarilyUnavailable(retry_hint(20))
    );
    assert!(!composite.video_eof);
    assert!(!composite.audio_eof);
}

#[test]
fn bootstrap_starvation_reaches_cap_and_recovers_in_event_order() {
    let video_events = VecDeque::from([
        DemuxReadEvent::Packet(packet(11, TrackKind::Video, 0, b"v0")),
        DemuxReadEvent::Packet(packet(11, TrackKind::Video, 40, b"v40")),
        DemuxReadEvent::Packet(packet(11, TrackKind::Video, 80, b"v80")),
        DemuxReadEvent::EndOfStream,
    ]);
    let audio_events = VecDeque::from([
        DemuxReadEvent::TemporarilyUnavailable(retry_hint(10)),
        DemuxReadEvent::TemporarilyUnavailable(retry_hint(10)),
        DemuxReadEvent::TemporarilyUnavailable(retry_hint(10)),
        DemuxReadEvent::Packet(packet(22, TrackKind::Audio, 0, b"a0")),
        DemuxReadEvent::Packet(packet(22, TrackKind::Audio, 40, b"a40")),
        DemuxReadEvent::Packet(packet(22, TrackKind::Audio, 80, b"a80")),
        DemuxReadEvent::EndOfStream,
    ]);
    let (mut composite, _, _) = composite(
        vec![track(11, TrackKind::Video, "V_H264", 10_000)],
        vec![track(22, TrackKind::Audio, "A_AAC", 10_000)],
        video_events,
        audio_events,
        SeekBehavior::Success(MediaTime::ZERO),
        SeekBehavior::Success(MediaTime::ZERO),
    );

    let first_video = composite
        .next_event()
        .expect("один bootstrap packet разрешён policy");
    assert!(matches!(
        first_video,
        DemuxReadEvent::Packet(ref packet) if packet.data.as_ref() == b"v0"
    ));

    let first_capped_retry = composite
        .next_event()
        .expect("достижение cap должно быть readiness, а не error");
    let second_capped_retry = composite
        .next_event()
        .expect("долгая starvation должна сохранять readiness");
    assert_eq!(
        first_capped_retry,
        DemuxReadEvent::TemporarilyUnavailable(retry_hint(10))
    );
    assert_eq!(second_capped_retry, first_capped_retry);
    assert_eq!(
        composite
            .pending_video_packet
            .as_ref()
            .map(|packet| packet.data.len()),
        Some(b"v40".len())
    );
    assert!(composite.pending_audio_packet.is_none());

    let mut recovered_markers = Vec::new();
    loop {
        match composite
            .next_event()
            .expect("recovery должна сохранить packet/event order")
        {
            DemuxReadEvent::Packet(packet) => recovered_markers.push(packet.data),
            DemuxReadEvent::EndOfStream => break,
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                panic!("scripted component уже восстановился")
            }
            DemuxReadEvent::TracksChanged(_) | DemuxReadEvent::MediaMetadataChanged(_) => {
                panic!("fixture не содержит lifecycle events")
            }
        }
    }
    assert_eq!(
        recovered_markers,
        vec![
            Bytes::from_static(b"a0"),
            Bytes::from_static(b"v40"),
            Bytes::from_static(b"a40"),
            Bytes::from_static(b"v80"),
            Bytes::from_static(b"a80"),
        ]
    );
}

#[test]
fn timestamp_lead_holds_one_packet_until_lagging_component_recovers() {
    let video_events = VecDeque::from([
        DemuxReadEvent::Packet(packet(11, TrackKind::Video, 0, b"v0")),
        DemuxReadEvent::Packet(packet(11, TrackKind::Video, 4_000, b"v4000")),
        DemuxReadEvent::Packet(packet(11, TrackKind::Video, 6_000, b"v6000")),
        DemuxReadEvent::EndOfStream,
    ]);
    let audio_events = VecDeque::from([
        DemuxReadEvent::Packet(packet(22, TrackKind::Audio, 0, b"a0")),
        DemuxReadEvent::TemporarilyUnavailable(retry_hint(15)),
        DemuxReadEvent::TemporarilyUnavailable(retry_hint(15)),
        DemuxReadEvent::TemporarilyUnavailable(retry_hint(15)),
        DemuxReadEvent::Packet(packet(22, TrackKind::Audio, 5_500, b"a5500")),
        DemuxReadEvent::EndOfStream,
    ]);
    let (mut composite, _, _) = composite(
        vec![track(11, TrackKind::Video, "V_H264", 10_000)],
        vec![track(22, TrackKind::Audio, "A_AAC", 10_000)],
        video_events,
        audio_events,
        SeekBehavior::Success(MediaTime::ZERO),
        SeekBehavior::Success(MediaTime::ZERO),
    );

    assert!(matches!(
        composite.next_event().expect("video zero packet"),
        DemuxReadEvent::Packet(ref packet) if packet.data.as_ref() == b"v0"
    ));
    assert!(matches!(
        composite.next_event().expect("audio zero packet"),
        DemuxReadEvent::Packet(ref packet) if packet.data.as_ref() == b"a0"
    ));
    assert!(matches!(
        composite
            .next_event()
            .expect("ready side может идти внутри timestamp lead"),
        DemuxReadEvent::Packet(ref packet) if packet.data.as_ref() == b"v4000"
    ));
    assert_eq!(
        composite.next_event().expect("timestamp lead cap"),
        DemuxReadEvent::TemporarilyUnavailable(retry_hint(15))
    );
    assert_eq!(
        composite.next_event().expect("continued starvation"),
        DemuxReadEvent::TemporarilyUnavailable(retry_hint(15))
    );
    assert_eq!(
        composite
            .pending_video_packet
            .as_ref()
            .map(|packet| packet.data.as_ref()),
        Some(b"v6000".as_slice())
    );
    assert!(matches!(
        composite.next_event().expect("lagging audio recovery"),
        DemuxReadEvent::Packet(ref packet) if packet.data.as_ref() == b"a5500"
    ));
    assert!(matches!(
        composite.next_event().expect("held video recovery"),
        DemuxReadEvent::Packet(ref packet) if packet.data.as_ref() == b"v6000"
    ));
}

#[test]
fn oversized_pending_packet_fails_before_composite_retains_it() {
    let video_tracks = vec![track(11, TrackKind::Video, "V_H264", 10_000)];
    let audio_tracks = vec![track(22, TrackKind::Audio, "A_AAC", 10_000)];
    let video_demuxer = ScriptedDemuxer::new(
        video_tracks,
        Some(Duration::from_secs(10)),
        VecDeque::from([DemuxReadEvent::Packet(packet(
            11,
            TrackKind::Video,
            0,
            b"oversized",
        ))]),
        SeekBehavior::Success(MediaTime::ZERO),
        Arc::new(Mutex::new(Vec::new())),
    );
    let audio_demuxer = ScriptedDemuxer::new(
        audio_tracks,
        Some(Duration::from_secs(10)),
        VecDeque::from([DemuxReadEvent::TemporarilyUnavailable(retry_hint(10))]),
        SeekBehavior::Success(MediaTime::ZERO),
        Arc::new(Mutex::new(Vec::new())),
    );
    let policy = CompositeComponentLeadPolicy::new(
        Duration::from_secs(5),
        NonZeroUsize::new(2).expect("non-zero packet cap"),
        NonZeroUsize::new(3).expect("non-zero byte cap"),
    )
    .expect("small focused policy remains within safety ceilings");
    let mut composite = CompositeAvDemuxer::new(
        Box::new(video_demuxer),
        Box::new(audio_demuxer),
        CompositeAvTrackSelection::new(TrackId::new(11), TrackId::new(22)),
        policy,
    )
    .expect("composite opens before first packet read");

    let error = composite
        .next_event()
        .expect_err("oversized pending packet должен нарушить memory invariant");
    let typed_error = error
        .downcast_ref::<CompositePendingPacketTooLargeError>()
        .expect("ошибка должна сохранять typed composite safety context");

    assert_eq!(typed_error.component, CompositeComponent::Video);
    assert_eq!(typed_error.packet_bytes, b"oversized".len());
    assert_eq!(typed_error.maximum_bytes, 3);
    assert!(composite.pending_video_packet.is_none());
}
