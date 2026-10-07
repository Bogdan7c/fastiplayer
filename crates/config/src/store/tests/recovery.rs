//! Восстановление после плохого config-файла (UX edge cases, сессия 05).
//!
//! Все тесты работают с настоящей файловой системой во временном каталоге и проверяют
//! наблюдаемый результат: что лежит на диске, какие байты в резервной копии, какой
//! итог загрузки и что произойдёт при попытке сохранить настройки.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;
use crate::{
    BrokenConfigProblem, BrokenConfigRecovery, ConfigSaveOutcome, ConfigSavePolicy,
    ConfigSessionOnlyReason, ConfigWriteProblem, load_or_recover_at,
};

/// 2026-10-05 14:30:12 UTC — фиксированный момент обнаружения поломки.
fn recovery_moment() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_791_210_612)
}

/// Имя резервной копии для [`recovery_moment`].
const EXPECTED_BACKUP_NAME: &str = "config.toml.broken-2026-10-05_14-30-12.bak";

/// Текст config-а по умолчанию — то, что должно оказаться в файле после восстановления.
fn default_config_text() -> String {
    AppConfig::default()
        .to_pretty_toml()
        .expect("defaults сериализуются")
}

/// Временный каталог с config-файлом заданного содержимого.
fn config_dir_with(config_bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let temp_dir = tempfile::tempdir().expect("temp dir создан");
    let config_path = temp_dir.path().join("config.toml");
    fs::write(&config_path, config_bytes).expect("config записан");
    (temp_dir, config_path)
}

/// Общая проверка восстановления: копия с исходными байтами, новый файл = defaults.
fn assert_recovered_from_broken(original_bytes: &[u8], expected_problem: BrokenConfigProblem) {
    let (temp_dir, config_path) = config_dir_with(original_bytes);

    let loaded = load_or_recover_at(&config_path, recovery_moment()).expect("запуск не падает");

    assert_eq!(
        loaded.origin,
        ConfigLoadOrigin::RecoveredFromBroken(BrokenConfigRecovery {
            problem: expected_problem,
            backup_file_name: EXPECTED_BACKUP_NAME.to_string(),
        })
    );
    assert_eq!(loaded.save_policy, ConfigSavePolicy::WriteToFile);
    assert_eq!(loaded.config, AppConfig::default());
    // Резервная копия хранит ровно те байты, что были у пользователя.
    let backup_bytes =
        fs::read(temp_dir.path().join(EXPECTED_BACKUP_NAME)).expect("резервная копия есть");
    assert_eq!(backup_bytes, original_bytes);
    // На месте config-а — полноценный файл по умолчанию, который снова загружается.
    assert_eq!(
        fs::read_to_string(&config_path).expect("новый config есть"),
        default_config_text()
    );
    let reloaded = load_from_path(&config_path).expect("новый config корректен");
    assert_eq!(reloaded.config, AppConfig::default());
    assert_no_save_temp_files(temp_dir.path());
}

/// Лишняя скобка (сценарий ручной приёмки) → копия, defaults, итог «восстановлен».
#[test]
fn syntax_garbage_is_backed_up_and_replaced_with_defaults() {
    let mut broken_text = default_config_text();
    broken_text.push_str("\n[[[\n");

    assert_recovered_from_broken(
        broken_text.as_bytes(),
        BrokenConfigProblem::InvalidTomlSyntax,
    );
}

/// Файл не в UTF-8 — тоже повреждён, а не «не читается».
#[test]
fn non_utf8_bytes_are_backed_up_as_invalid_syntax() {
    assert_recovered_from_broken(
        &[0xff, 0xfe, 0x00, 0x41],
        BrokenConfigProblem::InvalidTomlSyntax,
    );
}

/// Неизвестный ключ при текущей версии схемы → копия и defaults.
#[test]
fn unknown_key_is_backed_up_as_schema_mismatch() {
    let mut broken_text = default_config_text();
    broken_text.push_str("\nunexpected_root_key = true\n");

    assert_recovered_from_broken(broken_text.as_bytes(), BrokenConfigProblem::SchemaMismatch);
}

/// Значение вне диапазона → копия, в итоге названо проблемное поле.
#[test]
fn out_of_range_value_is_backed_up_with_field_name() {
    let broken_text = default_config_text().replace("width_points = 420", "width_points = 5");
    assert_ne!(
        broken_text,
        default_config_text(),
        "fixture должна поменять значение"
    );

    assert_recovered_from_broken(
        broken_text.as_bytes(),
        BrokenConfigProblem::InvalidValue {
            field: "ui.sidebar.width_points",
        },
    );
}

