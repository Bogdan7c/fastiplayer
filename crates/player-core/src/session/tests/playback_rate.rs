use std::sync::Arc;

use super::test_support::*;
use super::*;
use crate::{PlaybackRate, PlaybackRateValidationError, PlayerCommandOutcome, PlayerCommandReject};

fn playback_rate(multiplier: f32) -> PlaybackRate {
    PlaybackRate::new(multiplier).expect("test playback rate must be valid")
}

fn assert_rate_command_applied(result: PlayerResult<PlayerCommandOutcome>) -> PlayerCommandOutcome {
    let outcome = result.expect("rate command must not be fatal");
    assert_eq!(outcome, PlayerCommandOutcome::Applied);
    outcome
}

fn fake_prepared_media(label: &str) -> PreparedMedia {
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let demuxer = FakeDemuxer::new(Vec::new(), Some(Duration::from_secs(30)), seek_log);
    PreparedMedia::from_external_label(label, Box::new(demuxer))
}

#[test]
fn default_playback_rate_is_one_x() {
    let session = PlayerSession::new();

    assert_eq!(PlaybackRate::default(), PlaybackRate::NORMAL);
    assert_eq!(
        PlayerSnapshot::default().playback_rate,
        PlaybackRate::NORMAL
    );
    assert_eq!(session.snapshot().playback_rate, PlaybackRate::NORMAL);
    assert_eq!(session.snapshot().playback_rate.as_f32(), 1.0);
}

#[test]
fn playback_rate_validation_rejects_invalid_values() {
    let invalid_values = [
        (0.24, PlaybackRateValidationError::BelowMinimum),
        (4.01, PlaybackRateValidationError::AboveMaximum),
        (f32::NAN, PlaybackRateValidationError::NotFinite),
        (f32::INFINITY, PlaybackRateValidationError::NotFinite),
        (f32::NEG_INFINITY, PlaybackRateValidationError::NotFinite),
        (0.0, PlaybackRateValidationError::NonPositive),
        (-1.0, PlaybackRateValidationError::NonPositive),
    ];

    for (raw_multiplier, expected_error) in invalid_values {
        assert_eq!(PlaybackRate::new(raw_multiplier), Err(expected_error));
    }
}

#[test]
fn playback_rate_validation_accepts_boundary_values() {
    assert_eq!(PlaybackRate::new(0.25), Ok(PlaybackRate::MIN));
    assert_eq!(PlaybackRate::new(4.0), Ok(PlaybackRate::MAX));
}

#[test]
fn playback_rate_command_updates_snapshot_while_playing() {
    let mut session = PlayerSession::new();
    let requested_rate = playback_rate(1.5);

    session.set_playback_state(PlaybackState::Playing);
    assert_rate_command_applied(
        session.dispatch_command(PlayerCommand::SetPlaybackRate(requested_rate)),
    );

    assert_eq!(session.snapshot().playback_rate, requested_rate);
}

#[test]
fn paused_playback_rate_command_updates_snapshot_without_advancing_media_time() {
    let mut session = PlayerSession::new();
    let requested_rate = playback_rate(0.75);
    let position_before_command = Duration::from_secs(12);

    session.set_playback_state(PlaybackState::Paused);
    session.update_current_position(position_before_command);
    assert_rate_command_applied(
        session.dispatch_command(PlayerCommand::SetPlaybackRate(requested_rate)),
    );

    assert_eq!(session.snapshot().playback_rate, requested_rate);
    assert_eq!(session.snapshot().current_position, position_before_command);
    assert_eq!(
        session.snapshot().timeline.current_position,
        MediaTime::from_duration(position_before_command)
    );
}

#[test]
fn blocked_states_reject_playback_rate_without_mutating_snapshot_or_error_state() {
    let mut session = PlayerSession::new();
    let original_rate = playback_rate(1.5);
    let rejected_rate = playback_rate(2.0);
    let blocked_states = [
        PlaybackState::Idle,
        PlaybackState::Opening,
        PlaybackState::Buffering,
        PlaybackState::Seeking,
        PlaybackState::Scrubbing,
        PlaybackState::Draining,
        PlaybackState::Ended,
        PlaybackState::Stopped,
        PlaybackState::Failed,
    ];

    session.set_playback_state(PlaybackState::Playing);
    assert_rate_command_applied(
        session.dispatch_command(PlayerCommand::SetPlaybackRate(original_rate)),
    );

    for blocked_state in blocked_states {
        session.set_playback_state(blocked_state);
        let _ = session.take_events();

        let outcome = session
            .dispatch_command(PlayerCommand::SetPlaybackRate(rejected_rate))
            .expect("blocked playback-rate command must be a typed reject, not fatal");

        assert_eq!(
            outcome,
            PlayerCommandOutcome::Rejected(PlayerCommandReject::PlaybackRateUnavailableForState {
                state: blocked_state,
            })
        );
        assert_eq!(session.snapshot().playback_rate, original_rate);
        assert!(session.snapshot().last_error.is_none());
        assert!(session.take_events().is_empty());
    }
}

#[test]
fn buffering_entered_from_playing_rejects_playback_rate() {
    let mut session = PlayerSession::new();
    let original_rate = playback_rate(1.25);
    let rejected_rate = playback_rate(1.75);

    session.set_playback_state(PlaybackState::Playing);
    assert_rate_command_applied(
        session.dispatch_command(PlayerCommand::SetPlaybackRate(original_rate)),
    );
    session.set_playback_state(PlaybackState::Buffering);

    let outcome = session
        .dispatch_command(PlayerCommand::SetPlaybackRate(rejected_rate))
        .expect("buffering reject must not be fatal");

    assert_eq!(
        outcome,
        PlayerCommandOutcome::Rejected(PlayerCommandReject::PlaybackRateUnavailableForState {
            state: PlaybackState::Buffering,
        })
    );
    assert_eq!(session.snapshot().playback_rate, original_rate);
}

#[test]
fn new_media_load_resets_playback_rate_to_one_x() {
    let mut session = PlayerSession::new();
    let non_default_rate = playback_rate(2.0);

    session.set_playback_state(PlaybackState::Playing);
    assert_rate_command_applied(
        session.dispatch_command(PlayerCommand::SetPlaybackRate(non_default_rate)),
    );
    session.load_prepared_media_with_autoplay(fake_prepared_media("next media"), false);

    assert_eq!(session.snapshot().playback_rate, PlaybackRate::NORMAL);
}
