//! Шаги громкости: какая команда уходит player-у и как её увидит сохранение (UX 11).

use super::*;
use crate::user_audio_level::{MuteState, PlayerAudioObservation, UserAudioLevel, VolumePermille};

fn snapshot(volume: f32, muted: bool) -> PlayerSnapshot {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.volume = volume;
    snapshot.muted = muted;
    snapshot
}

const FALLBACK: f32 = 0.6;

#[test]
fn step_up_and_down_move_by_the_named_step_constant() {
    let step = VOLUME_STEP_PERMILLE as f32 / 1_000.0;

    assert_eq!(
        volume_command_from_steps(&snapshot(0.5, false), FALLBACK, 1),
        Some(PlayerCommand::SetVolume(0.5 + step))
    );
    assert_eq!(
        volume_command_from_steps(&snapshot(0.5, false), FALLBACK, -3),
        Some(PlayerCommand::SetVolume(0.5 - 3.0 * step))
    );
}

#[test]
fn volume_is_clamped_and_boundary_presses_send_nothing() {
    assert_eq!(
        volume_command_from_steps(&snapshot(0.98, false), FALLBACK, 5),
        Some(PlayerCommand::SetVolume(1.0))
    );
    assert_eq!(
        volume_command_from_steps(&snapshot(1.0, false), FALLBACK, 1),
        None,
        "At the maximum, the command does not leave the queue of the worker."
    );
    assert_eq!(
        volume_command_from_steps(&snapshot(0.03, false), FALLBACK, -1),
        Some(PlayerCommand::SetVolume(0.0)),
        "Descent to zero = mute, like a slider"
    );
    assert_eq!(
        volume_command_from_steps(&snapshot(0.5, false), FALLBACK, 0),
        None
    );
}

#[test]
fn muted_player_unmutes_on_step_up_and_ignores_step_down() {
    assert_eq!(
        volume_command_from_steps(&snapshot(0.0, true), FALLBACK, 1),
        Some(PlayerCommand::ToggleMute {
            fallback_volume: FALLBACK
        }),
        "The volume return \"up to mute\" belongs to the player, as the icon"
    );
    assert_eq!(
        volume_command_from_steps(&snapshot(0.0, true), FALLBACK, -1),
        None
    );
}

/// Громкость, выставленная шагом, доходит до сохранения тем же путём, что и со слайдера:
/// player применил `SetVolume`, snapshot наблюдается, слышимая громкость пользователя меняется.
#[test]
fn stepped_volume_becomes_the_users_persisted_level_like_the_slider() {
    let before = UserAudioLevel::new(VolumePermille::from_ratio(0.5), MuteState::Audible);
    let Some(PlayerCommand::SetVolume(target)) =
        volume_command_from_steps(&snapshot(0.5, false), FALLBACK, 1)
    else {
        panic!("A step up should give SetVolume");
    };

    let after = before.after_player_observation(PlayerAudioObservation::new(target, false));

    assert_eq!(after.audible_volume(), VolumePermille::from_ratio(0.55));
    assert_ne!(after, before, "The change in the level is intended to");
}

#[test]
fn stepping_down_to_zero_is_persisted_as_mute_keeping_previous_audible_volume() {
    let before = UserAudioLevel::new(VolumePermille::from_ratio(0.05), MuteState::Audible);
    let Some(PlayerCommand::SetVolume(target)) =
        volume_command_from_steps(&snapshot(0.05, false), FALLBACK, -1)
    else {
        panic!("A step down should give SetVolume");
    };

    let after = before.after_player_observation(PlayerAudioObservation::new(target, true));

    assert!(after.is_muted());
    assert_eq!(after.audible_volume(), VolumePermille::from_ratio(0.05));
}
