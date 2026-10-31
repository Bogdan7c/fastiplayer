//! «Общая доска» (04c, пакет F): дефекты, найденные ревью, через настоящий `SettingsRuntime`
//! и настоящий config-файл.
//!
//! - база несохранённого — последний виденный документ диска, а не committed копии;
//! - восстановимый режим «только на сеанс» снимается, когда доска читает нормальный файл;
//! - удалённый во время работы файл создаётся из документа копии;
//! - настройка «после перезапуска» от соседа принимается как своё Apply;
//! - отказ принятия одного значения не повторяет тост на несвязанные записи соседа.

use std::path::Path;

use fastiplayer_config::{
    BrokenConfigProblem, ConfigLockFailure, ConfigSessionOnlyReason, RenderProfile,
    SharedConfigRead, read_shared_config_at, save_validated_atomic_at,
};

use super::shared_board::{
    apply_in_settings_window, copy_on, deliver_file_signal, shared_config_file, text_size,
};
use super::*;
use crate::settings_runtime::render_profile_restart::APPLIES_AFTER_RESTART_STATUS;
use crate::settings_runtime::shared_board::SettingsFileNotice;
use crate::settings_ui::RestartOffer;

/// Документ на диске (файл обязан читаться).
fn on_disk(path: &Path) -> AppConfig {
    match read_shared_config_at(path) {
        SharedConfigRead::Parsed(config) => *config,
        other => panic!("config file must be readable: {other:?}"),
    }
}

/// Копия, стартовавшая в режиме «только на сеанс» с данной причиной (defaults в памяти).
fn session_only_copy_on(path: &Path, reason: ConfigSessionOnlyReason) -> SettingsRuntime {
    SettingsRuntime::from_loaded_config(LoadedConfig {
        config: AppConfig::default(),
        path: path.to_path_buf(),
        origin: ConfigLoadOrigin::DefaultsInMemory,
        save_policy: ConfigSavePolicy::SessionOnly(reason),
    })
    .expect("settings runtime should be built")
}

/// Apply в окне настроек без требования «полностью применено» (запись может быть
/// отложена или запрещена).
fn apply_values(
    runtime: &mut SettingsRuntime,
    adapter: &mut RecordingRuntimeAdapter,
    values: Vec<(&str, SettingValue)>,
) {
    let mut actions = vec![SettingsUiAction::Open];
    actions.extend(
        values
            .into_iter()
            .map(|(setting_id, value)| SettingsUiAction::SetValue {
                setting_id: SettingId::from(setting_id),
                value,
            }),
    );
    actions.push(SettingsUiAction::Apply);
    run_runtime_actions(runtime, actions, adapter);
}

/// Сосед записал громкость 0.15, эта копия видела файл, но громкость не приняла (решение
/// 137). Пользователь ставит 0.6, а файл в этот момент битый. Файл возвращается с тем же
/// содержимым, что копия видела, — сосед писал РАНЬШЕ, и 0.6 должно попасть в файл.
#[test]
fn own_volume_choice_is_not_swallowed_by_an_earlier_neighbour_volume() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut first = copy_on(&path, &config);
    let mut first_adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    let mut second = copy_on(&path, &config);
    let mut second_adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    apply_in_settings_window(
        &mut first,
        &mut first_adapter,
        vec![("audio.volume", SettingValue::Float(0.15))],
    );
    let neighbour_file = on_disk(&path);
    let _seen = deliver_file_signal(&mut second, &mut second_adapter);
    assert_eq!(second.committed_config().audio.volume, config.audio.volume);
    fs::write(&path, b"schema_version = [broken").expect("the file breaks for a moment");

    apply_values(
        &mut second,
        &mut second_adapter,
        vec![("audio.volume", SettingValue::Float(0.6))],
    );
    save_validated_atomic_at(&path, &neighbour_file).expect("the same content comes back");
    let _repaired = deliver_file_signal(&mut second, &mut second_adapter);

    assert!(
        (on_disk(&path).audio.volume - 0.6).abs() < f64::EPSILON,
        "the user's later choice is written"
    );
}

