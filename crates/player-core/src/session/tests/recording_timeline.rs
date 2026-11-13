//! Табло шкалы показывает запись (мост 16d1, решения 318–320).
//!
//! Что закрепляют тесты:
//! - эфир, ставший записью, получает длину и перемотку по всей записи, а позиция не двигается;
//! - длина записи растёт — перемотка не срезается до первой длины (дефект, найденный зондом
//!   16d1 на процессе браузерного режима);
//! - смена дорожек не переписывает длину, которой владеет табло (один писатель шкалы);
//! - «пока неизвестно» — без перемотки, пока источник не узнал длину.

use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use media_core::{
    DemuxTrackListUpdate, DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial,
    DynamicMediaTimelinePortGeneration, DynamicMediaTimelinePublisher, DynamicMediaTimelineState,
    DynamicMediaTimelineView, MediaTime, RecordingTimelineState, TimelineMode,
    TimelineNotSeekableReason, TimelineRange, TrackKind, dynamic_media_timeline,
};

use super::test_support::{FakeDemuxer, fake_track};
use crate::{PlayerCommand, PlayerSession, PreparedMedia, SeekRequest};

/// Эпоха источника: тесты публикуют в одной эпохе, как MSE-полка одного поколения.
const EPOCH: DynamicMediaTimelineEpoch = DynamicMediaTimelineEpoch::new(1);

/// Media с табло шкалы и журналом перемоток demuxer-а (куда реально приземлилась перемотка).
fn media_with_timeline(
    initial: DynamicMediaTimelineView,
) -> (
    PreparedMedia,
    DynamicMediaTimelinePublisher,
    Arc<Mutex<Vec<Duration>>>,
) {
    let (port, publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
        port_generation: DynamicMediaTimelinePortGeneration::new(NonZeroU64::MIN),
        source_epoch: EPOCH,
        state: initial,
    });
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    // Demuxer называет СВОЮ длину (45 с): табло главнее, иначе две копии длины разошлись бы.
    let demuxer = FakeDemuxer::new(
        vec![fake_track(1, TrackKind::Video)],
        Some(Duration::from_secs(45)),
        Arc::clone(&seek_log),
    );
    let prepared_media = PreparedMedia::from_external_label("fake-timeline", Box::new(demuxer))
        .with_dynamic_timeline(port)
        .expect("media without a playback window accepts a timeline port");
    (prepared_media, publisher, seek_log)
}

fn recording(duration_secs: u64) -> RecordingTimelineState {
    RecordingTimelineState::from_start(MediaTime::from_secs(duration_secs))
        .expect("non-empty test recording")
}

fn live_window(start_secs: u64, end_secs: u64) -> DynamicMediaTimelineState {
    let range = TimelineRange::new(
        MediaTime::from_secs(start_secs),
        MediaTime::from_secs(end_secs),
    )
    .expect("ordered test range");
    DynamicMediaTimelineState::with_dvr(MediaTime::from_secs(end_secs), range)
        .expect("valid test DVR state")
}

fn installed(prepared_media: PreparedMedia) -> PlayerSession {
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);
    session
}

fn seek_to(session: &mut PlayerSession, seconds: u64) {
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(seconds),
        )))
        .expect("a seek inside the timeline is accepted");
}

fn last_demux_seek(seek_log: &Mutex<Vec<Duration>>) -> Option<Duration> {
    seek_log.lock().expect("seek log lock").last().copied()
}

#[test]
fn a_live_stream_that_became_a_recording_gets_a_duration_without_moving_playback() {
    let (prepared_media, publisher, _seek_log) = media_with_timeline(live_window(20, 60).into());
    let mut session = installed(prepared_media);
    assert_eq!(session.snapshot().timeline.mode, TimelineMode::Live);
    let position_before = session.snapshot().current_position;

    publisher
        .publish_view(EPOCH, recording(61).into())
        .expect("the end of the stream is published");
    assert!(session.refresh_dynamic_timeline());

    let snapshot = session.snapshot();
    assert_eq!(snapshot.timeline.mode, TimelineMode::Static);
    assert_eq!(snapshot.duration, Some(Duration::from_secs(61)));
    assert_eq!(
        snapshot
            .timeline
            .duration
            .map(media_core::MediaDuration::as_duration),
        Some(Duration::from_secs(61))
    );
    assert_eq!(
        snapshot.timeline.seekable_range,
        TimelineRange::new(MediaTime::ZERO, MediaTime::from_secs(61))
    );
    assert!(snapshot.timeline.seekable);
    assert_eq!(snapshot.timeline.live_edge, None);
    assert_eq!(snapshot.timeline.live_revision, None);
    assert_eq!(
        snapshot.current_position, position_before,
        "playback does not jump"
    );
}

#[test]
fn a_seek_behind_the_former_live_window_lands_once_the_stream_is_a_recording() {
    let (prepared_media, publisher, seek_log) = media_with_timeline(live_window(20, 60).into());
    let mut session = installed(prepared_media);
    publisher
        .publish_view(EPOCH, recording(61).into())
        .expect("the end of the stream is published");
    assert!(session.refresh_dynamic_timeline());

    // В эфире 5 с лежали бы за окном 20–60 с (мягкий отказ); у записи — обычная перемотка.
    seek_to(&mut session, 5);

    assert_eq!(last_demux_seek(&seek_log), Some(Duration::from_secs(5)));
}

