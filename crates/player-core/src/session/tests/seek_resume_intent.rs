//! Play/Pause/Toggle во время одноразового seek-а меняют resume intent, а не отменяют seek.
//!
//! Решение владельца (вариант A, 2026-10-20). Регрессия: Space сразу после стрелки
//! (seek ещё «садится») отменял SeekLanding как внешнюю команду — demuxer/decoder уже у
//! новой цели, а часы/позиция на старом месте, видео замерзало. Тесты идут реальным путём
//! session: команда -> tick -> reused-decoder landing -> fake decoder -> present -> commit.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use media_core::{DemuxSeekResult, MediaTime, PacketKeyframe, TrackKind};

use super::prepared_demux_seek::FakePreparedDemuxSeekPort;
use super::test_support::{
    SeekRegressionHarness, fake_track, fake_video_packet_with_keyframe, land_video_seek_from,
    paused_video_only_harness_at, playing_video_only_harness_at, scripted_seek_demuxer,
    seek_regression_fast_preroll_tick_config,
};
use crate::seek_state::PlaybackResumeIntent;
use crate::{
    MediaInstanceId, PlaybackIntent, PlaybackState, PlayerCommand, PlayerEvent, PlayerTickContext,
    PreparedDemuxSeekMode, PreparedDemuxSeekOutcome, PreparedDemuxSeekPort, SeekRequest,
};

/// Позиция, на которой играет/стоит media до пользовательского seek-а.
const OLD_POSITION: Duration = Duration::from_secs(6);

/// Цель пользовательского seek-а (стрелка/клик по timeline).
const SEEK_TARGET: Duration = Duration::from_secs(20);

fn media_instance_id(value: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(NonZeroU64::new(value).expect("non-zero test identity"))
}

/// Запускает обычный one-shot seek и проверяет, что он реально в полёте (landing не сел).
fn start_seek_in_flight(
    harness: &mut SeekRegressionHarness,
    expected_intent: PlaybackResumeIntent,
) {
    harness.start_final_seek(MediaTime::from_duration(SEEK_TARGET));
    let seek_commit = harness
        .session
        .seek_commit()
        .expect("one-shot seek must stay in flight until its landing frame is presented");
    assert_eq!(
        seek_commit.target_position,
        MediaTime::from_duration(SEEK_TARGET)
    );
    assert_eq!(seek_commit.resume_intent, expected_intent);
    let _events_before_command = harness.session.take_events();
}

/// Resume intent seek-а в полёте; падает, если команда seek отменила.
fn in_flight_resume_intent(harness: &SeekRegressionHarness) -> PlaybackResumeIntent {
    harness
        .session
        .seek_commit()
        .expect("Play/Pause must not cancel an in-flight one-shot seek")
        .resume_intent
}

/// Проверяет наблюдаемый результат landing-а: commit в цели, кадр цели на экране, часы там же.
fn assert_committed_at_target(harness: &SeekRegressionHarness, expected_state: PlaybackState) {
    let session = &harness.session;
    assert!(session.seek_commit().is_none(), "seek landing must commit");
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(SEEK_TARGET),
        "the landing frame of the new target must be on screen"
    );
    assert_eq!(session.playback_state(), expected_state);
    assert!(!session.snapshot().timeline.seeking);
    assert!(!session.snapshot().timeline.scrubbing);
    let position = session.snapshot().current_position;
    assert!(
        position >= SEEK_TARGET && position < SEEK_TARGET + Duration::from_secs(1),
        "public position must be at the new target, not at the old one: {position:?}"
    );
    let clock = session.presentation_clock_position_at(Instant::now());
    assert!(
        clock >= SEEK_TARGET && clock < SEEK_TARGET + Duration::from_secs(1),
        "presentation clock must continue from the new target, not from the old one: {clock:?}"
    );
}