/// Окно браузерного режима открыто над битым файлом: только на сеанс. Файл починили —
/// запрет снят (кадр просит снять плашки), изменение этого сеанса дописано, значения
/// починенного файла приняты, а следующая запись (громкость) идёт в файл без плашки.
#[test]
fn browser_window_over_broken_file_writes_again_after_repair() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    fs::write(&path, b"[audio\n").expect("broken file");
    let mut runtime = session_only_copy_on(
        &path,
        ConfigSessionOnlyReason::BrokenConfigLeftToRegularPlayer(
            BrokenConfigProblem::InvalidTomlSyntax,
        ),
    );
    let mut adapter = RecordingRuntimeAdapter::from_config(&AppConfig::default()).expect("adapter");
    let own_size = AppConfig::default().subtitles.text_size_percent + 25;
    apply_values(
        &mut runtime,
        &mut adapter,
        vec![("subtitles.text_size_percent", text_size(own_size))],
    );
    assert_eq!(adapter.session_only_reports, 1);
    assert_eq!(fs::read(&path).expect("bytes"), b"[audio\n");

    let mut repaired = AppConfig::default();
    repaired.ui.language = "en".to_string();
    save_validated_atomic_at(&path, &repaired).expect("regular player repaired the file");
    let board_frame = deliver_file_signal(&mut runtime, &mut adapter);

    assert!(
        board_frame.session_only_lifted,
        "the session-only banner goes away"
    );
    assert_eq!(board_frame.notice, None);
    let after_repair = on_disk(&path);
    assert_eq!(after_repair.subtitles.text_size_percent, own_size);
    assert_eq!(after_repair.ui.language, "en");
    assert_eq!(
        runtime.committed_config().ui.language,
        "en",
        "repaired values adopted"
    );

    apply_values(
        &mut runtime,
        &mut adapter,
        vec![("audio.volume", SettingValue::Float(0.35))],
    );
    assert!((on_disk(&path).audio.volume - 0.35).abs() < f64::EPSILON);
    assert_eq!(
        adapter.session_only_reports, 1,
        "no new session-only banner"
    );
}

/// Обычный плеер стартовал при занятом замке над отсутствующим файлом: только на сеанс.
/// Сосед создал нормальный файл — запись пошла, изменение сеанса в файле.
#[test]
fn regular_player_with_lock_unavailable_writes_once_a_usable_file_appears() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    let mut runtime = session_only_copy_on(
        &path,
        ConfigSessionOnlyReason::ConfigLockUnavailable(ConfigLockFailure::Busy {
            waited: std::time::Duration::from_secs(2),
        }),
    );
    let mut adapter = RecordingRuntimeAdapter::from_config(&AppConfig::default()).expect("adapter");
    let own_size = AppConfig::default().subtitles.text_size_percent + 15;
    apply_values(
        &mut runtime,
        &mut adapter,
        vec![("subtitles.text_size_percent", text_size(own_size))],
    );
    assert!(!path.exists(), "session-only does not create the file");

    save_validated_atomic_at(&path, &AppConfig::default()).expect("neighbour created the file");
    let board_frame = deliver_file_signal(&mut runtime, &mut adapter);

    assert!(board_frame.session_only_lifted);
    assert_eq!(on_disk(&path).subtitles.text_size_percent, own_size);
}

/// Файл новой версии: даже если потом на его месте нормальный файл, эта копия в этот
/// запуск не пишет (файл принадлежит более новой версии плеера).
#[test]
fn newer_version_session_only_is_never_lifted() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    let mut runtime = session_only_copy_on(
        &path,
        ConfigSessionOnlyReason::NewerSchemaVersion { found: 999 },
    );
    let mut adapter = RecordingRuntimeAdapter::from_config(&AppConfig::default()).expect("adapter");
    save_validated_atomic_at(&path, &AppConfig::default()).expect("a normal file appears");
    let original_bytes = fs::read(&path).expect("bytes");

    let board_frame = deliver_file_signal(&mut runtime, &mut adapter);
    apply_values(
        &mut runtime,
        &mut adapter,
        vec![(
            "subtitles.text_size_percent",
            text_size(AppConfig::default().subtitles.text_size_percent + 5),
        )],
    );

    assert!(!board_frame.session_only_lifted);
    assert_eq!(fs::read(&path).expect("bytes"), original_bytes);
    assert_eq!(adapter.session_only_reports, 1, "still session-only");
}

