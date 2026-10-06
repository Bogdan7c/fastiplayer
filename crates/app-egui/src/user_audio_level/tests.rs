//! Тесты value type громкости пользователя (UX-11).

use fastiplayer_config::AudioConfig;
use player_core::PlayerCommand;

use super::*;

fn audio_config(volume: f64, muted: bool) -> AudioConfig {
    AudioConfig {
        volume,
        muted,
        ..AudioConfig::default()
    }
}

fn volume(permille: u16) -> VolumePermille {
    VolumePermille::from_ratio(f64::from(permille) / 1000.0)
}

/// f32 из плеера и f64 из config-а с «хвостами» дают одну и ту же громкость,
/// а в config пишется ровное число.
#[test]
fn volume_permille_rounds_float_noise_and_clamps_out_of_range() {
    assert_eq!(VolumePermille::from_ratio(f64::from(0.3_f32)), volume(300));
    assert_eq!(VolumePermille::from_ratio(0.300_000_01), volume(300));
    assert_eq!(volume(300).as_config_ratio(), 0.3);
    assert_eq!(volume(300).as_player_ratio(), 0.3_f32);
    assert_eq!(VolumePermille::from_ratio(1.7), volume(1000));
    assert_eq!(VolumePermille::from_ratio(-0.2), VolumePermille::SILENT);
    assert_eq!(VolumePermille::from_ratio(f64::NAN), VolumePermille::SILENT);
}

/// Mute восстанавливается двумя командами в правильном порядке: плеер сначала
/// запоминает громкость «до mute», потом выключает звук.
#[test]
fn muted_level_restores_remembered_volume_before_muting() {
    let level = UserAudioLevel::from_audio_config(&audio_config(0.3, true));

    assert_eq!(
        level.player_restore_commands(),
        vec![PlayerCommand::SetVolume(0.3), PlayerCommand::SetVolume(0.0)]
    );
    assert_eq!(level.effective_player_volume(), 0.0);
}

#[test]
fn audible_level_restores_single_volume_command() {
    let level = UserAudioLevel::from_audio_config(&audio_config(0.45, false));

    assert_eq!(
        level.player_restore_commands(),
        vec![PlayerCommand::SetVolume(0.45)]
    );
    assert_eq!(level.effective_player_volume(), 0.45);
}

/// `volume = 0` в config — это тишина: для плеера это mute, лишней записи
/// `muted = true` после запуска быть не должно.
#[test]
fn zero_config_volume_is_normalized_to_muted_silence() {
    let level = UserAudioLevel::from_audio_config(&audio_config(0.0, false));

    assert!(level.is_muted());
    assert_eq!(
        level.player_restore_commands(),
        vec![PlayerCommand::SetVolume(0.0)]
    );
    assert!(level.is_confirmed_by(PlayerAudioObservation::new(0.0, true)));
}

/// При mute плеер показывает 0.0, но громкость «до mute» не теряется.
#[test]
fn player_mute_keeps_previous_audible_volume() {
    let audible = UserAudioLevel::from_audio_config(&audio_config(0.6, false));

    let muted = audible.after_player_observation(PlayerAudioObservation::new(0.0, true));
    assert!(muted.is_muted());
    assert_eq!(muted.audible_volume(), volume(600));

    let louder = muted.after_player_observation(PlayerAudioObservation::new(0.25, false));
    assert_eq!(louder, UserAudioLevel::new(volume(250), MuteState::Audible));
}

#[test]
fn confirmation_requires_matching_mute_and_audible_volume() {
    let muted = UserAudioLevel::new(volume(300), MuteState::Muted);
    assert!(muted.is_confirmed_by(PlayerAudioObservation::new(0.0, true)));
    // Пустой snapshot нового worker-а (1.0, звук включён) — не подтверждение.
    assert!(!muted.is_confirmed_by(PlayerAudioObservation::new(1.0, false)));

    let audible = UserAudioLevel::new(volume(300), MuteState::Audible);
    assert!(audible.is_confirmed_by(PlayerAudioObservation::new(0.3, false)));
    assert!(!audible.is_confirmed_by(PlayerAudioObservation::new(1.0, false)));
    assert!(!audible.is_confirmed_by(PlayerAudioObservation::new(0.0, true)));
}
