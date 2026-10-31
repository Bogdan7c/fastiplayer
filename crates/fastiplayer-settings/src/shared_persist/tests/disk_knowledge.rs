//! Знание store об общем файле (04c, пакет F): база несохранённых изменений — последний
//! **виденный** документ диска, снятие восстановимого режима «только на сеанс», создание
//! удалённого файла из документа копии и разведение «конфликт значений» / «ошибка
//! сериализации». Всё через настоящий `AppConfigStore` и настоящую файловую систему.

use fastiplayer_config::{
    BrokenConfigProblem, ConfigError, ConfigLoadOrigin, ConfigSavePolicy, ConfigSessionOnlyReason,
    ConfigWriteResumption, LoadedConfig, SharedConfigUpdate,
};

use super::*;

/// Короткое ожидание замка: тест «занято» не должен ждать секунды.
const BUSY_LOCK_WAIT: Duration = Duration::from_millis(30);

const VOLUME_ID: &str = "audio.volume";

/// Store копии, которая при старте прочитала `config` из файла `path`.
fn store_after_reading(path: &Path, config: &AppConfig) -> AppConfigStore {
    let loaded = LoadedConfig {
        config: config.clone(),
        path: path.to_path_buf(),
        origin: ConfigLoadOrigin::LoadedExisting,
        save_policy: ConfigSavePolicy::WriteToFile,
    };
    AppConfigStore::from_loaded_config(&loaded).with_lock_wait(BUSY_LOCK_WAIT)
}

/// Документ с другой громкостью.
fn with_volume(base: &AppConfig, volume: f64) -> AppConfig {
    let mut document = base.clone();
    document.audio.volume = volume;
    document
}

/// Изменение громкости `previous → current` при занятом соседом замке: «не сохранено».
fn persist_volume_while_lock_is_busy(
    store: &mut AppConfigStore,
    config_path: &Path,
    previous: &AppConfig,
    current: &AppConfig,
) {
    let registry = app_config_registry().expect("registry builds");
    let changed_settings = registry.diff(previous, current).expect("diff builds");
    let neighbor_lock = hold_write_lock(config_path);
    let report = store
        .persist(PersistRequest {
            document: current,
            changed_settings: &changed_settings,
        })
        .expect("a busy file is not a transaction error");
    drop(neighbor_lock);
    assert!(
        matches!(
            settings_file_persist_outcome(&report),
            SettingsFilePersistOutcome::NotSavedYet { .. }
        ),
        "{report:?}"
    );
}

fn volume_on_disk(config_path: &Path) -> f64 {
    load_from_path(config_path)
        .expect("file loads")
        .config
        .audio
        .volume
}

