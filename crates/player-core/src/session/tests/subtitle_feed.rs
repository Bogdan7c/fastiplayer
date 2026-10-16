//! Встроенные субтитры: от demux-пакета через настоящий tick до приёмника трубы.
//!
//! Media ставится настоящим staged-протоколом, пакеты читает настоящий `PlayerSession::tick`.
//! Проверяем наблюдаемое: что получил приёмник и что дошло до video decoder-а.

use std::num::NonZeroU64;
use std::time::{Duration, Instant};

use bytes::Bytes;
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, MediaAttachment, MediaAttachments,
    MediaTime, Packet, PacketKeyframe, SkippedAttachment, SkippedAttachmentReason,
    TrackDisposition, TrackLabels, TrackLanguage,
};

use super::test_support::{
    SharedFakeVideoDecoderThread, fake_track, fake_video_packet_with_keyframe,
};
use super::*;
use crate::subtitle_feed::{
    SubtitleFeedCapacity, SubtitleFeedMediaStart, SubtitleFeedMessage, SubtitleFeedPacket,
    SubtitleFeedReceiver, subtitle_feed,
};
use crate::{
    AuthorizeInstallCommit, FrameCounters, MediaInstallCompletion, MediaInstallControl,
    MediaInstallControlOutcome, MediaInstallPhase, MediaInstallReceipt, MediaInstallRequestId,
    MediaInstanceId, PlaybackIntent, PlaybackIntentRevision,
};

const VIDEO_TRACK: u32 = 1;
const AUDIO_TRACK: u32 = 2;
const SUBTITLE_TRACK_A: u32 = 3;
const SUBTITLE_TRACK_B: u32 = 4;

/// Demuxer с возможностью перечитать пакеты после seek и отдать вложения один раз.
struct ReplayDemuxer {
    tracks: Vec<media_core::TrackInfo>,
    packets: Vec<Packet>,
    cursor: usize,
    attachments: Option<MediaAttachments>,
}

impl Demuxer for ReplayDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(60))
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        let packet = self.packets.get(self.cursor).cloned();
        if packet.is_some() {
            self.cursor += 1;
        }
        Ok(media_core::finite_packet_read_event(packet))
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        // Seek назад перечитывает поток с начала: те же пакеты приходят снова.
        self.cursor = 0;
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(timestamp),
            actual_position: MediaTime::from_duration(timestamp),
            actual_track_timestamp: None,
        })
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.seek(request.timestamp)
    }

    fn take_attachments(&mut self) -> MediaAttachments {
        self.attachments.take().unwrap_or_default()
    }
}

fn subtitle_packet(track: u32, pts_ms: u64, duration_ms: u64, text: &'static [u8]) -> Packet {
    Packet::new_unbounded(
        TrackId::new(track),
        TrackKind::Subtitle,
        Duration::from_millis(pts_ms),
        None,
        true,
        Bytes::from_static(text),
    )
    .with_duration(Duration::from_millis(duration_ms))
}

fn video_packet(pts_ms: u64) -> Packet {
    fake_video_packet_with_keyframe(
        TrackId::new(VIDEO_TRACK),
        Duration::from_millis(pts_ms),
        PacketKeyframe::Keyframe,
    )
}

fn standard_tracks() -> Vec<media_core::TrackInfo> {
    vec![
        fake_track(VIDEO_TRACK, TrackKind::Video),
        fake_track(AUDIO_TRACK, TrackKind::Audio),
        fake_track(SUBTITLE_TRACK_A, TrackKind::Subtitle),
        fake_track(SUBTITLE_TRACK_B, TrackKind::Subtitle),
    ]
}

/// Видео и две субтитровые дорожки вперемешку, как в mkv.
fn interleaved_packets() -> Vec<Packet> {
    vec![
        video_packet(0),
        subtitle_packet(SUBTITLE_TRACK_A, 100, 900, b"a-one"),
        subtitle_packet(SUBTITLE_TRACK_B, 120, 800, b"b-one"),
        video_packet(40),
        subtitle_packet(SUBTITLE_TRACK_A, 1_500, 700, b"a-two"),
        video_packet(80),
    ]
}

struct FeedHarness {
    session: PlayerSession,
    receiver: SubtitleFeedReceiver,
    decoder: SharedFakeVideoDecoderThread,
    media_instance_id: MediaInstanceId,
}

