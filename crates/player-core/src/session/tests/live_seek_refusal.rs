//! Решение владельца 233: перемотка эфира в место, которого в окне уже нет, — мягкий отказ.
//!
//! Проверяется настоящий путь session (команда → seek lifecycle → fake decoder → показ →
//! commit), а не только классификация ошибки:
//! - отказ при приёме команды: плеер играет/стоит как до неё, часы идут (или стоят) с
//!   прежнего места, `FatalError`/`Failed` нет, зритель получает recoverable-ошибку;
//! - отказ не трогает чужое: уже идущая перемотка садится в свою цель;
//! - окно уехало во время посадки: receipts — истечение ровно один раз, старая цель не
//!   коммитится, playback продолжается у безопасного края в прежнем Playing/Paused.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::bounded;
use media_core::{
    DemuxSeekResult, DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial,
    DynamicMediaTimelinePortGeneration, DynamicMediaTimelinePublisher, DynamicMediaTimelineState,
    MediaTime, TimelineRange, dynamic_media_timeline,
};

use super::prepared_demux_seek::FakePreparedDemuxSeekPort;
use super::test_support::{
    SeekRegressionHarness, land_video_seek_from, paused_video_only_harness_at,
    playing_video_only_harness_at,
};
use crate::seek_state::{PlaybackResumeIntent, SeekTargetRetention};
use crate::{
    ExactTimelineSeekOutcome, ExactTimelineSeekRequest, MediaInstanceId, PlaybackState,
    PlayerCommand, PlayerCommandOutcome, PlayerCommandReject, PlayerErrorKind, PlayerEvent,
    PreparedDemuxSeekLandingPolicy, PreparedDemuxSeekMode, PreparedDemuxSeekOutcome,
    PreparedDemuxSeekPort, SeekRequest, SeekTargetUnavailableReason, TimelineSeekKind,
    TimelineSeekRequestId,
};

/// Где играет/стоит эфир до перемотки зрителя (внутри окна).
const PLAYING_POSITION: Duration = Duration::from_secs(30);
/// Начало окна эфира.
const WINDOW_START: u64 = 20;
/// Край окна эфира (он же live edge).
const WINDOW_END: u64 = 60;
/// Цель за окном: такого места у плеера уже нет.
const EXPIRED_TARGET: MediaTime = MediaTime::from_secs(5);

fn media_instance_id(value: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(NonZeroU64::new(value).expect("non-zero test identity"))
}

fn range(start: u64, end: u64) -> TimelineRange {
    TimelineRange::new(MediaTime::from_secs(start), MediaTime::from_secs(end))
        .expect("ordered test range")
}

/// DVR-окно `[start, end]` с краем `end` и входом в эфир на текущей позиции harness-а.
fn dvr_state_joined_at_playing_position(start: u64, end: u64) -> DynamicMediaTimelineState {
    DynamicMediaTimelineState::with_dvr(MediaTime::from_secs(end), range(start, end))
        .expect("valid test DVR state")
        .with_join_position(MediaTime::from_duration(PLAYING_POSITION))
        .expect("join position inside the window")
}

/// Ставит harness в live mode через session-owned install boundary; publisher остаётся тесту.
///
/// Вход в эфир — на текущей позиции harness-а, поэтому часы не прыгают при установке.
fn install_live_window(
    harness: &mut SeekRegressionHarness,
    generation: u64,
    state: DynamicMediaTimelineState,
) -> DynamicMediaTimelinePublisher {
    let non_zero_generation = NonZeroU64::new(generation).expect("non-zero test generation");
    let media_instance_id = MediaInstanceId::from_non_zero(non_zero_generation);
    let (port, publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
        port_generation: DynamicMediaTimelinePortGeneration::new(non_zero_generation),
        source_epoch: DynamicMediaTimelineEpoch::new(1),
        state,
    });
    harness.session.set_snapshot_duration(None);
    harness.session.snapshot.media_instance_id = Some(media_instance_id);
    harness.session.install_timeline_mode(
        media_instance_id,
        crate::PreparedMediaTimelineMode::Live { port },
    );
    publisher
}

