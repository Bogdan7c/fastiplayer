use std::num::NonZeroU64;
use std::time::Duration;

use media_core::{DemuxSeekability, TimelineNotSeekableReason};

use super::test_support::{
    ScriptedAudioOutput, SeekRegressionHarness, install_fake_media,
    install_fake_media_with_seekability, land_video_seek_from, playing_video_only_harness_at,
};
use super::*;
use crate::{
    ExactMediaTransportAction, ExactMediaTransportFailureStage, ExactMediaTransportOutcome,
    ExactMediaTransportRequest, MediaInstanceId, PlaybackIntent,
};

fn media_instance_id(value: u64) -> MediaInstanceId {
    MediaInstanceId::from_non_zero(NonZeroU64::new(value).expect("non-zero test identity"))
}

fn request(
    media_instance_id: MediaInstanceId,
    action: ExactMediaTransportAction,
) -> ExactMediaTransportRequest {
    ExactMediaTransportRequest {
        media_instance_id,
        action,
    }
}

fn session_with_exact_media(instance_id: MediaInstanceId) -> PlayerSession {
    let mut session = PlayerSession::default();
    install_fake_media(&mut session, Vec::new());
    session.snapshot.media_instance_id = Some(instance_id);
    session
}

#[test]
fn exact_transport_rejects_stale_instance_without_touching_current() {
    let current_instance_id = media_instance_id(1);
    let stale_instance_id = media_instance_id(2);
    let mut session = session_with_exact_media(current_instance_id);
    session.set_playback_state(PlaybackState::Playing);

    assert_eq!(
        session.apply_exact_media_transport(request(
            stale_instance_id,
            ExactMediaTransportAction::NeutralStop,
        )),
        ExactMediaTransportOutcome::StaleInstance {
            requested_media_instance_id: stale_instance_id,
            current_media_instance_id: Some(current_instance_id),
        }
    );
    assert_eq!(session.playback_state(), PlaybackState::Playing);
}

#[test]
fn reset_media_fully_clears_matching_instance() {
    let instance_id = media_instance_id(20);
    let mut session = session_with_exact_media(instance_id);
    session.set_playback_state(PlaybackState::Playing);

    assert_eq!(
        session.apply_exact_media_transport(request(
            instance_id,
            ExactMediaTransportAction::ResetMedia,
        )),
        ExactMediaTransportOutcome::Applied {
            media_instance_id: instance_id,
        }
    );
    assert_eq!(session.snapshot.media_instance_id, None);
    assert_eq!(session.snapshot.source_label, None);
    assert_eq!(session.snapshot.current_video_frame, None);
    assert_eq!(session.playback_state(), PlaybackState::Stopped);
    assert!(!session.pipeline.has_demuxer());
}

#[test]
fn stale_reset_media_does_not_touch_newer_instance() {
    let current_instance_id = media_instance_id(21);
    let stale_instance_id = media_instance_id(22);
    let mut session = session_with_exact_media(current_instance_id);
    session.set_playback_state(PlaybackState::Playing);

    assert_eq!(
        session.apply_exact_media_transport(request(
            stale_instance_id,
            ExactMediaTransportAction::ResetMedia,
        )),
        ExactMediaTransportOutcome::StaleInstance {
            requested_media_instance_id: stale_instance_id,
            current_media_instance_id: Some(current_instance_id),
        }
    );
    assert_eq!(
        session.snapshot.media_instance_id,
        Some(current_instance_id)
    );
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert!(session.pipeline.has_demuxer());
}

#[test]
fn neutral_stop_pauses_then_seeks_matching_instance_without_destructive_reset() {
    let instance_id = media_instance_id(3);
    let mut session = session_with_exact_media(instance_id);
    session.set_playback_state(PlaybackState::Playing);

    assert_eq!(
        session.apply_exact_media_transport(request(
            instance_id,
            ExactMediaTransportAction::NeutralStop,
        )),
        ExactMediaTransportOutcome::Applied {
            media_instance_id: instance_id,
        }
    );
    assert_eq!(session.snapshot.media_instance_id, Some(instance_id));
    assert!(matches!(session.playback_state(), PlaybackState::Seeking));
}

#[test]
fn neutral_stop_reports_pause_failure_without_starting_seek() {
    let instance_id = media_instance_id(4);
    let mut session = session_with_exact_media(instance_id);
    session.pipeline.install_audio_output_for_tests(Box::new(
        ScriptedAudioOutput::with_pause_error(0.0, "pause failed"),
    ));

    let outcome = session
        .apply_exact_media_transport(request(instance_id, ExactMediaTransportAction::NeutralStop));
    assert!(matches!(
        outcome,
        ExactMediaTransportOutcome::Failed {
            media_instance_id,
            stage: ExactMediaTransportFailureStage::Pause,
            ..
        } if media_instance_id == instance_id
    ));
    assert_eq!(session.snapshot.current_position, std::time::Duration::ZERO);
}

#[test]
fn neutral_stop_reports_partial_result_when_pause_succeeds_but_seek_fails() {
    let instance_id = media_instance_id(5);
    let mut session = PlayerSession::default();
    install_fake_media_with_seekability(
        &mut session,
        Vec::new(),
        DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::SourceNotSeekable,
        },
    );
    session.snapshot.media_instance_id = Some(instance_id);
    session.snapshot.current_position = std::time::Duration::from_secs(10);
    session.set_playback_state(PlaybackState::Playing);

    let outcome = session
        .apply_exact_media_transport(request(instance_id, ExactMediaTransportAction::NeutralStop));
    assert!(
        matches!(
            outcome,
            ExactMediaTransportOutcome::PartiallyApplied {
                media_instance_id,
                completed_stage: ExactMediaTransportFailureStage::Pause,
                failed_stage: ExactMediaTransportFailureStage::SeekToBeginning,
                ..
            } if media_instance_id == instance_id
        ),
        "unexpected outcome: {outcome:?}"
    );
    assert_eq!(session.playback_state(), PlaybackState::Paused);
}