/// Ставит media через staged-протокол и подключает fake video backend.
fn install_media(
    session: &mut PlayerSession,
    demuxer: Box<dyn Demuxer + Send>,
    request: u64,
) -> MediaInstanceId {
    let request_id = MediaInstallRequestId::from_non_zero(
        NonZeroU64::new(request).expect("request id must be non-zero"),
    );
    let (install_receipt, install_port) = MediaInstallReceipt::new(request_id);
    session.stage_prepared_media_install_compatibility(
        request_id,
        PreparedMedia::from_external_label("subtitle-feed", demuxer),
        PlaybackIntent::StartPlaying,
        PlaybackIntentRevision::INITIAL,
        install_port,
    );
    assert_eq!(
        install_receipt.try_take_ready_to_commit(),
        Some(MediaInstallPhase::ReadyToCommit { request_id })
    );
    assert_eq!(
        session.apply_staged_media_install_control(MediaInstallControl::Authorize(
            AuthorizeInstallCommit { request_id },
        )),
        MediaInstallControlOutcome::AuthorizationAccepted
    );
    let MediaInstallCompletion::Installed {
        media_instance_id, ..
    } = install_receipt
        .try_take_completion()
        .expect("authorized install completes")
    else {
        panic!("install must succeed");
    };
    media_instance_id
}

fn harness_with(
    capacity: usize,
    tracks: Vec<media_core::TrackInfo>,
    packets: Vec<Packet>,
    attachments: Option<MediaAttachments>,
) -> FeedHarness {
    harness_with_demuxer(
        capacity,
        Box::new(ReplayDemuxer {
            tracks,
            packets,
            cursor: 0,
            attachments,
        }),
    )
}

fn harness_with_demuxer(capacity: usize, demuxer: Box<dyn Demuxer + Send>) -> FeedHarness {
    let (sender, receiver) = subtitle_feed(
        SubtitleFeedCapacity::from_messages(capacity).expect("test capacity is non-zero"),
    );
    let mut session = PlayerSession::new().with_subtitle_feed(sender);
    let media_instance_id = install_media(&mut session, demuxer, 7_001);
    let decoder = SharedFakeVideoDecoderThread::new();
    session.set_video_backend(StartedVideoBackend::from_decoder_thread(
        "subtitle-feed-test-backend",
        decoder.clone(),
    ));
    FeedHarness {
        session,
        receiver,
        decoder,
        media_instance_id,
    }
}

fn tick_config() -> PlayerTickConfig {
    PlayerTickConfig {
        max_demux_packets_per_tick: 64,
        ..PlayerTickConfig::default()
    }
}

fn run_ticks(session: &mut PlayerSession, ticks: usize) -> Vec<PlayerTickResult> {
    (0..ticks)
        .map(|_| {
            session.tick(PlayerTickContext::with_config(
                Instant::now(),
                tick_config(),
            ))
        })
        .collect()
}

fn drain(receiver: &SubtitleFeedReceiver) -> Vec<SubtitleFeedMessage> {
    let mut messages = Vec::new();
    receiver.drain_into(&mut messages, usize::MAX);
    messages
}

fn packets_of(messages: &[SubtitleFeedMessage]) -> Vec<&SubtitleFeedPacket> {
    messages
        .iter()
        .filter_map(|message| match message {
            SubtitleFeedMessage::Packet(packet) => Some(packet),
            SubtitleFeedMessage::MediaStarted(_) => None,
        })
        .collect()
}

#[test]
fn subtitle_packets_of_all_tracks_reach_the_receiver_while_video_still_reaches_the_decoder() {
    let mut harness = harness_with(64, standard_tracks(), interleaved_packets(), None);

    run_ticks(&mut harness.session, 3);

    let messages = drain(&harness.receiver);
    let packets = packets_of(&messages);
    let observed: Vec<_> = packets
        .iter()
        .map(|packet| {
            (
                packet.track_id,
                packet.presentation_time,
                packet.duration,
                packet.payload.clone(),
                packet.media_instance_id,
            )
        })
        .collect();
    let instance = harness.media_instance_id;
    assert_eq!(
        observed,
        vec![
            (
                TrackId::new(SUBTITLE_TRACK_A),
                Duration::from_millis(100),
                Some(Duration::from_millis(900)),
                Bytes::from_static(b"a-one"),
                instance
            ),
            (
                TrackId::new(SUBTITLE_TRACK_B),
                Duration::from_millis(120),
                Some(Duration::from_millis(800)),
                Bytes::from_static(b"b-one"),
                instance
            ),
            (
                TrackId::new(SUBTITLE_TRACK_A),
                Duration::from_millis(1_500),
                Some(Duration::from_millis(700)),
                Bytes::from_static(b"a-two"),
                instance
            ),
        ],
        "пакеты обеих субтитровых дорожек, в порядке demux-а, без изменений"
    );
    assert_eq!(harness.receiver.dropped_packets(), 0);

    // Видео не пострадало: пакеты дошли до decoder-а с pts, в той же шкале, что у реплик.
    let video_pts: Vec<_> = harness
        .decoder
        .sent_packets()
        .iter()
        .map(|packet| packet.pts)
        .collect();
    assert_eq!(
        video_pts,
        [0, 40, 80].map(Duration::from_millis),
        "video pts в decoder-е равен Packet::pts, как и время реплик в трубе"
    );
}

