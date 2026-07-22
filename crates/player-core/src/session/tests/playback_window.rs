use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::bounded;
use media_core::{MediaTime, TrackKind};

use super::test_support::{FakeDemuxer, fake_audio_packet, fake_track};
use super::*;
use crate::{
    ExactTimelineSeekOutcome, ExactTimelineSeekRequest, InstalledMediaStateRestore,
    InstalledMediaStateRestoreOutcome, InstalledPositionRestore, InstalledSubtitleRestore,
    InstalledTrackRestore, MediaInstallRequestId, MediaInstanceId, MediaPlaybackWindow,
    PlaybackIntent, PlaybackIntentRevision, PlaybackRate, PlaybackState, ScrubCommitPolicy,
    TimelineSeekKind, TimelineSeekRequestId,
};

/// Создаёт seekable prepared source с optional bounded/open-ended window.
fn prepared_window_media(
    tracks: Vec<media_core::TrackInfo>,
    source_duration: Duration,
    window_start: Duration,
    window_end: Option<Duration>,
) -> (PreparedMedia, Arc<Mutex<Vec<Duration>>>) {
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let demuxer = FakeDemuxer::new(tracks, Some(source_duration), Arc::clone(&seek_log));
    let playback_window = MediaPlaybackWindow::new(
        MediaTime::from_duration(window_start),
        window_end.map(MediaTime::from_duration),
    )
    .expect("test playback window must be valid");
    let prepared_media = PreparedMedia::from_external_label("windowed-source", Box::new(demuxer))
        .with_playback_window(playback_window);
    (prepared_media, seek_log)
}

#[test]
fn install_preseeks_absolute_start_and_publishes_relative_timeline() {
    let (prepared_media, seek_log) = prepared_window_media(
        Vec::new(),
        Duration::from_secs(60),
        Duration::from_secs(10),
        Some(Duration::from_secs(25)),
    );
    let mut session = PlayerSession::new();

    session.load_prepared_media_with_autoplay(prepared_media, false);

    assert_eq!(
        *seek_log.lock().expect("seek log mutex"),
        vec![Duration::from_secs(10)]
    );
    assert_eq!(session.snapshot().current_position, Duration::ZERO);
    assert_eq!(session.snapshot().duration, Some(Duration::from_secs(15)));
    assert_eq!(
        session.snapshot().timeline.seekable_range,
        Some(media_core::TimelineRange {
            start: MediaTime::ZERO,
            end: MediaTime::from_secs(15),
        })
    );
}

#[test]
fn open_end_uses_last_source_boundary_and_relative_seek_maps_to_absolute_demux_time() {
    let (prepared_media, seek_log) = prepared_window_media(
        Vec::new(),
        Duration::from_secs(55),
        Duration::from_secs(40),
        None,
    );
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(4),
        )))
        .expect("relative public seek accepted");

    assert_eq!(session.snapshot().duration, Some(Duration::from_secs(15)));
    assert_eq!(
        *seek_log.lock().expect("seek log mutex"),
        vec![Duration::from_secs(40), Duration::from_secs(44)]
    );
    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(MediaTime::from_secs(4))
    );
}

#[test]
fn installing_window_from_another_source_replaces_every_previous_absolute_boundary() {
    let (first_media, first_seek_log) = prepared_window_media(
        Vec::new(),
        Duration::from_secs(40),
        Duration::from_secs(8),
        Some(Duration::from_secs(20)),
    );
    let (second_media, second_seek_log) = prepared_window_media(
        Vec::new(),
        Duration::from_secs(90),
        Duration::from_secs(30),
        Some(Duration::from_secs(50)),
    );
    let mut session = PlayerSession::new();

    session.load_prepared_media_with_autoplay(first_media, false);
    session.load_prepared_media_with_autoplay(second_media, false);
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .expect("seek uses only the last installed source window");

    assert_eq!(
        *first_seek_log.lock().expect("first seek log mutex"),
        vec![Duration::from_secs(8)]
    );
    assert_eq!(
        *second_seek_log.lock().expect("second seek log mutex"),
        vec![Duration::from_secs(30), Duration::from_secs(35)]
    );
    assert_eq!(session.snapshot().duration, Some(Duration::from_secs(20)));
}