/// Играющий эфир с DVR-окном `[20, 60]`, позиция 30 с, кадр на экране.
fn playing_live_harness(generation: u64) -> (SeekRegressionHarness, DynamicMediaTimelinePublisher) {
    let mut harness =
        playing_video_only_harness_at(media_instance_id(generation), PLAYING_POSITION);
    let publisher = install_live_window(
        &mut harness,
        generation,
        dvr_state_joined_at_playing_position(WINDOW_START, WINDOW_END),
    );
    let _events_before_scenario = harness.session.take_events();
    (harness, publisher)
}

/// Тот же эфир, но на паузе.
fn paused_live_harness(generation: u64) -> (SeekRegressionHarness, DynamicMediaTimelinePublisher) {
    let mut harness = paused_video_only_harness_at(media_instance_id(generation), PLAYING_POSITION);
    let publisher = install_live_window(
        &mut harness,
        generation,
        dvr_state_joined_at_playing_position(WINDOW_START, WINDOW_END),
    );
    let _events_before_scenario = harness.session.take_events();
    (harness, publisher)
}

/// Сдвигает окно эфира и даёт session применить его (как будильник шкалы worker-а).
fn slide_window(
    harness: &mut SeekRegressionHarness,
    publisher: &DynamicMediaTimelinePublisher,
    epoch: u64,
    start: u64,
    end: u64,
) {
    publisher
        .publish(
            DynamicMediaTimelineEpoch::new(epoch),
            DynamicMediaTimelineState::with_dvr(MediaTime::from_secs(end), range(start, end))
                .expect("valid slid DVR state"),
        )
        .expect("window slide is published");
    assert!(harness.session.refresh_dynamic_timeline());
}

/// Ни одного фатального события: отказ перемотки не ломает плеер.
fn assert_no_fatal_error(events: &[PlayerEvent]) {
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PlayerEvent::FatalError(_))),
        "a refused live seek must not be fatal: {events:?}"
    );
}

/// Виды recoverable-ошибок, которые увидит зритель (временное уведомление).
fn recoverable_error_kinds(events: &[PlayerEvent]) -> Vec<PlayerErrorKind> {
    events
        .iter()
        .filter_map(|event| match event {
            PlayerEvent::RecoverableError(error) => Some(error.kind.clone()),
            _ => None,
        })
        .collect()
}

/// Цели всех `SeekCommitted` (в относительном времени шкалы).
fn committed_targets(events: &[PlayerEvent]) -> Vec<Duration> {
    events
        .iter()
        .filter_map(|event| match event {
            PlayerEvent::SeekCommitted(info) => Some(info.target_position),
            _ => None,
        })
        .collect()
}

#[test]
fn seek_behind_the_live_window_is_refused_and_playback_goes_on_from_the_same_place() {
    let (mut harness, _publisher) = playing_live_harness(301);

    let outcome = harness
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(EXPIRED_TARGET)))
        .expect("an expired live target is a command refusal, not a player error");

    assert_eq!(
        outcome,
        PlayerCommandOutcome::Rejected(PlayerCommandReject::SeekTargetUnavailable {
            reason: SeekTargetUnavailableReason::OutsideLiveWindow {
                requested_position: EXPIRED_TARGET,
                available_range: range(WINDOW_START, WINDOW_END),
            },
        })
    );
    let events = harness.session.take_events();
    assert_no_fatal_error(&events);
    assert_eq!(
        recoverable_error_kinds(&events),
        vec![PlayerErrorKind::SeekTargetExpired],
        "the viewer gets exactly one transient notice"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PlayerEvent::SeekRequested(_))),
        "a refused seek is not reported as an accepted one"
    );
    // Плеер продолжает играть с прежнего места: состояние, позиция, кадр и идущие часы.
    let session = &harness.session;
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().timeline.target_position, None);
    assert!(!session.snapshot().timeline.seeking);
    assert_eq!(session.snapshot().current_position, PLAYING_POSITION);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(PLAYING_POSITION)
    );
    let clock_later =
        session.presentation_clock_position_at(Instant::now() + Duration::from_secs(1));
    assert!(
        clock_later >= PLAYING_POSITION + Duration::from_millis(900),
        "the playback clock must keep running after the refusal: {clock_later:?}"
    );
    assert!(
        harness.seek_request_log.lock().expect("seek log").len() == 1,
        "only the harness positioning seek reached the demuxer"
    );
}