#[test]
fn subtitle_time_is_the_demuxer_packet_pts_unchanged_like_video_decode_pts() {
    // Реплика и видеопакет с одним Packet::pts обязаны получить одно и то же время.
    let shared_pts_ms = 40;
    let mut harness = harness_with(
        16,
        standard_tracks(),
        vec![
            video_packet(0),
            video_packet(shared_pts_ms),
            subtitle_packet(SUBTITLE_TRACK_A, shared_pts_ms, 500, b"same-time"),
        ],
        None,
    );

    run_ticks(&mut harness.session, 3);

    let messages = drain(&harness.receiver);
    let subtitle_time = packets_of(&messages)[0].presentation_time;
    let decoded_video_time = harness.decoder.sent_packets()[1].pts;
    assert_eq!(subtitle_time, decoded_video_time);
}

#[test]
fn full_feed_drops_subtitles_with_a_counter_but_video_keeps_flowing_without_backpressure() {
    // Ёмкость 3: media start + 2 пакета; остальные субтитры не помещаются.
    let mut harness = harness_with(3, standard_tracks(), interleaved_packets(), None);

    let results = run_ticks(&mut harness.session, 3);

    assert!(
        results.iter().all(|result| !result.demux_backpressured),
        "переполнение трубы не создаёт demux backpressure"
    );
    assert_eq!(harness.receiver.dropped_packets(), 1);
    assert_eq!(harness.receiver.dropped_media_starts(), 0);
    let video_pts: Vec<_> = harness
        .decoder
        .sent_packets()
        .iter()
        .map(|packet| packet.pts)
        .collect();
    assert_eq!(video_pts, [0, 40, 80].map(Duration::from_millis));
    let messages = drain(&harness.receiver);
    assert_eq!(messages.len(), 3, "в очереди ровно ёмкость трубы");
}

#[test]
fn media_start_with_attachments_is_delivered_first_and_exactly_once() {
    let attachments = MediaAttachments {
        attachments: vec![MediaAttachment {
            file_name: "Arial.ttf".to_owned(),
            media_type: Some("font/ttf".to_owned()),
            bytes: Bytes::from_static(b"font-data"),
        }],
        skipped: vec![SkippedAttachment {
            file_name: "huge.ttf".to_owned(),
            reason: SkippedAttachmentReason::Unreadable,
        }],
    };
    let mut harness = harness_with(
        64,
        standard_tracks(),
        interleaved_packets(),
        Some(attachments.clone()),
    );

    run_ticks(&mut harness.session, 3);

    let messages = drain(&harness.receiver);
    assert_eq!(
        messages[0],
        SubtitleFeedMessage::MediaStarted(SubtitleFeedMediaStart {
            media_instance_id: harness.media_instance_id,
            attachments,
        }),
        "первым идёт media start с вложениями"
    );
    let media_starts = messages
        .iter()
        .filter(|message| matches!(message, SubtitleFeedMessage::MediaStarted(_)))
        .count();
    assert_eq!(media_starts, 1);
}

#[test]
fn installing_new_media_announces_a_new_media_instance() {
    let mut harness = harness_with(64, standard_tracks(), interleaved_packets(), None);
    let first_instance = harness.media_instance_id;
    run_ticks(&mut harness.session, 2);
    drain(&harness.receiver);

    let second_instance = install_media(
        &mut harness.session,
        Box::new(ReplayDemuxer {
            tracks: standard_tracks(),
            packets: vec![subtitle_packet(SUBTITLE_TRACK_B, 10, 100, b"new-media")],
            cursor: 0,
            attachments: None,
        }),
        7_002,
    );
    run_ticks(&mut harness.session, 2);

    assert_ne!(first_instance, second_instance);
    let messages = drain(&harness.receiver);
    assert!(matches!(
        &messages[0],
        SubtitleFeedMessage::MediaStarted(start) if start.media_instance_id == second_instance
    ));
    let packets = packets_of(&messages);
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].media_instance_id, second_instance);
    assert_eq!(packets[0].payload, Bytes::from_static(b"new-media"));
}