#[test]
fn exact_set_position_reports_relative_commit_position() {
    let (prepared_media, seek_log) = prepared_window_media(
        Vec::new(),
        Duration::from_secs(60),
        Duration::from_secs(10),
        Some(Duration::from_secs(25)),
    );
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);
    let media_instance_id = session
        .snapshot()
        .media_instance_id
        .expect("installed media instance");
    let request_id =
        TimelineSeekRequestId::new(std::num::NonZeroU64::new(91).expect("non-zero request"));
    let (outcome_tx, outcome_rx) = bounded(1);

    session.begin_exact_timeline_seek(
        ExactTimelineSeekRequest {
            request_id,
            media_instance_id,
            target: MediaTime::from_secs(4),
            kind: TimelineSeekKind::SetPosition,
        },
        outcome_tx,
    );
    assert!(outcome_rx.try_recv().is_err());
    let seek_generation = session.pipeline.seek_generation();
    session.complete_pending_seek_receipts(MediaTime::from_secs(14), seek_generation);

    assert_eq!(
        *seek_log.lock().expect("seek log mutex"),
        vec![Duration::from_secs(10), Duration::from_secs(14)]
    );
    assert_eq!(
        outcome_rx.recv().expect("exact terminal outcome"),
        ExactTimelineSeekOutcome::Applied {
            request_id,
            media_instance_id,
            position: MediaTime::from_secs(4),
        }
    );
}

#[test]
fn ordinary_seek_clamps_to_window_end_while_exact_set_position_rejects_beyond_end() {
    let (prepared_media, seek_log) = prepared_window_media(
        Vec::new(),
        Duration::from_secs(60),
        Duration::from_secs(10),
        Some(Duration::from_secs(25)),
    );
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);
    let media_instance_id = session
        .snapshot()
        .media_instance_id
        .expect("installed media instance");

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(99),
        )))
        .expect("ordinary seek keeps clamp policy");
    assert_eq!(
        *seek_log.lock().expect("seek log mutex"),
        vec![Duration::from_secs(10), Duration::from_secs(25)]
    );

    let request_id =
        TimelineSeekRequestId::new(std::num::NonZeroU64::new(92).expect("non-zero request"));
    let (outcome_tx, outcome_rx) = bounded(1);
    session.begin_exact_timeline_seek(
        ExactTimelineSeekRequest {
            request_id,
            media_instance_id,
            target: MediaTime::from_secs(16),
            kind: TimelineSeekKind::SetPosition,
        },
        outcome_tx,
    );
    assert_eq!(
        outcome_rx.recv().expect("strict range outcome"),
        ExactTimelineSeekOutcome::InvalidRange { request_id }
    );
}

#[test]
fn simple_live_scrub_keeps_public_target_relative_and_commits_absolute_seek() {
    let (prepared_media, seek_log) = prepared_window_media(
        Vec::new(),
        Duration::from_secs(60),
        Duration::from_secs(10),
        Some(Duration::from_secs(25)),
    );
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);

    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .expect("scrub begin accepted");
    session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            MediaTime::from_secs(4),
        )))
        .expect("relative scrub target accepted");

    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(MediaTime::from_secs(4))
    );
    assert_eq!(
        *seek_log.lock().expect("seek log mutex"),
        vec![Duration::from_secs(10)]
    );

    session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::DEFAULT_TIMELINE_RELEASE,
        ))
        .expect("scrub release accepted");

    assert_eq!(
        *seek_log.lock().expect("seek log mutex"),
        vec![Duration::from_secs(10), Duration::from_secs(14)]
    );
    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(MediaTime::from_secs(4))
    );
}

#[test]
fn installed_restore_stays_pending_and_maps_relative_target_until_absolute_commit() {
    let (prepared_media, seek_log) = prepared_window_media(
        Vec::new(),
        Duration::from_secs(60),
        Duration::from_secs(10),
        Some(Duration::from_secs(25)),
    );
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);
    let request_id = MediaInstallRequestId::from_non_zero(
        std::num::NonZeroU64::new(301).expect("non-zero request"),
    );
    let media_instance_id =
        MediaInstanceId::from_non_zero(std::num::NonZeroU64::new(302).expect("non-zero instance"));
    session.playback_intent_control.register_staged_request(
        request_id,
        crate::media_install::AcceptedPlaybackIntent {
            revision: PlaybackIntentRevision::INITIAL,
            intent: PlaybackIntent::StartPaused,
        },
    );
    session
        .playback_intent_control
        .commit_staged_request(request_id, media_instance_id, |_| {});
    session.snapshot.media_instance_id = Some(media_instance_id);
    let (outcome_tx, outcome_rx) = bounded(1);

    session.begin_installed_media_state_restore(
        InstalledMediaStateRestore {
            request_id,
            media_instance_id,
            video_track: InstalledTrackRestore::KeepDefault,
            audio_track: InstalledTrackRestore::KeepDefault,
            subtitle_track: InstalledSubtitleRestore::KeepDefault,
            volume: crate::InstalledVolumeRestore::KeepCurrent,
            position: InstalledPositionRestore::SeekTo(Duration::from_secs(4)),
        },
        outcome_tx,
    );

    assert!(outcome_rx.try_recv().is_err());
    assert_eq!(
        *seek_log.lock().expect("seek log mutex"),
        vec![Duration::from_secs(10), Duration::from_secs(14)]
    );
    let seek_generation = session.pipeline.seek_generation();
    session.complete_pending_seek_receipts(MediaTime::from_secs(14), seek_generation);
    assert_eq!(
        outcome_rx.recv().expect("restore terminal outcome"),
        InstalledMediaStateRestoreOutcome::Applied { media_instance_id }
    );
}

