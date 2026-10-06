//! Громкость и mute между запусками (UX-11).
//!
//! «Перезапуск» здесь — настоящий: config пишется на диск через production store,
//! затем читается заново и из него строится новый `SettingsRuntime`, как при запуске.

use std::time::SystemTime;

use fastiplayer_config::{load_from_path, load_or_recover_at};
use player_core::PlayerCommand;

use super::*;
use crate::user_audio_level::{MuteState, PlayerAudioObservation, UserAudioLevel, VolumePermille};

/// Пауза перед записью громкости в production (держим тест в синхроне с модулем).
const PERSIST_DEBOUNCE: Duration = Duration::from_secs(3);

fn runtime_with_config_at(config: AppConfig, path: &std::path::Path) -> SettingsRuntime {
    SettingsRuntime::from_loaded_config(loaded_config_for_test_at(config, path.to_path_buf()))
        .expect("settings runtime should build")
}

/// Запуск: читает config с диска и выдаёт команды, которые получит новый плеер.
fn restart_and_restore(
    path: &std::path::Path,
    now: Instant,
) -> (SettingsRuntime, Vec<PlayerCommand>) {
    let loaded = load_from_path(path).expect("persisted config should reload");
    let mut runtime =
        SettingsRuntime::from_loaded_config(loaded).expect("restarted runtime should build");
    let commands = runtime
        .begin_player_audio_level_restore(now)
        .player_restore_commands();
    (runtime, commands)
}

fn observe(runtime: &mut SettingsRuntime, volume: f32, muted: bool, at: Instant) -> bool {
    runtime.record_player_audio_observation(PlayerAudioObservation::new(volume, muted), at)
}

/// Плеер подтвердил восстановленный уровень — дальше snapshot-ы считаются действиями.
fn start_following_player(runtime: &mut SettingsRuntime, now: Instant) {
    let restored = runtime.begin_player_audio_level_restore(now);
    let observation = if restored.is_muted() {
        PlayerAudioObservation::new(0.0, true)
    } else {
        PlayerAudioObservation::new(restored.audible_volume().as_player_ratio(), false)
    };
    let _confirmation_changed_level = runtime.record_player_audio_observation(observation, now);
}

fn level(permille: u16, mute: MuteState) -> UserAudioLevel {
    UserAudioLevel::new(
        VolumePermille::from_ratio(f64::from(permille) / 1000.0),
        mute,
    )
}

/// Поставили 30 % → пауза → запись → перезапуск: плеер получает 30 %.
/// Автосохранение не трогает текущее воспроизведение.
#[test]
fn volume_change_is_written_after_quiet_period_and_restored_after_restart() {
    let path = temp_config_path("user-audio-volume-restart");
    remove_file_if_exists(&path);
    let mut runtime = runtime_with_config_at(AppConfig::default(), &path);
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&AppConfig::default()).expect("adapter builds");
    let started_at = Instant::now();
    start_following_player(&mut runtime, started_at);

    assert!(observe(&mut runtime, 0.3, false, started_at));
    let deadline = started_at + PERSIST_DEBOUNCE;
    assert_eq!(
        runtime.next_user_audio_level_persist_deadline(),
        Some(deadline)
    );
    assert_eq!(
        runtime
            .flush_due_user_audio_level(deadline - Duration::from_millis(1), &mut adapter)
            .expect("early flush stays pending"),
        super::super::UserAudioLevelFlushOutcome::NoPending
    );
    assert!(!path.exists(), "до конца паузы config не пишется");

    assert_eq!(
        runtime
            .flush_due_user_audio_level(deadline, &mut adapter)
            .expect("due flush commits"),
        super::super::UserAudioLevelFlushOutcome::Succeeded
    );
    assert_eq!(runtime.next_user_audio_level_persist_deadline(), None);
    assert!(adapter.delivered_audio_levels.is_empty());

    let (_restarted, commands) = restart_and_restore(&path, Instant::now());
    assert_eq!(commands, vec![PlayerCommand::SetVolume(0.3)]);
    remove_file_if_exists(&path);
}