/// Сценарий ревью: сосед записал громкость 0.3, эта копия её видела, но не приняла
/// (запоминаемое состояние, её committed 0.8). Пользователь ставит 0.7, файл занят. При
/// дописывании на диске всё ещё 0.3 — сосед писал РАНЬШЕ, и 0.7 должно попасть в файл.
#[test]
fn own_choice_is_written_when_the_neighbor_wrote_before_it() {
    let directory = unique_test_directory("seen-neighbor-earlier");
    let config_path = directory.join("config.toml");
    let start = with_volume(&AppConfig::default(), 0.8);
    save_validated_atomic_at(&config_path, &start).expect("initial file");
    let mut store = store_after_reading(&config_path, &start);
    let neighbor = with_volume(&start, 0.3);
    save_validated_atomic_at(&config_path, &neighbor).expect("neighbor wrote 0.3");
    store
        .note_disk_config_seen(&neighbor)
        .expect("the board read the neighbour's file");
    let memory = with_volume(&start, 0.7);
    persist_volume_while_lock_is_busy(&mut store, &config_path, &start, &memory);

    let outcome = store.save_unsaved_changes(&memory);

    assert!(
        matches!(&outcome, UnsavedChangesSave::Saved { written, .. }
            if written == &vec![SettingId::from(VOLUME_ID)]),
        "{outcome:?}"
    );
    assert!((volume_on_disk(&config_path) - 0.7).abs() < f64::EPSILON);
    assert!(!store.has_unsaved_changes());
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Видели 0.3, наша запись не удалась (0.7), ПОСЛЕ этого сосед записал 0.5 и доска его
/// прочитала: последний — сосед, его 0.5 остаётся, наше несохранённое забыто.
#[test]
fn neighbor_value_written_after_our_failure_is_kept() {
    let directory = unique_test_directory("neighbor-after-failure");
    let config_path = directory.join("config.toml");
    let start = with_volume(&AppConfig::default(), 0.3);
    save_validated_atomic_at(&config_path, &start).expect("initial file");
    let mut store = store_after_reading(&config_path, &start);
    let memory = with_volume(&start, 0.7);
    persist_volume_while_lock_is_busy(&mut store, &config_path, &start, &memory);
    let neighbor = with_volume(&start, 0.5);
    save_validated_atomic_at(&config_path, &neighbor).expect("neighbor wrote 0.5 later");
    store
        .note_disk_config_seen(&neighbor)
        .expect("the board read the neighbour's file");

    let outcome = store.save_unsaved_changes(&memory);

    assert!(
        matches!(&outcome, UnsavedChangesSave::Saved { kept_from_disk, written, .. }
            if written.is_empty() && kept_from_disk == &vec![SettingId::from(VOLUME_ID)]),
        "{outcome:?}"
    );
    assert!((volume_on_disk(&config_path) - 0.5).abs() < f64::EPSILON);
    assert!(!store.has_unsaved_changes());
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Пользователь снова меняет ключ ПОСЛЕ того, как копия увидела запись соседа: его новое
/// значение позже соседского и при дописывании не уступает.
#[test]
fn own_change_made_after_seeing_the_neighbor_wins() {
    let directory = unique_test_directory("own-after-neighbor");
    let config_path = directory.join("config.toml");
    let start = with_volume(&AppConfig::default(), 0.3);
    save_validated_atomic_at(&config_path, &start).expect("initial file");
    let mut store = store_after_reading(&config_path, &start);
    let first_choice = with_volume(&start, 0.7);
    persist_volume_while_lock_is_busy(&mut store, &config_path, &start, &first_choice);
    let neighbor = with_volume(&start, 0.5);
    save_validated_atomic_at(&config_path, &neighbor).expect("neighbor wrote 0.5");
    store
        .note_disk_config_seen(&neighbor)
        .expect("the board read the neighbour's file");
    let second_choice = with_volume(&start, 0.6);
    persist_volume_while_lock_is_busy(&mut store, &config_path, &first_choice, &second_choice);

    let outcome = store.save_unsaved_changes(&second_choice);

    assert!(
        matches!(&outcome, UnsavedChangesSave::Saved { written, .. }
            if written == &vec![SettingId::from(VOLUME_ID)]),
        "{outcome:?}"
    );
    assert!((volume_on_disk(&config_path) - 0.6).abs() < f64::EPSILON);
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Окно браузерного режима открыто над битым файлом: изменения живут только в памяти, но
/// запоминаются; файл починили — запрет снят, изменения дописаны поверх починенного файла.
#[test]
fn recoverable_session_only_changes_are_written_once_the_file_is_usable() {
    let directory = unique_test_directory("session-only-recovers");
    let config_path = directory.join("config.toml");
    fs::write(&config_path, "[[[").expect("broken file written");
    let loaded = LoadedConfig {
        config: AppConfig::default(),
        path: config_path.clone(),
        origin: ConfigLoadOrigin::DefaultsInMemory,
        save_policy: ConfigSavePolicy::SessionOnly(
            ConfigSessionOnlyReason::BrokenConfigLeftToRegularPlayer(
                BrokenConfigProblem::InvalidTomlSyntax,
            ),
        ),
    };
    let mut store = AppConfigStore::from_loaded_config(&loaded);
    let (memory, _diff) = diff_with(&[("ui.sidebar.width_points", SettingValue::Integer(480))]);
    let report = persist_changes(
        &mut store,
        &[("ui.sidebar.width_points", SettingValue::Integer(480))],
    )
    .expect("session-only is not a transaction error");
    assert_eq!(
        settings_file_persist_outcome(&report),
        SettingsFilePersistOutcome::SessionOnly
    );
    assert_eq!(fs::read_to_string(&config_path).expect("file"), "[[[");
    assert!(store.has_unsaved_changes(), "kept to be written later");

    let mut repaired = AppConfig::default();
    repaired.ui.language = "en".to_string();
    save_validated_atomic_at(&config_path, &repaired).expect("regular player repaired it");
    let resumption = store
        .note_disk_config_seen(&repaired)
        .expect("the board read the repaired file");
    let outcome = store.save_unsaved_changes(&memory);

    assert!(
        matches!(resumption, ConfigWriteResumption::Resumed { .. }),
        "{resumption:?}"
    );
    assert_eq!(store.save_policy(), &ConfigSavePolicy::WriteToFile);
    assert!(
        matches!(&outcome, UnsavedChangesSave::Saved { written, .. }
            if written == &vec![SettingId::from("ui.sidebar.width_points")]),
        "{outcome:?}"
    );
    let on_disk = load_from_path(&config_path).expect("file loads").config;
    assert_eq!(on_disk.ui.sidebar.width_points, 480);
    assert_eq!(on_disk.ui.language, "en", "the repaired file's keys stay");
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Файл новой версии: изменения не запоминаются как несохранённые, запрет не снимается,
/// файл байт-в-байт прежний.
#[test]
fn newer_schema_session_only_never_writes() {
    let directory = unique_test_directory("session-only-newer");
    let config_path = directory.join("config.toml");
    let newer_text = "schema_version = 999\nfuture_key = 1\n";
    fs::write(&config_path, newer_text).expect("newer file written");
    let loaded = LoadedConfig {
        config: AppConfig::default(),
        path: config_path.clone(),
        origin: ConfigLoadOrigin::DefaultsInMemory,
        save_policy: ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::NewerSchemaVersion {
            found: 999,
        }),
    };
    let mut store = AppConfigStore::from_loaded_config(&loaded);
    let (memory, _diff) = diff_with(&[("ui.sidebar.width_points", SettingValue::Integer(480))]);
    persist_changes(
        &mut store,
        &[("ui.sidebar.width_points", SettingValue::Integer(480))],
    )
    .expect("session-only is not a transaction error");

    let resumption = store
        .note_disk_config_seen(&AppConfig::default())
        .expect("sighting is recorded");

    assert!(!store.has_unsaved_changes());
    assert!(
        matches!(resumption, ConfigWriteResumption::StillSessionOnly(_)),
        "{resumption:?}"
    );
    assert_eq!(
        store.save_unsaved_changes(&memory),
        UnsavedChangesSave::NothingToSave
    );
    assert_eq!(fs::read_to_string(&config_path).expect("file"), newer_text);
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Файл удалили во время работы: запись громкости создаёт его из документа копии — в нём
/// и громкость, и прежние настройки копии, а не defaults.
#[test]
fn deleted_file_is_recreated_from_the_copy_document() {
    let directory = unique_test_directory("deleted-file");
    let config_path = directory.join("config.toml");
    let mut start = AppConfig::default();
    start.ui.language = "en".to_string();
    start.ui.sidebar.width_points = 500;
    save_validated_atomic_at(&config_path, &start).expect("initial file");
    let mut store = store_after_reading(&config_path, &start);
    fs::remove_file(&config_path).expect("someone deleted the file");
    let memory = with_volume(&start, 0.4);
    let registry = app_config_registry().expect("registry builds");
    let changed_settings = registry.diff(&start, &memory).expect("diff builds");

    let report = store
        .persist(PersistRequest {
            document: &memory,
            changed_settings: &changed_settings,
        })
        .expect("persist succeeds");

    assert_eq!(report.outcome, PersistOutcome::Persisted);
    let on_disk = load_from_path(&config_path).expect("file recreated").config;
    assert!((on_disk.audio.volume - 0.4).abs() < f64::EPSILON);
    assert_eq!(on_disk.ui.language, "en");
    assert_eq!(on_disk.ui.sidebar.width_points, 500);
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Наше значение несовместимо с тем, что сосед уже записал (`behind_s` больше его
/// `live_retention_s`): это конфликт содержимого — «не сохранено» с честным текстом, без
/// отката транзакции, файл прежний.
#[test]
fn value_conflict_with_the_neighbor_defers_with_a_conflict_message() {
    let directory = unique_test_directory("value-conflict");
    let config_path = directory.join("config.toml");
    let mut neighbor = AppConfig::default();
    neighbor.subtitles.stream.live_retention_s = 60;
    save_validated_atomic_at(&config_path, &neighbor).expect("neighbor wrote");
    let original_bytes = fs::read(&config_path).expect("original bytes");
    let mut store = AppConfigStore::new(&config_path);

    let notice = persist_expecting_not_saved_yet(
        &mut store,
        &[("subtitles.stream.behind_s", SettingValue::Integer(300))],
    );

    assert!(
        notice.contains("conflict with settings saved by another Fastiplayer window"),
        "{notice}"
    );
    assert_eq!(fs::read(&config_path).expect("bytes"), original_bytes);
    assert!(store.has_unsaved_changes());
    fs::remove_dir_all(&directory).expect("test directory removed");
}

/// Сбой сериализации слитого документа — ошибка программы (откат транзакции), а
/// отказ проверки значений — отложенная запись с текстом про соседа.
#[test]
fn serialization_failure_is_a_transaction_error_not_a_neighbor_conflict() {
    let registry = app_config_registry().expect("registry builds");
    let serialization_failure = SharedConfigUpdate::MergedConfigNotSerializable(
        ConfigError::SerializedConfigRoundtripMismatch {
            path: PathBuf::from("config.toml"),
        },
    );
    let validation_failure =
        SharedConfigUpdate::MergedConfigInvalid(ConfigError::ValidateConfigFile {
            path: PathBuf::from("config.toml"),
            source: Box::new(ConfigError::InvalidValue {
                field: "subtitles.stream.behind_s",
                message: "must not exceed live retention".to_string(),
            }),
        });

    let serialization_attempt = classify_update(&registry, serialization_failure);
    let validation_attempt = classify_update(&registry, validation_failure);

    assert!(matches!(serialization_attempt, SaveAttempt::Rejected(_)));
    assert!(matches!(
        validation_attempt,
        SaveAttempt::Deferred(SettingsFileSaveDeferral::ConflictsWithSavedSettings { .. })
    ));
}