/// Возвращает resume intent из единственного `SeekCommitted` события.
fn committed_resume_intent(events: &[PlayerEvent]) -> PlaybackResumeIntent {
    let committed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            PlayerEvent::SeekCommitted(info) => Some(*info),
            _ => None,
        })
        .collect();
    let [info] = committed.as_slice() else {
        panic!("exactly one SeekCommitted event expected, got {committed:?}");
    };
    assert_eq!(info.target_position, SEEK_TARGET);
    info.resume_intent
}

/// Последнее опубликованное `PlaybackStateChanged` — то, что увидит UI после commit-а.
fn last_published_state(events: &[PlayerEvent]) -> Option<PlaybackState> {
    events.iter().rev().find_map(|event| match event {
        PlayerEvent::PlaybackStateChanged(state) => Some(*state),
        _ => None,
    })
}

#[test]
fn pause_during_playing_seek_landing_lands_paused_at_new_target() {
    let instance_id = media_instance_id(21);
    let mut harness = playing_video_only_harness_at(instance_id, OLD_POSITION);
    start_seek_in_flight(&mut harness, PlaybackResumeIntent::Play);
    let state_while_landing = harness.session.playback_state();

    // Тот же путь, что Space в приложении: stable intent -> exact installed instance.
    assert!(
        harness
            .session
            .apply_playback_intent_to_exact_installed_instance(
                instance_id,
                PlaybackIntent::StartPaused,
            )
    );

    assert_eq!(
        in_flight_resume_intent(&harness),
        PlaybackResumeIntent::Pause
    );
    assert_eq!(
        harness.session.playback_state(),
        state_while_landing,
        "public state stays the in-flight seek state until commit"
    );

    land_video_seek_from(&mut harness, SEEK_TARGET);

    assert_committed_at_target(&harness, PlaybackState::Paused);
    assert_eq!(harness.session.snapshot().current_position, SEEK_TARGET);
    let events = harness.session.take_events();
    assert_eq!(
        committed_resume_intent(&events),
        PlaybackResumeIntent::Pause
    );
    assert_eq!(last_published_state(&events), Some(PlaybackState::Paused));
}

#[test]
fn play_during_paused_seek_landing_lands_playing_and_advances_from_new_target() {
    let mut harness = paused_video_only_harness_at(media_instance_id(22), OLD_POSITION);
    start_seek_in_flight(&mut harness, PlaybackResumeIntent::Pause);

    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .expect("Play must reach the session");

    assert_eq!(
        in_flight_resume_intent(&harness),
        PlaybackResumeIntent::Play
    );

    land_video_seek_from(&mut harness, SEEK_TARGET);

    assert_committed_at_target(&harness, PlaybackState::Playing);
    let events = harness.session.take_events();
    assert_eq!(committed_resume_intent(&events), PlaybackResumeIntent::Play);
    assert_eq!(last_published_state(&events), Some(PlaybackState::Playing));

    // Playback реально идёт от новой цели: позже по часам на экран выходит следующий
    // кадр после цели, а не кадр старой позиции и не замёрзший landing.
    let _advance_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now() + Duration::from_millis(80),
        seek_regression_fast_preroll_tick_config(),
    ));
    let advanced_pts = harness
        .session
        .pipeline
        .present_video_frame()
        .map(|frame| frame.pts)
        .expect("a frame must stay on screen while playing");
    assert!(
        advanced_pts > SEEK_TARGET && advanced_pts < SEEK_TARGET + Duration::from_secs(1),
        "playback must advance from the new target: {advanced_pts:?}"
    );
}

#[test]
fn double_toggle_during_one_seek_restores_original_intent() {
    let mut harness = playing_video_only_harness_at(media_instance_id(23), OLD_POSITION);
    start_seek_in_flight(&mut harness, PlaybackResumeIntent::Play);

    harness
        .session
        .dispatch_command(PlayerCommand::TogglePlayback)
        .expect("first toggle must reach the session");
    assert_eq!(
        in_flight_resume_intent(&harness),
        PlaybackResumeIntent::Pause,
        "toggle flips the pending resume intent, not the transient public state"
    );

    harness
        .session
        .dispatch_command(PlayerCommand::TogglePlayback)
        .expect("second toggle must reach the session");
    assert_eq!(
        in_flight_resume_intent(&harness),
        PlaybackResumeIntent::Play
    );

    land_video_seek_from(&mut harness, SEEK_TARGET);

    assert_committed_at_target(&harness, PlaybackState::Playing);
}