#[test]
fn paused_playback_rate_change_keeps_relative_and_absolute_positions_stable() {
    let (prepared_media, _) = prepared_window_media(
        Vec::new(),
        Duration::from_secs(60),
        Duration::from_secs(10),
        Some(Duration::from_secs(25)),
    );
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);
    session
        .dispatch_command(PlayerCommand::Play)
        .expect("windowed media starts playing");
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    session
        .dispatch_command(PlayerCommand::Pause)
        .expect("windowed media pauses");
    assert_eq!(session.playback_state(), PlaybackState::Paused);
    session.update_current_position(Duration::from_secs(5));
    let requested_rate = PlaybackRate::new(2.0).expect("valid rate");

    session
        .dispatch_command(PlayerCommand::SetPlaybackRate(requested_rate))
        .expect("video-only paused rate applies");

    assert_eq!(session.snapshot().current_position, Duration::from_secs(5));
    assert_eq!(session.current_source_position, Duration::from_secs(15));
    assert_eq!(session.snapshot().playback_rate, requested_rate);
}

#[test]
fn selected_track_crossing_window_end_enters_clean_drain_boundary() {
    let audio_track = fake_track(7, TrackKind::Audio);
    let (prepared_media, seek_log) = prepared_window_media(
        vec![audio_track],
        Duration::from_secs(60),
        Duration::from_secs(10),
        Some(Duration::from_secs(25)),
    );
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);
    session.pipeline.select_audio_track(TrackId::new(7));
    let first_outside_packet = fake_audio_packet(
        TrackId::new(7),
        Duration::from_secs(25),
        Duration::from_millis(20),
    );

    assert!(session.packet_is_outside_playback_window(&first_outside_packet));
    assert!(session.playback_window_end_observed());
    session.enter_eof_drain();

    assert_eq!(session.playback_state(), PlaybackState::Draining);
    assert!(session.finish_eof_drain_if_ready(Instant::now(), Duration::from_millis(500)));
    assert_eq!(session.playback_state(), PlaybackState::Ended);
    assert_eq!(session.snapshot().current_position, Duration::from_secs(15));

    session
        .play()
        .expect("replay from window Ended starts seek");
    assert_eq!(
        *seek_log.lock().expect("seek log mutex"),
        vec![Duration::from_secs(10), Duration::from_secs(10)]
    );
}

#[test]
fn audio_preroll_before_start_is_dropped_but_overlapping_packet_is_retained() {
    let (prepared_media, _) = prepared_window_media(
        vec![fake_track(8, TrackKind::Audio)],
        Duration::from_secs(60),
        Duration::from_secs(10),
        Some(Duration::from_secs(25)),
    );
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);
    let fully_before = fake_audio_packet(
        TrackId::new(8),
        Duration::from_secs(9),
        Duration::from_millis(500),
    );
    let overlaps_start = fake_audio_packet(
        TrackId::new(8),
        Duration::from_millis(9_800),
        Duration::from_millis(500),
    );

    assert!(session.audio_packet_is_before_playback_window(&fully_before));
    assert!(!session.audio_packet_is_before_playback_window(&overlaps_start));
}

#[test]
fn out_of_source_window_fails_before_replacing_active_media() {
    let (active_media, _) =
        prepared_window_media(Vec::new(), Duration::from_secs(30), Duration::ZERO, None);
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(active_media, false);
    let active_instance = session.snapshot().media_instance_id;

    let invalid_window =
        MediaPlaybackWindow::new(MediaTime::from_secs(31), None).expect("shape is valid");
    let invalid_demuxer = FakeDemuxer::new(
        Vec::new(),
        Some(Duration::from_secs(30)),
        Arc::new(Mutex::new(Vec::new())),
    );
    let invalid_media =
        PreparedMedia::from_external_label("invalid-window", Box::new(invalid_demuxer))
            .with_playback_window(invalid_window);
    session.load_prepared_media_with_autoplay(invalid_media, false);

    assert_eq!(session.snapshot().media_instance_id, active_instance);
    assert_eq!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .expect("pre-barrier failure is visible")
            .kind,
        PlayerErrorKind::SeekUnavailable
    );
}