#[test]
fn restart_from_ended_uses_existing_replay_boundary_and_keeps_exact_instance() {
    let instance_id = media_instance_id(6);
    let mut session = session_with_exact_media(instance_id);
    session.set_playback_state(PlaybackState::Ended);

    assert_eq!(
        session.apply_exact_media_transport(request(
            instance_id,
            ExactMediaTransportAction::RestartFromBeginning {
                intent: PlaybackIntent::StartPlaying,
            },
        )),
        ExactMediaTransportOutcome::Applied {
            media_instance_id: instance_id,
        }
    );
    assert_eq!(session.snapshot.media_instance_id, Some(instance_id));
    assert_eq!(session.playback_state(), PlaybackState::Seeking);
}

#[test]
fn exact_play_pause_intent_never_falls_through_to_newer_instance() {
    let stale_instance_id = media_instance_id(6);
    let newer_instance_id = media_instance_id(7);
    let mut session = session_with_exact_media(newer_instance_id);
    let state_before = session.playback_state();

    // Play/Pause, адресованный старому instance, не должен «провалиться»
    // на более новый: команда отклоняется, состояние нового media не меняется.
    assert_eq!(
        session.apply_exact_media_transport(request(
            stale_instance_id,
            ExactMediaTransportAction::SetPlaybackIntent {
                intent: PlaybackIntent::StartPlaying,
            },
        )),
        ExactMediaTransportOutcome::StaleInstance {
            requested_media_instance_id: stale_instance_id,
            current_media_instance_id: Some(newer_instance_id),
        }
    );
    assert_eq!(session.playback_state(), state_before);
    assert_ne!(session.playback_state(), PlaybackState::Playing);

    // Та же команда для актуального instance применяется.
    assert_eq!(
        session.apply_exact_media_transport(request(
            newer_instance_id,
            ExactMediaTransportAction::SetPlaybackIntent {
                intent: PlaybackIntent::StartPlaying,
            },
        )),
        ExactMediaTransportOutcome::Applied {
            media_instance_id: newer_instance_id,
        }
    );
    assert_eq!(session.playback_state(), PlaybackState::Playing);
}

/// Доводит restart-landing до commit-а: декодер отдаёт кадры с нуля, session тикает.
fn land_restart_at_zero(harness: &mut SeekRegressionHarness) {
    land_video_seek_from(harness, Duration::ZERO);
}

#[test]
fn restart_from_playing_video_lands_at_zero_and_resumes_playing() {
    let instance_id = media_instance_id(8);
    let old_position = Duration::from_secs(6);
    let mut harness = playing_video_only_harness_at(instance_id, old_position);

    assert_eq!(
        harness.session.apply_exact_media_transport(request(
            instance_id,
            ExactMediaTransportAction::RestartFromBeginning {
                intent: PlaybackIntent::StartPlaying,
            },
        )),
        ExactMediaTransportOutcome::Applied {
            media_instance_id: instance_id,
        }
    );
    // Seek к нулю остаётся активным: собственный Play restart-а не имеет права его отменить.
    let restart_seek = harness
        .session
        .seek_commit()
        .expect("restart must keep its seek-to-zero transaction in flight");
    assert_eq!(
        restart_seek.target_position,
        MediaTime::from_duration(Duration::ZERO)
    );
    assert_eq!(restart_seek.resume_intent, PlaybackResumeIntent::Play);

    land_restart_at_zero(&mut harness);

    // Наблюдаемый результат: commit состоялся, на экране кадр с нуля, позиция и часы
    // у начала файла, playback снова идёт (а не застрял на старой позиции).
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::ZERO)
    );
    assert_eq!(harness.session.playback_state(), PlaybackState::Playing);
    assert!(!harness.session.snapshot().timeline.seeking);
    assert!(!harness.session.snapshot().timeline.scrubbing);
    assert!(harness.session.snapshot().current_position < Duration::from_secs(1));
    assert!(
        harness
            .session
            .presentation_clock_position_at(Instant::now())
            < Duration::from_secs(1),
        "presentation clock must restart from zero, not continue from the old position"
    );
    assert_eq!(
        harness
            .seek_requests()
            .last()
            .map(|request| request.timestamp),
        Some(Duration::ZERO)
    );
}

#[test]
fn restart_with_paused_intent_from_playing_video_lands_at_zero_paused() {
    let instance_id = media_instance_id(9);
    let mut harness = playing_video_only_harness_at(instance_id, Duration::from_secs(6));

    assert_eq!(
        harness.session.apply_exact_media_transport(request(
            instance_id,
            ExactMediaTransportAction::RestartFromBeginning {
                intent: PlaybackIntent::StartPaused,
            },
        )),
        ExactMediaTransportOutcome::Applied {
            media_instance_id: instance_id,
        }
    );
    let restart_seek = harness
        .session
        .seek_commit()
        .expect("restart must keep its seek-to-zero transaction in flight");
    assert_eq!(restart_seek.resume_intent, PlaybackResumeIntent::Pause);

    land_restart_at_zero(&mut harness);

    assert!(harness.session.seek_commit().is_none());
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::ZERO)
    );
    assert_eq!(harness.session.playback_state(), PlaybackState::Paused);
    assert_eq!(harness.session.snapshot().current_position, Duration::ZERO);
}