#[test]
fn paused_live_stays_paused_on_the_same_frame_after_a_refused_seek() {
    let (mut harness, _publisher) = paused_live_harness(302);

    let outcome = harness
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(EXPIRED_TARGET)))
        .expect("refusal is not an error");

    assert!(matches!(
        outcome,
        PlayerCommandOutcome::Rejected(PlayerCommandReject::SeekTargetUnavailable { .. })
    ));
    let events = harness.session.take_events();
    assert_no_fatal_error(&events);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PlayerEvent::PlaybackStateChanged(_))),
        "a refused seek must not touch the playback state: {events:?}"
    );
    let session = &harness.session;
    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert_eq!(session.snapshot().current_position, PLAYING_POSITION);
    assert_eq!(
        session.presentation_clock_position_at(Instant::now() + Duration::from_secs(1)),
        PLAYING_POSITION,
        "a paused clock must stay where it was"
    );
}

#[test]
fn seek_in_live_without_dvr_is_refused_softly() {
    let mut harness = playing_video_only_harness_at(media_instance_id(303), PLAYING_POSITION);
    let _publisher = install_live_window(
        &mut harness,
        303,
        DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(WINDOW_END))
            .with_join_position(MediaTime::from_duration(PLAYING_POSITION))
            .expect("join before the edge"),
    );
    let _events_before_scenario = harness.session.take_events();

    let outcome = harness
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(45),
        )))
        .expect("a live stream without DVR refuses seeks softly");

    assert_eq!(
        outcome,
        PlayerCommandOutcome::Rejected(PlayerCommandReject::SeekTargetUnavailable {
            reason: SeekTargetUnavailableReason::LiveWindowUnavailable,
        })
    );
    let events = harness.session.take_events();
    assert_no_fatal_error(&events);
    assert_eq!(
        recoverable_error_kinds(&events),
        vec![PlayerErrorKind::SeekUnavailable]
    );
    assert_eq!(harness.session.playback_state(), PlaybackState::Playing);
}

#[test]
fn scrub_pointer_behind_the_live_window_is_refused_without_breaking_the_gesture() {
    let (mut harness, _publisher) = playing_live_harness(304);
    harness
        .session
        .dispatch_command(PlayerCommand::begin_scrub())
        .expect("scrub gesture begins");

    let outcome = harness
        .session
        .dispatch_command(PlayerCommand::UpdateScrub(SeekRequest::absolute(
            EXPIRED_TARGET,
        )))
        .expect("an expired pointer target is a refusal");

    assert!(matches!(
        outcome,
        PlayerCommandOutcome::Rejected(PlayerCommandReject::SeekTargetUnavailable { .. })
    ));
    assert_no_fatal_error(&harness.session.take_events());
    // Жест жив и без цели за окном: отпускание возвращает playback, как до жеста.
    assert_ne!(
        harness.session.snapshot().timeline.target_position,
        Some(EXPIRED_TARGET)
    );
    harness
        .session
        .dispatch_command(PlayerCommand::end_scrub(
            crate::ScrubCommitPolicy::CommitLatestTarget,
        ))
        .expect("release after a refused pointer target");
    assert_eq!(harness.session.playback_state(), PlaybackState::Playing);
    assert_no_fatal_error(&harness.session.take_events());
}