#[test]
fn a_growing_recording_does_not_clamp_seeks_to_its_first_duration() {
    let (prepared_media, publisher, seek_log) = media_with_timeline(recording(3).into());
    let mut session = installed(prepared_media);
    assert_eq!(session.snapshot().timeline.mode, TimelineMode::Static);
    assert_eq!(session.snapshot().duration, Some(Duration::from_secs(3)));

    // Страница объявила 6 с (YouTube растит длительность эфира на ходу).
    publisher
        .publish_view(EPOCH, recording(6).into())
        .expect("the longer duration is published");
    assert!(session.refresh_dynamic_timeline());
    seek_to(&mut session, 4);

    assert_eq!(session.snapshot().duration, Some(Duration::from_secs(6)));
    assert_eq!(
        last_demux_seek(&seek_log),
        Some(Duration::from_secs(4)),
        "the seek lands at 4 s, not at the first duration of 3 s"
    );
}

#[test]
fn before_the_page_announces_more_the_recording_still_clamps_to_its_end() {
    let (prepared_media, _publisher, seek_log) = media_with_timeline(recording(3).into());
    let mut session = installed(prepared_media);

    seek_to(&mut session, 4);

    assert_eq!(last_demux_seek(&seek_log), Some(Duration::from_secs(3)));
}

#[test]
fn a_track_list_update_keeps_the_duration_owned_by_the_timeline() {
    let (prepared_media, _publisher, _seek_log) = media_with_timeline(recording(6).into());
    let mut session = installed(prepared_media);

    // Смена качества: demuxer снова называет свою длину 45 с.
    session.handle_demux_track_list_update(DemuxTrackListUpdate::new(
        vec![fake_track(2, TrackKind::Video)],
        Some(Duration::from_secs(45)),
    ));

    let snapshot = session.snapshot();
    assert_eq!(snapshot.duration, Some(Duration::from_secs(6)));
    assert_eq!(
        snapshot.timeline.seekable_range,
        TimelineRange::new(MediaTime::ZERO, MediaTime::from_secs(6))
    );
}

/// Цель, а не нынешнее поведение: у эфира смена дорожек пока затирает шкалу (известный
/// дефект, найден в 16d1, `apply_demuxer_reported_duration`). Включить вместе с починкой.
#[test]
#[ignore = "16d1: the live timeline survives track list updates only after the live audio-start fix"]
fn a_track_list_update_keeps_the_live_window() {
    let (prepared_media, _publisher, _seek_log) = media_with_timeline(live_window(20, 60).into());
    let mut session = installed(prepared_media);

    session.handle_demux_track_list_update(DemuxTrackListUpdate::new(
        vec![fake_track(2, TrackKind::Video)],
        None,
    ));

    let snapshot = session.snapshot();
    assert_eq!(snapshot.timeline.mode, TimelineMode::Live);
    assert_eq!(snapshot.timeline.live_edge, Some(MediaTime::from_secs(60)));
    assert_eq!(
        snapshot.timeline.seekable_range,
        TimelineRange::new(MediaTime::from_secs(20), MediaTime::from_secs(60))
    );
}

#[test]
fn an_undetermined_timeline_becomes_seekable_once_the_source_knows_the_length() {
    let (prepared_media, publisher, seek_log) =
        media_with_timeline(DynamicMediaTimelineView::Undetermined);
    let mut session = installed(prepared_media);
    let snapshot = session.snapshot();
    assert_eq!(snapshot.duration, None);
    assert!(!snapshot.timeline.seekable);
    assert_eq!(
        snapshot.timeline.not_seekable_reason,
        Some(TimelineNotSeekableReason::UnknownTimeline)
    );

    publisher
        .publish_view(EPOCH, recording(10).into())
        .expect("the length is published");
    assert!(session.refresh_dynamic_timeline());
    seek_to(&mut session, 7);

    assert_eq!(session.snapshot().duration, Some(Duration::from_secs(10)));
    assert_eq!(last_demux_seek(&seek_log), Some(Duration::from_secs(7)));
}

#[test]
fn a_publication_between_preparation_and_install_is_installed_from_the_fresh_snapshot() {
    let (prepared_media, publisher, _seek_log) = media_with_timeline(recording(3).into());
    // Источник публикует из своего потока: открытие поколения может ждать до 10 с.
    publisher
        .publish_view(EPOCH, recording(6).into())
        .expect("the page named a longer duration before the install");

    let session = installed(prepared_media);

    assert_eq!(session.snapshot().duration, Some(Duration::from_secs(6)));
    assert_eq!(
        session.snapshot().timeline.seekable_range,
        TimelineRange::new(MediaTime::ZERO, MediaTime::from_secs(6))
    );
}

#[test]
fn a_seek_to_the_end_of_a_growing_recording_is_not_expired_by_the_growth() {
    let (prepared_media, publisher, seek_log) = media_with_timeline(recording(6).into());
    let mut session = installed(prepared_media);
    seek_to(&mut session, 6);
    let seek_before = session.seek_commit().map(|commit| commit.target_position);

    publisher
        .publish_view(EPOCH, recording(8).into())
        .expect("the recording grows");
    assert!(session.refresh_dynamic_timeline());

    assert_eq!(last_demux_seek(&seek_log), Some(Duration::from_secs(6)));
    assert_eq!(
        session.seek_commit().map(|commit| commit.target_position),
        seek_before,
        "the growth keeps the seek that is already landing"
    );
    assert!(
        session.snapshot().last_error.is_none(),
        "no SeekTargetExpired: {:?}",
        session.snapshot().last_error
    );
}