/// Слишком старая версия схемы (без миграции) считается повреждённой.
#[test]
fn unsupported_old_schema_version_is_backed_up() {
    let broken_text = default_config_text().replace("schema_version = 12", "schema_version = 1");

    assert_recovered_from_broken(
        broken_text.as_bytes(),
        BrokenConfigProblem::InvalidValue {
            field: "schema_version",
        },
    );
}

/// Конфиг новой версии: файл не тронут, работа без записи, save отказан типизированно.
#[test]
fn newer_schema_version_keeps_file_untouched_and_session_only() {
    // Реалистичный файл новой версии: другая версия и незнакомый нам ключ.
    let newer_text = default_config_text().replace("schema_version = 12", "schema_version = 99")
        + "\nfeature_from_the_future = true\n";
    let (temp_dir, config_path) = config_dir_with(newer_text.as_bytes());

    let loaded = load_or_recover_at(&config_path, recovery_moment()).expect("запуск не падает");

    assert_eq!(loaded.origin, ConfigLoadOrigin::DefaultsInMemory);
    let expected_reason = ConfigSessionOnlyReason::NewerSchemaVersion { found: 99 };
    assert_eq!(
        loaded.save_policy,
        ConfigSavePolicy::SessionOnly(expected_reason.clone())
    );
    assert_eq!(loaded.config, AppConfig::default());

    // Попытка сохранить изменённые настройки не пишет файл и сообщает причину.
    let mut changed_config = loaded.config.clone();
    changed_config.ui.sidebar.width_points = 500;
    let save_outcome = loaded
        .save_target()
        .save(&changed_config)
        .expect("корректный документ принимается");
    assert_eq!(
        save_outcome,
        ConfigSaveOutcome::KeptInMemoryOnly(expected_reason)
    );
    assert_eq!(
        fs::read_to_string(&config_path).expect("файл на месте"),
        newer_text
    );
    // Резервной копии нет: файл исправен, просто не для этой версии.
    assert_eq!(
        fs::read_dir(temp_dir.path())
            .expect("каталог читается")
            .count(),
        1
    );
}

/// В режиме без записи некорректный документ по-прежнему отклоняется проверкой.
#[test]
fn session_only_save_still_rejects_invalid_document() {
    let newer_text = default_config_text().replace("schema_version = 12", "schema_version = 99");
    let (_temp_dir, config_path) = config_dir_with(newer_text.as_bytes());
    let loaded = load_or_recover_at(&config_path, recovery_moment()).expect("запуск не падает");

    let mut invalid_config = loaded.config.clone();
    invalid_config.ui.sidebar.width_points = 5;
    let save_error = loaded
        .save_target()
        .save(&invalid_config)
        .expect_err("значение вне диапазона отклоняется");

    assert!(save_error.to_string().contains("ui.sidebar.width_points"));
}

/// По пути config-а лежит каталог: ничего не трогаем, работа без записи.
#[test]
fn directory_at_config_path_is_left_untouched() {
    let temp_dir = tempfile::tempdir().expect("temp dir создан");
    let config_path = temp_dir.path().join("config.toml");
    fs::create_dir(&config_path).expect("каталог создан");

    let loaded = load_or_recover_at(&config_path, recovery_moment()).expect("запуск не падает");

    assert_eq!(loaded.origin, ConfigLoadOrigin::DefaultsInMemory);
    assert_eq!(
        loaded.save_policy,
        ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::ConfigPathIsNotFile)
    );
    assert!(config_path.is_dir());
    assert_eq!(
        fs::read_dir(temp_dir.path())
            .expect("каталог читается")
            .count(),
        1
    );
}

/// Файл без права чтения: без резервной копии, работа без записи, файл не тронут.
#[cfg(unix)]
#[test]
fn unreadable_file_is_left_untouched_without_backup() {
    use std::os::unix::fs::PermissionsExt;

    let original_text = default_config_text();
    let (temp_dir, config_path) = config_dir_with(original_text.as_bytes());
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o000)).expect("права сняты");
    if fs::read(&config_path).is_ok() {
        // Под root права не мешают чтению — сценарий в этом окружении невоспроизводим.
        eprintln!("пропуск: процесс читает файл без прав (root)");
        return;
    }

    let loaded = load_or_recover_at(&config_path, recovery_moment()).expect("запуск не падает");

    assert_eq!(loaded.origin, ConfigLoadOrigin::DefaultsInMemory);
    assert_eq!(
        loaded.save_policy,
        ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::ConfigFileUnreadable {
            error_kind: io::ErrorKind::PermissionDenied,
        })
    );
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).expect("права вернули");
    assert_eq!(
        fs::read_to_string(&config_path).expect("файл на месте"),
        original_text
    );
    assert_eq!(
        fs::read_dir(temp_dir.path())
            .expect("каталог читается")
            .count(),
        1
    );
}

