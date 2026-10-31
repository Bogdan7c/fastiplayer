//! Снятие запрета «только на сеанс», когда файл config-а снова нормальный (04c, пакет F).
//!
//! Тесты идут через настоящую загрузку `load_or_recover_at` и настоящую запись под замком:
//! наблюдаемый результат — байты файла на диске и итог записи, а не флаги.

use std::time::{Duration, SystemTime};

use super::*;
use crate::{
    ConfigLockFailure, ConfigSessionOnlyReason, ConfigWriteResumption, MissingConfigFileSeed,
    SharedConfigLockProblem, SharedConfigSave, SharedConfigUpdate, config_write_lock_path_for,
};

/// Срок ожидания замка при записи в тестах.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// Пишет громкость 0.4 через цель сохранения и сообщает итог.
fn write_volume(target: &ConfigSaveTarget) -> SharedConfigSave<()> {
    target.update_shared_file(LOCK_WAIT, MissingConfigFileSeed::Defaults, |config| {
        config.audio.volume = 0.4;
        Ok(())
    })
}

/// Обычный плеер стартовал, когда замок был непригоден (на его месте каталог), а файла
/// не было: только на сеанс. Потом замок стал обычным, сосед создал нормальный файл —
/// после снятия запрета запись идёт в файл.
#[test]
fn lock_unavailable_at_startup_resumes_writing_once_the_file_is_usable() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let lock_path = config_write_lock_path_for(&config_path);
    fs::create_dir(&lock_path).expect("a directory blocks the lock");
    let loaded = load_or_recover_at(&config_path, SystemTime::now()).expect("startup load");
    assert_eq!(
        loaded.save_policy,
        ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::ConfigLockUnavailable(
            ConfigLockFailure::Unavailable(SharedConfigLockProblem::NotRegularFile)
        ))
    );
    let mut target = loaded.save_target();
    assert!(matches!(
        write_volume(&target),
        SharedConfigSave::KeptInMemoryOnly(_)
    ));
    assert!(
        !config_path.exists(),
        "session-only must not create the file"
    );

    fs::remove_dir(&lock_path).expect("the lock path is free again");
    let mut neighbor = AppConfig::default();
    neighbor.ui.language = "en".to_string();
    save_validated_atomic_at(&config_path, &neighbor).expect("neighbor creates the file");
    let resumption = target.resume_writing_after_usable_file();

    assert!(
        matches!(resumption, ConfigWriteResumption::Resumed { .. }),
        "{resumption:?}"
    );
    assert_eq!(target.policy(), &ConfigSavePolicy::WriteToFile);
    assert!(matches!(
        write_volume(&target),
        SharedConfigSave::Attempted(SharedConfigUpdate::Written { .. })
    ));
    let on_disk = load_from_path(&config_path).expect("file loads").config;
    assert!((on_disk.audio.volume - 0.4).abs() < f64::EPSILON);
    assert_eq!(on_disk.ui.language, "en", "the neighbour's key stays");
}

/// Файл новой версии: запрет не снимается, файл не трогаем.
#[test]
fn newer_schema_file_never_resumes_writing() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let newer_text = "schema_version = 999\nfuture_key = 1\n";
    fs::write(&config_path, newer_text).expect("newer file written");
    let loaded = load_or_recover_at(&config_path, SystemTime::now()).expect("startup load");
    let mut target = loaded.save_target();

    let resumption = target.resume_writing_after_usable_file();

    assert_eq!(
        resumption,
        ConfigWriteResumption::StillSessionOnly(ConfigSessionOnlyReason::NewerSchemaVersion {
            found: 999
        })
    );
    assert!(matches!(
        write_volume(&target),
        SharedConfigSave::KeptInMemoryOnly(_)
    ));
    assert_eq!(fs::read_to_string(&config_path).expect("file"), newer_text);
}

/// Список восстановимых причин: всё, что про состояние файла, — да; новая схема и
/// неизвестный каталог — нет. Уже записываемая цель остаётся как есть.
#[test]
fn only_file_state_reasons_lift_and_writable_target_is_unchanged() {
    let lifting = [
        ConfigSessionOnlyReason::BrokenConfigLeftToRegularPlayer(
            BrokenConfigProblem::InvalidTomlSyntax,
        ),
        ConfigSessionOnlyReason::BackupFailed {
            error_kind: io::ErrorKind::PermissionDenied,
        },
        ConfigSessionOnlyReason::ConfigFileUnreadable {
            error_kind: io::ErrorKind::PermissionDenied,
        },
        ConfigSessionOnlyReason::ConfigPathIsNotFile,
        ConfigSessionOnlyReason::ConfigLockUnavailable(ConfigLockFailure::Busy {
            waited: Duration::from_secs(2),
        }),
    ];
    for reason in lifting {
        assert!(reason.lifts_when_file_becomes_usable(), "{reason:?}");
    }
    for reason in [
        ConfigSessionOnlyReason::NewerSchemaVersion { found: 99 },
        ConfigSessionOnlyReason::ConfigDirectoryUnknown,
    ] {
        assert!(!reason.lifts_when_file_becomes_usable(), "{reason:?}");
    }

    let mut writable = ConfigSaveTarget::writable("config.toml");
    assert_eq!(
        writable.resume_writing_after_usable_file(),
        ConfigWriteResumption::AlreadyWritable
    );
}

/// «Что лежит на диске» известно только когда файл прочитан или записан при загрузке.
#[test]
fn config_seen_on_disk_follows_the_load_outcome() {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    let created = load_or_recover_at(&config_path, SystemTime::now()).expect("created");
    let loaded = load_or_recover_at(&config_path, SystemTime::now()).expect("loaded");
    let in_memory = LoadedConfig {
        config: AppConfig::default(),
        path: config_path.clone(),
        origin: ConfigLoadOrigin::DefaultsInMemory,
        save_policy: ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::ConfigPathIsNotFile),
    };

    assert_eq!(created.origin, ConfigLoadOrigin::CreatedDefault);
    assert_eq!(created.config_seen_on_disk(), Some(&AppConfig::default()));
    assert_eq!(loaded.origin, ConfigLoadOrigin::LoadedExisting);
    assert_eq!(loaded.config_seen_on_disk(), Some(&loaded.config));
    assert_eq!(in_memory.config_seen_on_disk(), None);
}