#[test]
fn single_toggle_during_playing_seek_lands_paused() {
    let mut harness = playing_video_only_harness_at(media_instance_id(24), OLD_POSITION);
    start_seek_in_flight(&mut harness, PlaybackResumeIntent::Play);

    harness
        .session
        .dispatch_command(PlayerCommand::TogglePlayback)
        .expect("toggle must reach the session");
    land_video_seek_from(&mut harness, SEEK_TARGET);

    assert_committed_at_target(&harness, PlaybackState::Paused);
}

#[test]
fn new_seek_after_pause_inherits_retargeted_intent() {
    let mut harness = playing_video_only_harness_at(media_instance_id(25), OLD_POSITION);
    start_seek_in_flight(&mut harness, PlaybackResumeIntent::Play);
    harness
        .session
        .dispatch_command(PlayerCommand::Pause)
        .expect("Pause must reach the session");

    // Вторая стрелка поверх незавершённого seek-а: старый landing заменяется новым,
    // и намерение пользователя (Pause) обязано перейти к нему, а не вернуться к Play.
    let superseding_target = SEEK_TARGET + Duration::from_secs(5);
    harness.start_final_seek(MediaTime::from_duration(superseding_target));

    let seek_commit = harness
        .session
        .seek_commit()
        .expect("superseding seek must be in flight");
    assert_eq!(
        seek_commit.target_position,
        MediaTime::from_duration(superseding_target)
    );
    assert_eq!(seek_commit.resume_intent, PlaybackResumeIntent::Pause);
}

#[test]
fn stop_during_seek_landing_still_cancels_the_seek() {
    let mut harness = playing_video_only_harness_at(media_instance_id(26), OLD_POSITION);
    start_seek_in_flight(&mut harness, PlaybackResumeIntent::Play);

    harness
        .session
        .dispatch_command(PlayerCommand::Stop)
        .expect("Stop must reach the session");

    assert!(harness.session.seek_commit().is_none());
    assert_eq!(harness.session.playback_state(), PlaybackState::Stopped);
    assert!(
        harness
            .session
            .take_events()
            .iter()
            .all(|event| !matches!(event, PlayerEvent::SeekCommitted(_))),
        "a stopped seek must never commit"
    );
}

/// Worker-receipted seek до receipt-а не имеет ни commit-а, ни landing-а: намерение живёт
/// только в pending request-е. Pause в этом окне раньше ставил `Paused` мимо seek-а, а
/// receipt потом всё равно возобновлял Playing по старому intent-у.
#[test]
fn pause_before_worker_receipt_lands_paused_at_target_frame() {
    let actual_position = Duration::from_secs(15);
    let video_track = fake_track(1, TrackKind::Video);
    let packets = vec![
        fake_video_packet_with_keyframe(video_track.id, actual_position, PacketKeyframe::Keyframe),
        fake_video_packet_with_keyframe(video_track.id, SEEK_TARGET, PacketKeyframe::NotKeyframe),
    ];
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        SEEK_TARGET,
        actual_position,
        packets,
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);
    let port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased_port: Arc<dyn PreparedDemuxSeekPort> = port.clone();
    harness
        .session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port: erased_port,
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        });
    harness
        .decoder
        .decode_next_packet_as_frame(actual_position, 931);
    harness
        .decoder
        .decode_next_packet_as_frame(SEEK_TARGET, 932);
    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .expect("Play must reach the session");

    harness
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_duration(SEEK_TARGET),
        )))
        .expect("receipted seek command");
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(harness.session.playback_state(), PlaybackState::Seeking);

    harness
        .session
        .dispatch_command(PlayerCommand::Pause)
        .expect("Pause must reach the session");

    assert_eq!(
        harness.session.playback_state(),
        PlaybackState::Seeking,
        "Pause must not leave the seek pretending to be a settled Paused state"
    );
    assert_eq!(
        harness.session.prepared_demux_seek.pending_resume_intent(),
        Some(PlaybackResumeIntent::Pause)
    );

    let commands = port.commands();
    let [(request_id, _request)] = commands.as_slice() else {
        panic!("exactly one worker seek request expected");
    };
    port.complete(
        *request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_duration(SEEK_TARGET),
            actual_position: MediaTime::from_duration(actual_position),
            actual_track_timestamp: None,
        }),
    );
    harness.session.service_prepared_demux_seek_receipts();
    assert_eq!(
        harness
            .session
            .seek_commit()
            .map(|commit| commit.resume_intent),
        Some(PlaybackResumeIntent::Pause),
        "receipt must carry the retargeted intent into the commit"
    );

    for _ in 0..6 {
        harness.tick_once_fast_preroll();
        if harness.session.seek_commit().is_none() {
            break;
        }
    }

    assert!(harness.session.seek_commit().is_none());
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(SEEK_TARGET)
    );
    assert_eq!(harness.session.playback_state(), PlaybackState::Paused);
    assert_eq!(harness.session.snapshot().current_position, SEEK_TARGET);
}