#[test]
fn refused_seek_does_not_cancel_a_seek_already_landing_inside_the_window() {
    let (mut harness, _publisher) = playing_live_harness(305);
    let inside_target = Duration::from_secs(45);
    harness.start_final_seek(MediaTime::from_duration(inside_target));
    let landing_generation = harness
        .session
        .seek_commit()
        .expect("the inside seek is landing")
        .generation;
    let demux_seeks_before = harness.seek_request_log.lock().expect("seek log").len();
    let _events_before_refusal = harness.session.take_events();

    let outcome = harness
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(EXPIRED_TARGET)))
        .expect("refusal is not an error");

    assert!(matches!(outcome, PlayerCommandOutcome::Rejected(_)));
    let landing = harness
        .session
        .seek_commit()
        .expect("the refused command must not cancel the landing seek");
    assert_eq!(landing.generation, landing_generation);
    assert_eq!(
        landing.target_position,
        MediaTime::from_duration(inside_target)
    );
    assert_eq!(landing.resume_intent, PlaybackResumeIntent::Play);
    assert_eq!(
        harness.seek_request_log.lock().expect("seek log").len(),
        demux_seeks_before,
        "the refused command must not reach the demuxer"
    );

    land_video_seek_from(&mut harness, inside_target);

    let events = harness.session.take_events();
    assert_no_fatal_error(&events);
    assert_eq!(committed_targets(&events), vec![inside_target]);
    assert_eq!(harness.session.playback_state(), PlaybackState::Playing);
    assert_eq!(harness.session.snapshot().current_position, inside_target);
}

#[test]
fn window_sliding_past_a_landing_seek_continues_playing_at_the_live_edge() {
    let (mut harness, publisher) = playing_live_harness(306);
    let doomed_target = Duration::from_secs(25);
    harness.start_final_seek(MediaTime::from_duration(doomed_target));
    let media_instance = harness
        .session
        .snapshot()
        .media_instance_id
        .expect("installed live media");
    let (outcome_tx, outcome_rx) = bounded(4);
    // Exact receipt той же перемотки (как у MPRIS SetPosition) должен разрешиться ровно раз.
    harness.session.begin_exact_timeline_seek(
        ExactTimelineSeekRequest {
            request_id: TimelineSeekRequestId::new(NonZeroU64::new(7).expect("request id")),
            media_instance_id: media_instance,
            target: MediaTime::from_duration(doomed_target),
            kind: TimelineSeekKind::SetPosition,
        },
        outcome_tx,
    );
    let _events_before_slide = harness.session.take_events();

    slide_window(&mut harness, &publisher, 2, 27, 70);

    let live_edge = Duration::from_secs(70);
    let continuation = harness
        .session
        .seek_commit()
        .expect("playback continues through a seek to the live edge");
    assert_eq!(
        continuation.target_position,
        MediaTime::from_duration(live_edge)
    );
    assert_eq!(continuation.resume_intent, PlaybackResumeIntent::Play);
    assert_eq!(
        continuation.target_retention,
        SeekTargetRetention::LiveAvailability
    );
    assert!(matches!(
        outcome_rx.try_recv().expect("the expired receipt is resolved"),
        ExactTimelineSeekOutcome::Expired { requested_position, .. }
            if requested_position == MediaTime::from_duration(doomed_target)
    ));
    assert!(
        outcome_rx.try_recv().is_err(),
        "the receipt is resolved exactly once"
    );

    land_video_seek_from(&mut harness, live_edge);

    let events = harness.session.take_events();
    assert_no_fatal_error(&events);
    assert_eq!(
        recoverable_error_kinds(&events),
        vec![PlayerErrorKind::SeekTargetExpired]
    );
    assert_eq!(
        committed_targets(&events),
        vec![live_edge],
        "the expired target never commits; only the live edge does"
    );
    let session = &harness.session;
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(session.snapshot().current_position, live_edge);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(live_edge)
    );
}