/// Файл удалили во время работы: запись громкости создаёт его из документа копии —
/// прежние пользовательские настройки не пропадают после перезапуска.
#[test]
fn deleted_file_is_recreated_with_the_copy_settings() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut runtime = copy_on(&path, &config);
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    fs::remove_file(&path).expect("someone deleted the file");

    apply_in_settings_window(
        &mut runtime,
        &mut adapter,
        vec![("audio.volume", SettingValue::Float(0.45))],
    );

    let recreated = on_disk(&path);
    assert!((recreated.audio.volume - 0.45).abs() < f64::EPSILON);
    assert_eq!(recreated.render.profile, config.render.profile);
    assert_eq!(
        recreated.subtitles.text_size_percent,
        config.subtitles.text_size_percent
    );
    assert_eq!(recreated.player.start_paused, config.player.start_paused);
    assert_ne!(
        recreated.player.start_paused,
        AppConfig::default().player.start_paused,
        "the custom value really differs from defaults"
    );
}

/// Сосед сменил профиль рендера («после перезапуска»): копия принимает его как своё
/// Apply — committed новый, тоста нет, у поля «Applies after restart» и «Restart now».
#[test]
fn neighbour_render_profile_is_adopted_with_the_restart_offer() {
    let config = AppConfig::default();
    let (_directory, path) = shared_config_file(&config);
    let mut first = copy_on(&path, &config);
    let mut first_adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    let mut second = copy_on(&path, &config);
    let mut second_adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    apply_in_settings_window(
        &mut first,
        &mut first_adapter,
        vec![(
            "render.profile",
            SettingValue::Select(SettingOptionId::from("vulkan")),
        )],
    );

    let board_frame = deliver_file_signal(&mut second, &mut second_adapter);

    assert_eq!(board_frame.notice, None);
    assert_eq!(
        second.committed_config().render.profile,
        RenderProfile::Vulkan
    );
    assert!(second_adapter.renderer_recreation_updates.is_empty());
    run_runtime_actions(
        &mut second,
        vec![SettingsUiAction::Open],
        &mut second_adapter,
    );
    let profile_field = second
        .ui_model()
        .fields
        .iter()
        .find(|field| field.descriptor.id == SettingId::from("render.profile"))
        .cloned()
        .expect("render.profile is in the visual model");
    assert_eq!(
        profile_field.status.as_deref(),
        Some(APPLIES_AFTER_RESTART_STATUS)
    );
    assert_eq!(profile_field.restart_offer, RestartOffer::Available);
}

/// Маршрут отказал на значении соседа: один тост. Следующая запись соседа по другому
/// ключу (язык) принимается сама по себе, без нового тоста и без повторной пересборки.
#[test]
fn unrelated_neighbour_write_does_not_repeat_the_refusal_toast() {
    let config = custom_config_for_test();
    let (_directory, path) = shared_config_file(&config);
    let mut runtime = copy_on(&path, &config);
    let mut adapter = RecordingRuntimeAdapter::from_config(&config).expect("adapter");
    adapter.fail_media = true;
    let mut neighbour = config.clone();
    neighbour.network.read_ahead_mb = config.network.read_ahead_mb + 3;
    save_validated_atomic_at(&path, &neighbour).expect("neighbour write");
    let failed = deliver_file_signal(&mut runtime, &mut adapter);
    assert_eq!(failed.notice, Some(SettingsFileNotice::AdoptionFailed));
    assert_eq!(adapter.media_updates, 1);

    neighbour.ui.language = "en".to_string();
    save_validated_atomic_at(&path, &neighbour).expect("unrelated neighbour write");
    let unrelated = deliver_file_signal(&mut runtime, &mut adapter);

    assert_eq!(unrelated.notice, None, "no repeated toast");
    assert_eq!(adapter.media_updates, 1, "the refused value is not retried");
    assert_eq!(runtime.committed_config().ui.language, "en");
    assert_eq!(
        runtime.committed_config().network.read_ahead_mb,
        config.network.read_ahead_mb
    );
}
