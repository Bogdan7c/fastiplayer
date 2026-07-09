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

fn fake_prepared_media_with_seek_log(label: &str) -> (PreparedMedia, Arc<Mutex<Vec<Duration>>>) {
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let demuxer = FakeDemuxer::new(
        Vec::new(),
        Some(Duration::from_secs(30)),
        Arc::clone(&seek_log),
    );

    (
        PreparedMedia::from_external_label(label, Box::new(demuxer)),
        seek_log,
    )
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
fn playing_playback_rate_change_reanchors_no_audio_clock_without_seek_transaction() {
    let mut session = PlayerSession::new();
    let requested_rate = playback_rate(2.0);
    let initial_position = Duration::from_millis(100);
    let (prepared_media, seek_log) = fake_prepared_media_with_seek_log("rate-change media");

    session.load_prepared_media_with_autoplay(prepared_media, false);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.update_current_position(initial_position);
    let seek_generation_before_command = session.pipeline.seek_generation();

    assert_rate_command_applied(
        session.dispatch_command(PlayerCommand::SetPlaybackRate(requested_rate)),
    );

    let position_after_forty_wall_ms =
        session.presentation_clock_position_at(Instant::now() + Duration::from_millis(40));

    assert_eq!(session.snapshot().playback_rate, requested_rate);
    assert!(
        position_after_forty_wall_ms >= initial_position + Duration::from_millis(80),
        "2x no-audio clock must advance by at least the scaled wall delta: {position_after_forty_wall_ms:?}"
    );
    assert_eq!(
        session.pipeline.seek_generation(),
        seek_generation_before_command
    );
    assert!(
        seek_log
            .lock()
            .expect("seek log mutex should not be poisoned")
            .is_empty()
    );
}

#[test]
fn playing_half_rate_change_reanchors_no_audio_clock_slower_than_wall_time() {
    let mut session = PlayerSession::new();
    let requested_rate = playback_rate(0.5);
    let initial_position = Duration::from_millis(100);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.update_current_position(initial_position);
    assert_rate_command_applied(
        session.dispatch_command(PlayerCommand::SetPlaybackRate(requested_rate)),
    );

    let position_after_one_wall_sec =
        session.presentation_clock_position_at(Instant::now() + Duration::from_secs(1));

    assert!(
        position_after_one_wall_sec < initial_position + Duration::from_secs(1),
        "0.5x no-audio clock must advance slower than raw wall time: {position_after_one_wall_sec:?}"
    );
    assert!(
        position_after_one_wall_sec >= initial_position + Duration::from_millis(500),
        "0.5x no-audio clock must still advance by the scaled wall delta: {position_after_one_wall_sec:?}"
    );
}

#[test]
fn playing_playback_rate_change_reanchors_audio_clock_mapping_without_seek_transaction() {
    let mut session = PlayerSession::new();
    let requested_rate = playback_rate(2.0);
    let initial_position = Duration::from_secs(5);
    let audio_clock = Arc::new(ScriptedAudioClock::new());
    let (prepared_media, seek_log) = fake_prepared_media_with_seek_log("audio rate-change media");

    session.load_prepared_media_with_autoplay(prepared_media, false);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .pipeline
        .install_audio_clock(audio_clock.as_player_clock());
    session
        .pipeline
        .reanchor_audio_clock_media_mapping(initial_position, PlaybackRate::NORMAL);
    session.update_current_position(initial_position);
    let seek_generation_before_command = session.pipeline.seek_generation();
    let timing_reads_before_command = audio_clock.output_timing_read_count();

    assert_rate_command_applied(
        session.dispatch_command(PlayerCommand::SetPlaybackRate(requested_rate)),
    );
    assert_eq!(
        audio_clock.output_timing_read_count(),
        timing_reads_before_command + 1,
        "rate transaction должна использовать один timing snapshot для current media и tail"
    );
    audio_clock.record_played(96_000);

    assert_eq!(
        session.presentation_clock_position_at(Instant::now()),
        Duration::from_secs(7)
    );
    assert_eq!(
        session.pipeline.seek_generation(),
        seek_generation_before_command
    );
    assert!(
        seek_log
            .lock()
            .expect("seek log mutex should not be poisoned")
            .is_empty()
    );
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
fn play_audio_clock_advance_pause_rate_change_play_preserves_frozen_position() {
    let mut session = PlayerSession::new();
    let audio_output = ScriptedAudioOutput::new(0.0, None);
    let audio_output_handle = audio_output.handle.clone();
    let audio_clock = Arc::clone(&audio_output_handle.clock);
    let initial_media_position = Duration::from_secs(5);
    let requested_rate = playback_rate(2.0);

    session.load_prepared_media_with_autoplay(fake_prepared_media("pause clock regression"), false);
    session
        .dispatch_command(PlayerCommand::Play)
        .expect("play command should succeed");
    session
        .pipeline
        .install_audio_output_for_tests(Box::new(audio_output));
    session
        .pipeline
        .install_audio_clock(audio_clock.as_player_clock());
    session
        .pipeline
        .reanchor_audio_clock_media_mapping(initial_media_position, PlaybackRate::NORMAL);
    session.update_current_position(initial_media_position);

    // 96 000 interleaved stereo samples соответствуют одной секунде output clock.
    audio_clock.record_played(96_000);
    session
        .dispatch_command(PlayerCommand::Pause)
        .expect("pause command should succeed");

    let frozen_pause_position = Duration::from_secs(6);
    assert_eq!(session.snapshot().current_position, frozen_pause_position);

    // Даже если scripted source пытается продвинуться во время pause, output
    // boundary продолжает возвращать атомарно frozen coordinate.
    audio_clock.record_played(192_000);
    assert_eq!(audio_clock.now(), Duration::from_secs(1));
    assert_rate_command_applied(
        session.dispatch_command(PlayerCommand::SetPlaybackRate(requested_rate)),
    );
    assert_eq!(session.snapshot().current_position, frozen_pause_position);

    session
        .dispatch_command(PlayerCommand::Play)
        .expect("resume command should succeed");
    // Ещё 0.5 секунды output clock после re-anchor дают ровно 1 секунду media при 2x.
    audio_clock.record_played(144_000);
    assert_eq!(
        session.presentation_clock_position_at(Instant::now()),
        Duration::from_secs(7)
    );
}

#[test]
fn pause_backend_error_does_not_publish_paused_snapshot() {
    let mut session = PlayerSession::new();
    let initial_media_position = Duration::from_secs(5);

    session.load_prepared_media_with_autoplay(fake_prepared_media("pause error"), false);
    session
        .dispatch_command(PlayerCommand::Play)
        .expect("play command should succeed");
    session.pipeline.install_audio_output_for_tests(Box::new(
        ScriptedAudioOutput::with_pause_error(0.0, "scripted pause failure"),
    ));
    session.update_current_position(initial_media_position);

    let pause_error = session
        .dispatch_command(PlayerCommand::Pause)
        .expect_err("real backend pause error должна выйти из session transaction");

    assert_eq!(pause_error.kind, PlayerErrorKind::RuntimeError);
    assert_eq!(session.playback_state(), PlaybackState::Playing);
    assert_eq!(session.snapshot().current_position, initial_media_position);
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