#[test]
fn window_sliding_past_a_paused_landing_seek_stays_paused_at_the_live_edge() {
    let (mut harness, publisher) = paused_live_harness(307);
    harness.start_final_seek(MediaTime::from_secs(25));
    let _events_before_slide = harness.session.take_events();

    slide_window(&mut harness, &publisher, 2, 27, 70);

    let live_edge = Duration::from_secs(70);
    let continuation = harness
        .session
        .seek_commit()
        .expect("a paused viewer also gets a frame of the live edge");
    assert_eq!(continuation.resume_intent, PlaybackResumeIntent::Pause);
    land_video_seek_from(&mut harness, live_edge);

    let events = harness.session.take_events();
    assert_no_fatal_error(&events);
    assert_eq!(committed_targets(&events), vec![live_edge]);
    assert_eq!(harness.session.playback_state(), PlaybackState::Paused);
    assert_eq!(harness.session.snapshot().current_position, live_edge);
}

#[test]
fn window_sliding_before_the_worker_receipt_requeues_the_seek_at_the_live_edge() {
    let (mut harness, publisher) = playing_live_harness(308);
    let prepared_seek_port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased_port: Arc<dyn PreparedDemuxSeekPort> = prepared_seek_port.clone();
    harness
        .session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port: erased_port,
            landing_policy: PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        });
    harness.start_final_seek(MediaTime::from_secs(25));
    assert!(harness.session.prepared_demux_seek.receipt_pending());
    let doomed_request = prepared_seek_port.commands()[0].0;

    slide_window(&mut harness, &publisher, 2, 27, 70);

    let commands = prepared_seek_port.commands();
    assert_eq!(
        commands.len(),
        2,
        "the continuation goes through the worker port"
    );
    assert_eq!(commands[1].1.timestamp, Duration::from_secs(70));
    assert_eq!(
        harness.session.in_flight_seek_resume_intent(),
        Some(PlaybackResumeIntent::Play)
    );
    assert_eq!(harness.session.playback_state(), PlaybackState::Seeking);
    assert_no_fatal_error(&harness.session.take_events());

    // Поздний receipt протухшей цели больше ничего не коммитит.
    prepared_seek_port.complete(
        doomed_request,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(25),
            actual_position: MediaTime::from_secs(25),
            actual_track_timestamp: None,
        }),
    );
    harness.session.service_prepared_demux_seek_receipts();
    assert!(harness.session.seek_commit().is_none());
    assert!(harness.session.prepared_demux_seek.receipt_pending());

    prepared_seek_port.complete(
        commands[1].0,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(70),
            actual_position: MediaTime::from_secs(70),
            actual_track_timestamp: None,
        }),
    );
    harness.session.service_prepared_demux_seek_receipts();
    let continuation = harness
        .session
        .seek_commit()
        .expect("the continuation receipt opens the landing");
    assert_eq!(continuation.target_position, MediaTime::from_secs(70));
    assert_eq!(continuation.resume_intent, PlaybackResumeIntent::Play);
}

#[test]
fn refused_seek_does_not_supersede_a_seek_waiting_for_its_worker_receipt() {
    let (mut harness, _publisher) = playing_live_harness(309);
    let prepared_seek_port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased_port: Arc<dyn PreparedDemuxSeekPort> = prepared_seek_port.clone();
    harness
        .session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port: erased_port,
            landing_policy: PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        });
    let inside_target = MediaTime::from_secs(45);
    harness.start_final_seek(inside_target);
    let pending_request = prepared_seek_port.commands()[0].0;

    let outcome = harness
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(EXPIRED_TARGET)))
        .expect("refusal is not an error");

    assert!(matches!(outcome, PlayerCommandOutcome::Rejected(_)));
    assert_eq!(
        prepared_seek_port.commands().len(),
        1,
        "the refused command must not reach the worker port"
    );
    assert!(
        harness.session.prepared_demux_seek.receipt_pending(),
        "the refused command must not supersede the seek waiting for its receipt"
    );
    prepared_seek_port.complete(
        pending_request,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: inside_target,
            actual_position: inside_target,
            actual_track_timestamp: None,
        }),
    );
    harness.session.service_prepared_demux_seek_receipts();
    let landing = harness
        .session
        .seek_commit()
        .expect("the earlier seek still lands at its own target");
    assert_eq!(landing.target_position, inside_target);
    assert_eq!(landing.resume_intent, PlaybackResumeIntent::Play);
    assert_no_fatal_error(&harness.session.take_events());
}