/// Mute → выход → запуск: снова mute, а включение звука вернёт прежние 30 %.
#[test]
fn mute_is_written_on_exit_and_restored_with_previous_volume() {
    let path = temp_config_path("user-audio-mute-restart");
    remove_file_if_exists(&path);
    let mut runtime = runtime_with_config_at(AppConfig::default(), &path);
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&AppConfig::default()).expect("adapter builds");
    let now = Instant::now();
    start_following_player(&mut runtime, now);

    observe(&mut runtime, 0.3, false, now);
    observe(&mut runtime, 0.0, true, now + Duration::from_millis(100));
    // Выход раньше конца паузы: lifecycle flush пишет сразу.
    assert_eq!(
        runtime
            .flush_pending_user_audio_level(&mut adapter)
            .expect("lifecycle flush commits"),
        super::super::UserAudioLevelFlushOutcome::Succeeded
    );

    let persisted = load_from_path(&path).expect("config reloads");
    assert_eq!(persisted.config.audio.volume, 0.3);
    assert!(persisted.config.audio.muted);
    let (_restarted, commands) = restart_and_restore(&path, Instant::now());
    assert_eq!(
        commands,
        vec![PlayerCommand::SetVolume(0.3), PlayerCommand::SetVolume(0.0)]
    );
    remove_file_if_exists(&path);
}

/// 100 движений слайдера подряд дают ровно одну запись config-а с последним значением.
#[test]
fn hundred_slider_moves_produce_single_config_write() {
    let path = temp_config_path("user-audio-debounce");
    remove_file_if_exists(&path);
    let mut runtime = runtime_with_config_at(AppConfig::default(), &path);
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&AppConfig::default()).expect("adapter builds");
    let started_at = Instant::now();
    start_following_player(&mut runtime, started_at);

    let mut frame_at = started_at;
    for step in 1..=100_u16 {
        frame_at = started_at + Duration::from_millis(u64::from(step) * 16);
        observe(&mut runtime, f32::from(step) / 200.0, false, frame_at);
        let _outcome = runtime
            .flush_due_user_audio_level(frame_at, &mut adapter)
            .expect("frame flush works");
    }
    assert!(
        !path.exists(),
        "пока слайдер двигается, запись не происходит"
    );

    let quiet_until = frame_at + PERSIST_DEBOUNCE;
    runtime
        .flush_due_user_audio_level(quiet_until, &mut adapter)
        .expect("quiet flush commits");
    runtime
        .flush_due_user_audio_level(quiet_until + PERSIST_DEBOUNCE, &mut adapter)
        .expect("second quiet flush is no-op");

    assert_eq!(adapter.committed_snapshots.len(), 1, "ровно одна запись");
    let persisted = load_from_path(&path).expect("config reloads");
    assert_eq!(persisted.config.audio.volume, 0.5);
    remove_file_if_exists(&path);
}

/// Пустой snapshot нового worker-а (100 %, звук включён) до подтверждения
/// восстановленного уровня не должен попасть в config ни через паузу, ни при выходе.
#[test]
fn placeholder_snapshot_before_restore_confirmation_is_never_persisted() {
    let path = temp_config_path("user-audio-placeholder");
    remove_file_if_exists(&path);
    let mut config = AppConfig::default();
    config.audio.volume = 0.3;
    config.audio.muted = true;
    let mut runtime = runtime_with_config_at(config.clone(), &path);
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter builds");
    let now = Instant::now();

    // Плеера ещё нет: snapshot — заглушка.
    assert!(!observe(&mut runtime, 1.0, false, now));
    let restored = runtime.begin_player_audio_level_restore(now);
    assert_eq!(restored, level(300, MuteState::Muted));
    // Worker ещё не применил команды.
    assert!(!observe(
        &mut runtime,
        1.0,
        false,
        now + Duration::from_millis(5)
    ));
    assert_eq!(runtime.next_user_audio_level_persist_deadline(), None);
    assert_eq!(
        runtime
            .flush_pending_user_audio_level(&mut adapter)
            .expect("exit flush works"),
        super::super::UserAudioLevelFlushOutcome::NoPending
    );
    assert!(!path.exists());

    // Подтверждение: дальше плеер снова источник правды.
    assert!(!observe(
        &mut runtime,
        0.0,
        true,
        now + Duration::from_millis(10)
    ));
    assert!(observe(
        &mut runtime,
        0.6,
        false,
        now + Duration::from_millis(20)
    ));
    assert!(runtime.next_user_audio_level_persist_deadline().is_some());
    remove_file_if_exists(&path);
}

/// Если подтверждающий snapshot пропущен, сохранение не выключается навсегда.
#[test]
fn missed_restore_confirmation_times_out_and_follows_player_again() {
    let path = temp_config_path("user-audio-sync-timeout");
    remove_file_if_exists(&path);
    let mut runtime = runtime_with_config_at(AppConfig::default(), &path);
    let now = Instant::now();
    let _restored = runtime.begin_player_audio_level_restore(now);

    assert!(!observe(
        &mut runtime,
        0.5,
        false,
        now + Duration::from_millis(1999)
    ));
    assert!(observe(
        &mut runtime,
        0.5,
        false,
        now + Duration::from_secs(2)
    ));
    assert!(runtime.next_user_audio_level_persist_deadline().is_some());
    remove_file_if_exists(&path);
}