/// Каталог только для чтения: копию сделать нельзя → файл не тронут, запись запрещена.
#[cfg(unix)]
#[test]
fn backup_failure_keeps_broken_file_and_disables_saving() {
    use std::os::unix::fs::PermissionsExt;

    let (temp_dir, config_path) = config_dir_with(b"[[[");
    fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o500)).expect("права сняты");
    if fs::write(temp_dir.path().join("probe"), b"").is_ok() {
        eprintln!("пропуск: процесс пишет в read-only каталог (root)");
        return;
    }

    let loaded = load_or_recover_at(&config_path, recovery_moment()).expect("запуск не падает");
    fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o700)).expect("права вернули");

    assert_eq!(loaded.origin, ConfigLoadOrigin::DefaultsInMemory);
    assert_eq!(
        loaded.save_policy,
        ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::BackupFailed {
            error_kind: io::ErrorKind::PermissionDenied,
        })
    );
    assert_eq!(fs::read(&config_path).expect("файл на месте"), b"[[[");
    assert_eq!(
        fs::read_dir(temp_dir.path())
            .expect("каталог читается")
            .count(),
        1
    );
}

/// Имя резервной копии уже занято: старая копия не перезаписывается, берётся `-2`.
#[test]
fn existing_backup_is_never_overwritten() {
    let (temp_dir, config_path) = config_dir_with(b"second broken");
    let older_backup = temp_dir.path().join(EXPECTED_BACKUP_NAME);
    fs::write(&older_backup, b"first broken").expect("старая копия создана");

    let loaded = load_or_recover_at(&config_path, recovery_moment()).expect("запуск не падает");

    let expected_second_name = "config.toml.broken-2026-10-05_14-30-12-2.bak";
    assert_eq!(
        loaded.origin,
        ConfigLoadOrigin::RecoveredFromBroken(BrokenConfigRecovery {
            problem: BrokenConfigProblem::InvalidTomlSyntax,
            backup_file_name: expected_second_name.to_string(),
        })
    );
    assert_eq!(
        fs::read(&older_backup).expect("старая копия цела"),
        b"first broken"
    );
    assert_eq!(
        fs::read(temp_dir.path().join(expected_second_name)).expect("новая копия есть"),
        b"second broken"
    );
}

/// Прерванная прошлая запись: остался обрезанный temp, config-а нет.
///
/// Это след сбоя посреди первой записи при новом протоколе: обрезанные байты могут
/// оказаться только во временном файле, а `config.toml` либо отсутствует, либо полон.
/// Следующий запуск создаёт полный config и убирает брошенный temp.
#[test]
fn interrupted_first_write_leaves_no_truncated_config() {
    let temp_dir = tempfile::tempdir().expect("temp dir создан");
    let config_path = temp_dir.path().join("config.toml");
    let truncated_default = &default_config_text()[..40];
    let stale_temp = temp_dir.path().join(".config.toml.save-777-3.tmp");
    fs::write(&stale_temp, truncated_default).expect("брошенный temp создан");
    let legacy_stale_temp = temp_dir.path().join(".config.toml.777.0.tmp");
    fs::write(&legacy_stale_temp, truncated_default).expect("брошенный temp старого формата");

    let loaded = load_or_recover_at(&config_path, recovery_moment()).expect("запуск не падает");

    assert_eq!(loaded.origin, ConfigLoadOrigin::CreatedDefault);
    assert_eq!(
        fs::read_to_string(&config_path).expect("config создан"),
        default_config_text()
    );
    assert!(!stale_temp.exists());
    assert!(!legacy_stale_temp.exists());
}

/// Запись defaults не удалась (каталог config-а создать нельзя): нет ни обрезанного
/// файла, ни отказа запуска — defaults в памяти без записи.
#[cfg(unix)]
#[test]
fn failed_first_write_runs_in_memory_without_partial_file() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::tempdir().expect("temp dir создан");
    // Каталог `fastiplayer` ещё не существует, а создать его в read-only родителе нельзя.
    let config_path = temp_dir.path().join("fastiplayer").join("config.toml");
    fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o500)).expect("права сняты");
    if fs::write(temp_dir.path().join("probe"), b"").is_ok() {
        eprintln!("пропуск: процесс пишет в read-only каталог (root)");
        return;
    }

    let loaded = load_or_recover_at(&config_path, recovery_moment()).expect("запуск не падает");
    fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o700)).expect("права вернули");

    assert_eq!(loaded.origin, ConfigLoadOrigin::DefaultsInMemory);
    assert_eq!(
        loaded.save_policy,
        ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::DefaultConfigNotWritten(
            ConfigWriteProblem::CreateDirectory(io::ErrorKind::PermissionDenied)
        ))
    );
    assert!(!config_path.exists());
}

