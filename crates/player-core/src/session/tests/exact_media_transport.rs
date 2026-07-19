use std::num::NonZeroU64;

use media_core::{DemuxSeekability, TimelineNotSeekableReason};

use super::test_support::{
    ScriptedAudioOutput, install_fake_media, install_fake_media_with_seekability,
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
    let instance_id = media_instance_id(7);
    let mut session = session_with_exact_media(instance_id);
    assert_eq!(
        session.apply_exact_media_transport(request(
            instance_id,
            ExactMediaTransportAction::SetPlaybackIntent {
                intent: PlaybackIntent::StartPlaying,
            },
        )),
        ExactMediaTransportOutcome::Applied {
            media_instance_id: instance_id,
        }
    );
    assert_eq!(session.playback_state(), PlaybackState::Playing);
}