/// Вернулись к сохранённой громкости до конца паузы — писать нечего.
#[test]
fn returning_to_saved_level_cancels_pending_write() {
    let path = temp_config_path("user-audio-return");
    remove_file_if_exists(&path);
    let mut runtime = runtime_with_config_at(AppConfig::default(), &path);
    let now = Instant::now();
    start_following_player(&mut runtime, now);

    assert!(observe(&mut runtime, 0.3, false, now));
    assert!(observe(
        &mut runtime,
        0.8,
        false,
        now + Duration::from_millis(500)
    ));
    assert_eq!(runtime.next_user_audio_level_persist_deadline(), None);
    remove_file_if_exists(&path);
}

/// Битое значение громкости в config: config уходит в `.bak`, плеер получает
/// громкость по умолчанию, ничего не паникует.
#[test]
fn broken_config_volume_falls_back_to_default_level() {
    let directory = std::env::temp_dir().join(format!(
        "fastiplayer-user-audio-broken-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("test directory created");
    let config_path = directory.join("config.toml");
    fs::write(
        &config_path,
        "schema_version = 11\n\n[audio]\nvolume = 5.0\nmuted = true\n",
    )
    .expect("broken config written");

    let loaded = load_or_recover_at(&config_path, SystemTime::now()).expect("startup recovers");
    let mut runtime = SettingsRuntime::from_loaded_config(loaded).expect("runtime builds");
    let commands = runtime
        .begin_player_audio_level_restore(Instant::now())
        .player_restore_commands();

    assert_eq!(commands, vec![PlayerCommand::SetVolume(0.8)]);
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Apply из окна настроек сразу меняет текущее воспроизведение и сбрасывает
/// отложенную запись слайдера (действие в настройках новее).
#[test]
fn settings_apply_of_audio_level_is_delivered_to_playback() {
    let path = temp_config_path("user-audio-settings-apply");
    remove_file_if_exists(&path);
    let mut runtime = runtime_with_config_at(AppConfig::default(), &path);
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&AppConfig::default()).expect("adapter builds");
    let now = Instant::now();
    start_following_player(&mut runtime, now);
    observe(&mut runtime, 0.6, false, now);
    assert!(runtime.next_user_audio_level_persist_deadline().is_some());

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("audio.volume"),
                value: SettingValue::Float(0.25),
            },
            SettingsUiAction::SetValue {
                setting_id: SettingId::from("audio.muted"),
                value: SettingValue::Bool(true),
            },
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );

    assert_eq!(
        adapter.delivered_audio_levels,
        vec![level(250, MuteState::Muted)]
    );
    assert_eq!(runtime.next_user_audio_level_persist_deadline(), None);
    let persisted = load_from_path(&path).expect("config reloads");
    assert_eq!(persisted.config.audio.volume, 0.25);
    assert!(persisted.config.audio.muted);
    remove_file_if_exists(&path);
}

/// Apply других настроек не трогает громкость плеера.
#[test]
fn settings_apply_without_audio_change_does_not_touch_playback() {
    let path = temp_config_path("user-audio-unrelated-apply");
    remove_file_if_exists(&path);
    let mut runtime = runtime_with_config_at(AppConfig::default(), &path);
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&AppConfig::default()).expect("adapter builds");

    run_runtime_actions(
        &mut runtime,
        vec![
            SettingsUiAction::Open,
            brightness_action(0.2),
            SettingsUiAction::Apply,
        ],
        &mut adapter,
    );

    assert!(adapter.delivered_audio_levels.is_empty());
    remove_file_if_exists(&path);
}

/// Ошибка записи: config прежний, пользователь видит причину, плеер не тронут.
#[test]
fn failed_audio_level_commit_reports_failure_and_keeps_config() {
    let path = temp_config_path("user-audio-commit-failure");
    remove_file_if_exists(&path);
    let mut runtime = runtime_with_config_at(AppConfig::default(), &path);
    let mut adapter =
        RecordingRuntimeAdapter::from_config(&AppConfig::default()).expect("adapter builds");
    adapter.fail_player = true;
    let now = Instant::now();
    start_following_player(&mut runtime, now);
    observe(&mut runtime, 0.3, false, now);

    assert_eq!(
        runtime
            .flush_pending_user_audio_level(&mut adapter)
            .expect("failure is typed, not an error"),
        super::super::UserAudioLevelFlushOutcome::Failed
    );
    assert_eq!(runtime.committed_config().audio.volume, 0.8);
    assert!(!path.exists());
    assert!(adapter.delivered_audio_levels.is_empty());
    assert!(runtime.ui_model().status.summary.is_some());
    remove_file_if_exists(&path);
}