#[test]
fn after_seek_back_the_same_subtitle_packets_are_forwarded_again() {
    let mut harness = harness_with(64, standard_tracks(), interleaved_packets(), None);
    run_ticks(&mut harness.session, 3);
    let first_pass = drain(&harness.receiver);
    assert_eq!(packets_of(&first_pass).len(), 3);

    harness
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_millis(10),
        )))
        .expect("seek back is accepted");
    run_ticks(&mut harness.session, 6);

    let second_pass = drain(&harness.receiver);
    let replayed: Vec<_> = packets_of(&second_pass)
        .iter()
        .map(|packet| (packet.track_id, packet.presentation_time))
        .collect();
    assert_eq!(
        replayed,
        vec![
            (TrackId::new(SUBTITLE_TRACK_A), Duration::from_millis(100)),
            (TrackId::new(SUBTITLE_TRACK_B), Duration::from_millis(120)),
            (TrackId::new(SUBTITLE_TRACK_A), Duration::from_millis(1_500)),
        ],
        "после seek назад приёмник получает те же пакеты снова (дедупликация — в приложении)"
    );
}

#[test]
fn session_keeps_playing_video_when_the_receiver_is_dropped() {
    let mut harness = harness_with(64, standard_tracks(), interleaved_packets(), None);
    drop(harness.receiver);

    let results = run_ticks(&mut harness.session, 3);

    assert!(results.iter().all(|result| !result.demux_backpressured));
    let video_pts: Vec<_> = harness
        .decoder
        .sent_packets()
        .iter()
        .map(|packet| packet.pts)
        .collect();
    assert_eq!(video_pts, [0, 40, 80].map(Duration::from_millis));
}

#[test]
fn snapshot_publishes_labels_for_video_audio_and_subtitle_tracks() {
    let mut tracks = standard_tracks();
    tracks[0].labels = TrackLabels {
        language: None,
        title: Some("Main video".to_owned()),
        disposition: TrackDisposition::default(),
    };
    tracks[1].labels = TrackLabels {
        language: Some(TrackLanguage::new("jpn")),
        title: None,
        disposition: TrackDisposition {
            is_default: true,
            ..TrackDisposition::default()
        },
    };
    tracks[2].labels = TrackLabels {
        language: Some(TrackLanguage::new("en")),
        title: Some("Signs & Songs".to_owned()),
        disposition: TrackDisposition {
            is_forced: true,
            is_hearing_impaired: true,
            ..TrackDisposition::default()
        },
    };
    let expected: Vec<_> = tracks.iter().map(|track| track.labels.clone()).collect();
    let harness = harness_with(8, tracks, Vec::new(), None);

    let snapshot = harness
        .session
        .snapshot_with_frame_counters(FrameCounters::default());

    let published: Vec<_> = snapshot
        .tracks
        .iter()
        .map(|track| track.labels.clone())
        .collect();
    assert_eq!(published, expected);
    assert_eq!(snapshot.tracks[2].kind, TrackKind::Subtitle);
}

#[test]
fn real_mkv_with_two_srt_tracks_delivers_cue_text_through_the_session_tick() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../symphonia-demux/tests/fixtures/subtitles/two-srt-tracks.mkv");
    let demuxer =
        symphonia_demux::SymphoniaDemuxer::from_file(&fixture).expect("subtitle fixture must open");
    let subtitle_track_ids: Vec<_> = demuxer
        .tracks()
        .iter()
        .filter(|track| track.kind == TrackKind::Subtitle)
        .map(|track| track.id)
        .collect();
    assert_eq!(
        subtitle_track_ids.len(),
        2,
        "fixture has two subtitle tracks"
    );
    let mut harness = harness_with_demuxer(1_024, Box::new(demuxer));

    run_ticks(&mut harness.session, 20);

    let messages = drain(&harness.receiver);
    let packets = packets_of(&messages);
    for track_id in subtitle_track_ids {
        let cue_texts: Vec<_> = packets
            .iter()
            .filter(|packet| packet.track_id == track_id)
            .map(|packet| String::from_utf8_lossy(&packet.payload).into_owned())
            .collect();
        assert!(
            !cue_texts.is_empty() && cue_texts.iter().all(|text| !text.trim().is_empty()),
            "track {track_id:?} delivers non-empty cue text, got {cue_texts:?}"
        );
    }
    assert!(
        !harness.decoder.sent_packets().is_empty(),
        "video packets of the same file still reach the decoder"
    );
    assert_eq!(harness.receiver.dropped_packets(), 0);
}