/// Один drag по timeline: begin -> preview в цель -> release с commit-ом последней цели.
fn drag_timeline_to(harness: &mut SeekRegressionHarness, target: Duration) {
    let session = &mut harness.session;
    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .expect("BeginScrub must reach the session");
    session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_duration(target),
        )))
        .expect("PreviewScrub must reach the session");
    session
        .dispatch_command(PlayerCommand::end_scrub(
            crate::ScrubCommitPolicy::CommitLatestTarget,
        ))
        .expect("EndScrub must reach the session");
}

/// Resume intent финального commit-а второго drag-а, пока его landing ещё в полёте.
fn in_flight_intent_after_double_drag(mut harness: SeekRegressionHarness) -> PlaybackResumeIntent {
    drag_timeline_to(&mut harness, SEEK_TARGET - Duration::from_secs(4));
    assert!(
        harness.session.in_flight_seek_resume_intent().is_some(),
        "the first drag commit must still be in flight (slow decoder)"
    );
    // Второй drag стартует, пока commit первого не сел.
    drag_timeline_to(&mut harness, SEEK_TARGET);
    harness
        .session
        .in_flight_seek_resume_intent()
        .expect("the second drag commit must be in flight")
}

#[test]
fn second_drag_over_in_flight_commit_while_playing_keeps_play_intent() {
    let harness = playing_video_only_harness_at(media_instance_id(40), OLD_POSITION);
    assert_eq!(
        in_flight_intent_after_double_drag(harness),
        PlaybackResumeIntent::Play,
        "второй drag обязан унаследовать Play от commit-а первого"
    );
}

#[test]
fn second_drag_over_in_flight_commit_while_paused_stays_paused() {
    let harness = paused_video_only_harness_at(media_instance_id(41), OLD_POSITION);
    assert_eq!(
        in_flight_intent_after_double_drag(harness),
        PlaybackResumeIntent::Pause
    );
}

#[test]
fn single_drag_while_playing_commits_with_play_intent_and_resumes_playing() {
    let mut harness = playing_video_only_harness_at(media_instance_id(42), OLD_POSITION);
    drag_timeline_to(&mut harness, SEEK_TARGET);
    assert_eq!(
        harness.session.in_flight_seek_resume_intent(),
        Some(PlaybackResumeIntent::Play)
    );
    land_video_seek_from(&mut harness, SEEK_TARGET);
    assert_committed_at_target(&harness, PlaybackState::Playing);
}

#[test]
fn double_drag_while_playing_lands_playing_after_commit() {
    let mut harness = playing_video_only_harness_at(media_instance_id(43), OLD_POSITION);
    drag_timeline_to(&mut harness, SEEK_TARGET - Duration::from_secs(4));
    drag_timeline_to(&mut harness, SEEK_TARGET);
    land_video_seek_from(&mut harness, SEEK_TARGET);
    assert_committed_at_target(&harness, PlaybackState::Playing);
}