/// Путь config-а проходит через обычный файл: это «не читается», а не «повреждён».
#[test]
fn path_through_regular_file_is_unreadable_not_broken() {
    let temp_dir = tempfile::tempdir().expect("temp dir создан");
    let blocking_file = temp_dir.path().join("not-a-directory");
    fs::write(&blocking_file, b"user data").expect("блокирующий файл создан");
    let config_path = blocking_file.join("config.toml");

    let loaded = load_or_recover_at(&config_path, recovery_moment()).expect("запуск не падает");

    assert_eq!(loaded.origin, ConfigLoadOrigin::DefaultsInMemory);
    assert!(matches!(
        loaded.save_policy,
        ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::ConfigFileUnreadable { .. })
    ));
    assert_eq!(fs::read(&blocking_file).expect("файл цел"), b"user data");
}

/// Каталог только для чтения при первом запуске: атомарная запись не оставляет ничего.
#[cfg(unix)]
#[test]
fn first_write_into_read_only_directory_leaves_nothing_behind() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::tempdir().expect("temp dir создан");
    let config_path = temp_dir.path().join("config.toml");
    fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o500)).expect("права сняты");
    if fs::write(temp_dir.path().join("probe"), b"").is_ok() {
        eprintln!("пропуск: процесс пишет в read-only каталог (root)");
        return;
    }

    let strict_error = load_or_create_at(&config_path).expect_err("строгий путь сообщает ошибку");
    let loaded = load_or_recover_at(&config_path, recovery_moment()).expect("запуск не падает");
    fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o700)).expect("права вернули");

    assert!(matches!(
        strict_error,
        ConfigError::ReplaceConfigFile { .. }
    ));
    assert!(matches!(
        loaded.save_policy,
        ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::DefaultConfigNotWritten(
            ConfigWriteProblem::Write(_)
        ))
    ));
    assert_eq!(
        fs::read_dir(temp_dir.path())
            .expect("каталог читается")
            .count(),
        0
    );
}

/// Похожие на temp, но чужие файлы при старте не удаляются.
#[test]
fn foreign_temp_like_files_survive_startup_cleanup() {
    let (temp_dir, config_path) = config_dir_with(default_config_text().as_bytes());
    let foreign_names = [
        ".playlist-state.json.save-1-1.tmp",
        ".config.toml.save-1-1.tmp.user-note",
        "config.toml.1.1.tmp",
        ".config.toml.save-x-1.tmp",
        "notes.tmp",
    ];
    for foreign_name in foreign_names {
        fs::write(temp_dir.path().join(foreign_name), b"keep").expect("чужой файл создан");
    }

    load_or_recover_at(&config_path, recovery_moment()).expect("запуск не падает");

    for foreign_name in foreign_names {
        assert_eq!(
            fs::read(temp_dir.path().join(foreign_name)).expect("чужой файл на месте"),
            b"keep",
            "{foreign_name} не должен удаляться"
        );
    }
}

/// Регрессия: корректный config загружается как раньше и не переписывается.
#[test]
fn valid_config_loads_unchanged() {
    let mut custom_config = AppConfig::default();
    custom_config.ui.sidebar.width_points = 500;
    let custom_text = custom_config
        .to_pretty_toml()
        .expect("config сериализуется");
    let (temp_dir, config_path) = config_dir_with(custom_text.as_bytes());

    let loaded = load_or_recover_at(&config_path, recovery_moment()).expect("загрузка успешна");

    assert_eq!(loaded.origin, ConfigLoadOrigin::LoadedExisting);
    assert_eq!(loaded.save_policy, ConfigSavePolicy::WriteToFile);
    assert_eq!(loaded.config, custom_config);
    assert_eq!(
        fs::read_to_string(&config_path).expect("файл на месте"),
        custom_text
    );
    assert_eq!(
        fs::read_dir(temp_dir.path())
            .expect("каталог читается")
            .count(),
        1
    );

    // Сохранение в обычном режиме реально пишет файл.
    custom_config.ui.sidebar.width_points = 450;
    assert_eq!(
        loaded
            .save_target()
            .save(&custom_config)
            .expect("save успешен"),
        ConfigSaveOutcome::Saved
    );
    assert_eq!(
        load_from_path(&config_path).expect("перечитан").config,
        custom_config
    );
}

/// Первый запуск: config создаётся атомарно, только для владельца (0600).
#[cfg(unix)]
#[test]
fn missing_config_is_created_user_only() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::tempdir().expect("temp dir создан");
    let config_path = temp_dir.path().join("fastiplayer").join("config.toml");

    let loaded = load_or_recover_at(&config_path, recovery_moment()).expect("запуск не падает");

    assert_eq!(loaded.origin, ConfigLoadOrigin::CreatedDefault);
    assert_eq!(loaded.save_policy, ConfigSavePolicy::WriteToFile);
    let mode = fs::metadata(&config_path)
        .expect("config создан")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}
