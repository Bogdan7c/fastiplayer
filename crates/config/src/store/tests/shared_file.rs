//! «Общая доска» config-а (решение 135 моста): запись под замком только своих ключей
//! поверх перечитанного файла и чтение для слежения без побочных эффектов.
//!
//! Все тесты работают с настоящей файловой системой во временном каталоге и проверяют
//! наблюдаемый результат: байты файла на диске, наличие `.bak`, итог операции.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::*;
use crate::store::shared_file::{ConfigWriteGuard, acquire_config_write_lock};
use crate::{
    BrokenConfigProblem, ConfigLockFailure, SharedConfigLockProblem, SharedConfigProblem,
    SharedConfigRead, SharedConfigUpdate, config_write_lock_path_for, read_shared_config_at,
    update_shared_config_at,
};

/// Короткий срок ожидания замка для тестов «занят»: тест не должен ждать секунды.
const SHORT_LOCK_WAIT: Duration = Duration::from_millis(60);

/// Срок, за который свободный замок берётся заведомо.
const GENEROUS_LOCK_WAIT: Duration = Duration::from_secs(5);

/// Временный каталог с путём `config.toml` (файл не создаётся).
fn empty_config_dir() -> (tempfile::TempDir, PathBuf) {
    let temp_dir = tempfile::tempdir().expect("temp dir created");
    let config_path = temp_dir.path().join("config.toml");
    (temp_dir, config_path)
}

/// Временный каталог с `config.toml` заданного содержимого.
fn config_dir_with(config_text: &str) -> (tempfile::TempDir, PathBuf) {
    let (temp_dir, config_path) = empty_config_dir();
    fs::write(&config_path, config_text).expect("config fixture written");
    (temp_dir, config_path)
}

/// Слияние «громкость 0.25» — своё изменение вызывающего кода.
fn set_volume_quarter(config: &mut AppConfig) -> Result<(), &'static str> {
    config.audio.volume = 0.25;
    Ok(())
}

/// Берёт замок записи «соседом»: отдельный дескриптор, как у другого процесса (`flock`
/// исключает разные открытые описания файла и внутри одного процесса).
fn hold_lock_as_neighbor(config_path: &std::path::Path) -> ConfigWriteGuard {
    let guard = acquire_config_write_lock(&config_write_lock_path_for(config_path), Duration::ZERO)
        .expect("neighbor takes the free lock");
    assert!(matches!(guard, ConfigWriteGuard::Locked { .. }));
    guard
}

/// Имена файлов каталога, кроме самого config-а и замка (резервные копии, temp).
fn extra_entries(directory: &std::path::Path) -> Vec<String> {
    fs::read_dir(directory)
        .expect("directory readable")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "config.toml" && name != "config.toml.lock")
        .collect()
}

/// Нет файла → основа defaults, файл создан с defaults и изменённым ключом.
#[test]
fn missing_file_is_created_from_defaults_plus_changed_key() {
    let (_temp_dir, config_path) = empty_config_dir();

    let outcome = update_shared_config_at(&config_path, GENEROUS_LOCK_WAIT, set_volume_quarter);

    let SharedConfigUpdate::Written { written } = outcome else {
        panic!("missing file must be created: {outcome:?}");
    };
    let mut expected = AppConfig::default();
    expected.audio.volume = 0.25;
    assert_eq!(*written, expected);
    let on_disk = load_from_path(&config_path).expect("created file loads");
    assert_eq!(on_disk.config, expected);
}

/// Сосед поменял X на диске, мы пишем Y: X сохранился, Y записан.
#[test]
fn merge_keeps_keys_written_by_neighbor() {
    let mut neighbor_config = AppConfig::default();
    neighbor_config.ui.sidebar.width_points = 500;
    let (_temp_dir, config_path) = empty_config_dir();
    save_validated_atomic_at(&config_path, &neighbor_config).expect("neighbor wrote X");

    let outcome = update_shared_config_at(&config_path, GENEROUS_LOCK_WAIT, set_volume_quarter);

    assert!(matches!(outcome, SharedConfigUpdate::Written { .. }));
    let on_disk = load_from_path(&config_path).expect("merged file loads");
    assert_eq!(on_disk.config.ui.sidebar.width_points, 500);
    assert!((on_disk.config.audio.volume - 0.25).abs() < f64::EPSILON);
}

/// Битый файл → ошибка, файл байт-в-байт прежний, `.bak` не создан, слияние не вызвано.
#[test]
fn broken_file_is_refused_without_backup_or_change() {
    let broken_text = "schema_version = 29\n[[[";
    let (temp_dir, config_path) = config_dir_with(broken_text);
    let mut merge_called = false;

    let outcome = update_shared_config_at(&config_path, GENEROUS_LOCK_WAIT, |config| {
        merge_called = true;
        set_volume_quarter(config)
    });

    assert!(matches!(
        outcome,
        SharedConfigUpdate::DiskConfigUnusable(SharedConfigProblem::Broken(
            BrokenConfigProblem::InvalidTomlSyntax
        ))
    ));
    assert!(!merge_called);
    assert_eq!(fs::read_to_string(&config_path).expect("file"), broken_text);
    assert!(extra_entries(temp_dir.path()).is_empty());
}

/// Значение вне диапазона на диске — тоже битый файл: не перезаписываем defaults-ами.
#[test]
fn out_of_range_value_on_disk_is_refused_without_change() {
    let broken_text = "schema_version = 29\n\n[ui.sidebar]\nwidth_points = 5\n";
    let (temp_dir, config_path) = config_dir_with(broken_text);

    let outcome = update_shared_config_at(&config_path, GENEROUS_LOCK_WAIT, set_volume_quarter);

    assert!(matches!(
        outcome,
        SharedConfigUpdate::DiskConfigUnusable(SharedConfigProblem::Broken(
            BrokenConfigProblem::InvalidValue {
                field: "ui.sidebar.width_points"
            }
        ))
    ));
    assert_eq!(fs::read_to_string(&config_path).expect("file"), broken_text);
    assert!(extra_entries(temp_dir.path()).is_empty());
}

/// Файл новой версии → ошибка, файл не изменён.
#[test]
fn newer_schema_file_is_refused_without_change() {
    let newer_text = AppConfig::default()
        .to_pretty_toml()
        .expect("defaults serialize")
        .replace("schema_version = 29", "schema_version = 99");
    let (temp_dir, config_path) = config_dir_with(&newer_text);

    let outcome = update_shared_config_at(&config_path, GENEROUS_LOCK_WAIT, set_volume_quarter);

    assert!(matches!(
        outcome,
        SharedConfigUpdate::DiskConfigNewer { found: 99 }
    ));
    assert_eq!(fs::read_to_string(&config_path).expect("file"), newer_text);
    assert!(extra_entries(temp_dir.path()).is_empty());
}

/// Замок держит сосед дольше срока → «занят», файл байт-в-байт прежний.
#[test]
fn busy_lock_is_reported_after_wait_and_file_is_untouched() {
    let original_text = AppConfig::default()
        .to_pretty_toml()
        .expect("defaults serialize");
    let (_temp_dir, config_path) = config_dir_with(&original_text);
    let neighbor_guard = hold_lock_as_neighbor(&config_path);
    let started_at = Instant::now();

    let outcome = update_shared_config_at(&config_path, SHORT_LOCK_WAIT, set_volume_quarter);

    let SharedConfigUpdate::LockBusy { waited } = outcome else {
        panic!("held lock must report busy: {outcome:?}");
    };
    assert!(waited >= SHORT_LOCK_WAIT);
    // Ждали ограниченно, а не вечно.
    assert!(started_at.elapsed() < GENEROUS_LOCK_WAIT);
    assert_eq!(
        fs::read_to_string(&config_path).expect("file"),
        original_text
    );
    drop(neighbor_guard);
}

/// Сосед отпускает замок в пределах срока → запись проходит после ожидания.
#[test]
fn lock_released_within_wait_lets_update_through() {
    let (_temp_dir, config_path) = empty_config_dir();
    let neighbor_guard = hold_lock_as_neighbor(&config_path);
    let release_thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        drop(neighbor_guard);
    });

    let outcome = update_shared_config_at(&config_path, GENEROUS_LOCK_WAIT, set_volume_quarter);

    release_thread.join().expect("release thread finished");
    assert!(matches!(outcome, SharedConfigUpdate::Written { .. }));
    let on_disk = load_from_path(&config_path).expect("file loads");
    assert!((on_disk.config.audio.volume - 0.25).abs() < f64::EPSILON);
}

/// Отказ функции слияния → ошибка без записи.
#[test]
fn merge_rejection_writes_nothing() {
    let original_text = AppConfig::default()
        .to_pretty_toml()
        .expect("defaults serialize");
    let (_temp_dir, config_path) = config_dir_with(&original_text);

    let outcome = update_shared_config_at(&config_path, GENEROUS_LOCK_WAIT, |_config| {
        Err("registry refused")
    });

    assert!(matches!(
        outcome,
        SharedConfigUpdate::MergeRejected("registry refused")
    ));
    assert_eq!(
        fs::read_to_string(&config_path).expect("file"),
        original_text
    );
}

/// Слитый документ нарушает проверку (сосед и мы вместе дали недопустимую пару) → без записи.
#[test]
fn invalid_merged_document_writes_nothing() {
    let original_text = AppConfig::default()
        .to_pretty_toml()
        .expect("defaults serialize");
    let (_temp_dir, config_path) = config_dir_with(&original_text);

    let outcome = update_shared_config_at(&config_path, GENEROUS_LOCK_WAIT, |config| {
        config.ui.sidebar.width_points = 5;
        Ok::<(), ()>(())
    });

    let SharedConfigUpdate::MergedConfigRejected(error) = outcome else {
        panic!("invalid merged document must be rejected: {outcome:?}");
    };
    assert!(error.to_string().contains("ui.sidebar.width_points"));
    assert_eq!(
        fs::read_to_string(&config_path).expect("file"),
        original_text
    );
}

/// Старый файл (v28) поднимается миграцией и записывается текущей схемой с новой таблицей.
#[test]
fn update_migrates_older_file_and_keeps_user_values() {
    let (_temp_dir, config_path) =
        config_dir_with("schema_version = 28\n\n[ui.sidebar]\nwidth_points = 480\n");

    let outcome = update_shared_config_at(&config_path, GENEROUS_LOCK_WAIT, set_volume_quarter);

    assert!(matches!(outcome, SharedConfigUpdate::Written { .. }));
    let saved_text = fs::read_to_string(&config_path).expect("file");
    assert!(saved_text.contains("schema_version = 29"));
    assert!(saved_text.contains("[browser_session.window]"));
    assert!(saved_text.contains("width_points = 480"));
}

/// Файл-замок создаётся с правами 0600, существующий «широкий» замок сужается, и после
/// записи замок остаётся на месте (никогда не удаляется).
#[cfg(unix)]
#[test]
fn lock_file_is_private_and_never_removed() {
    use std::os::unix::fs::PermissionsExt;

    let (_temp_dir, config_path) = empty_config_dir();
    let lock_path = config_write_lock_path_for(&config_path);

    let first = update_shared_config_at(&config_path, GENEROUS_LOCK_WAIT, set_volume_quarter);
    assert!(matches!(first, SharedConfigUpdate::Written { .. }));
    let lock_mode = fs::metadata(&lock_path)
        .expect("lock kept")
        .permissions()
        .mode();
    assert_eq!(lock_mode & 0o777, 0o600);

    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o666)).expect("widen lock");
    let second = update_shared_config_at(&config_path, GENEROUS_LOCK_WAIT, set_volume_quarter);
    assert!(matches!(second, SharedConfigUpdate::Written { .. }));
    let lock_mode = fs::metadata(&lock_path)
        .expect("lock kept")
        .permissions()
        .mode();
    assert_eq!(lock_mode & 0o777, 0o600);
}

/// Замок — символическая ссылка: отказ, цель ссылки не тронута, config не записан.
#[cfg(unix)]
#[test]
fn symlinked_lock_is_refused() {
    let (temp_dir, config_path) = empty_config_dir();
    let link_target = temp_dir.path().join("victim.txt");
    fs::write(&link_target, b"keep").expect("victim written");
    std::os::unix::fs::symlink(&link_target, config_write_lock_path_for(&config_path))
        .expect("symlink planted");

    let outcome = update_shared_config_at(&config_path, GENEROUS_LOCK_WAIT, set_volume_quarter);

    assert!(matches!(
        outcome,
        SharedConfigUpdate::LockUnavailable(SharedConfigLockProblem::NotRegularFile)
    ));
    assert_eq!(fs::read(&link_target).expect("victim"), b"keep");
    assert!(!config_path.exists());
}

/// Замок — каталог (чужой тип файла): отказ без записи config-а.
#[test]
fn directory_at_lock_path_is_refused() {
    let (_temp_dir, config_path) = empty_config_dir();
    fs::create_dir(config_write_lock_path_for(&config_path)).expect("directory planted");

    let outcome = update_shared_config_at(&config_path, GENEROUS_LOCK_WAIT, set_volume_quarter);

    assert!(matches!(
        outcome,
        SharedConfigUpdate::LockUnavailable(SharedConfigLockProblem::NotRegularFile)
    ));
    assert!(!config_path.exists());
}

/// Каталог только для чтения: замок не создаётся, но и писать некому — запись пробуется
/// и честно сообщает «не записано», файл прежний.
#[cfg(unix)]
#[test]
fn read_only_directory_reports_write_failure_without_change() {
    use std::os::unix::fs::PermissionsExt;

    let original_text = AppConfig::default()
        .to_pretty_toml()
        .expect("defaults serialize");
    let (temp_dir, config_path) = config_dir_with(&original_text);
    fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o500)).expect("read-only");
    if fs::write(temp_dir.path().join("probe"), b"").is_ok() {
        eprintln!("Pass: the process writes to read-only directory (root)");
        return;
    }

    let outcome = update_shared_config_at(&config_path, GENEROUS_LOCK_WAIT, set_volume_quarter);
    fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o700)).expect("rights back");

    assert!(matches!(outcome, SharedConfigUpdate::WriteFailed(_)));
    assert_eq!(
        fs::read_to_string(&config_path).expect("file"),
        original_text
    );
}

/// Чтение для слежения различает все состояния и ничего не создаёт.
#[test]
fn read_shared_config_distinguishes_every_state_without_side_effects() {
    let (temp_dir, config_path) = empty_config_dir();
    assert_eq!(
        read_shared_config_at(&config_path),
        SharedConfigRead::Missing
    );
    assert!(!config_path.exists());
    assert!(!config_write_lock_path_for(&config_path).exists());

    let mut custom = AppConfig::default();
    custom.audio.volume = 0.5;
    save_validated_atomic_at(&config_path, &custom).expect("valid file written");
    assert_eq!(
        read_shared_config_at(&config_path),
        SharedConfigRead::Parsed(Box::new(custom))
    );

    fs::write(&config_path, "[[[").expect("broken file written");
    assert_eq!(
        read_shared_config_at(&config_path),
        SharedConfigRead::Unreadable {
            problem: SharedConfigProblem::Broken(BrokenConfigProblem::InvalidTomlSyntax)
        }
    );

    fs::write(&config_path, "schema_version = 99\nfuture_key = 1\n").expect("newer written");
    assert_eq!(
        read_shared_config_at(&config_path),
        SharedConfigRead::Newer { found: 99 }
    );
    // Ни резервных копий, ни замка, ни temp: чтение без побочных эффектов.
    assert!(extra_entries(temp_dir.path()).is_empty());
    assert!(!config_write_lock_path_for(&config_path).exists());

    fs::remove_file(&config_path).expect("file removed");
    fs::create_dir(&config_path).expect("directory at config path");
    assert_eq!(
        read_shared_config_at(&config_path),
        SharedConfigRead::Unreadable {
            problem: SharedConfigProblem::NotRegularFile
        }
    );
}

/// Старт при занятом замке и битом файле: каталог не трогаем (нет `.bak`, temp соседа
/// цел), defaults в памяти без записи с понятной причиной.
#[test]
fn startup_with_busy_lock_leaves_directory_untouched() {
    let (temp_dir, config_path) = config_dir_with("[[[");
    // Temp «соседа посреди записи» по шаблону atomic-file-store.
    let neighbor_temp = temp_dir.path().join(".config.toml.save-4242-1.tmp");
    fs::write(&neighbor_temp, b"half written").expect("neighbor temp written");
    let neighbor_guard = hold_lock_as_neighbor(&config_path);

    let loaded = load_or_recover_at(&config_path, std::time::SystemTime::now())
        .expect("startup does not fail");
    drop(neighbor_guard);

    assert_eq!(loaded.origin, ConfigLoadOrigin::DefaultsInMemory);
    assert!(matches!(
        loaded.save_policy,
        ConfigSavePolicy::SessionOnly(ConfigSessionOnlyReason::ConfigLockUnavailable(
            ConfigLockFailure::Busy { .. }
        ))
    ));
    assert_eq!(fs::read_to_string(&config_path).expect("file"), "[[[");
    assert_eq!(
        fs::read(&neighbor_temp).expect("neighbor temp"),
        b"half written"
    );
    assert_eq!(
        extra_entries(temp_dir.path()),
        vec![".config.toml.save-4242-1.tmp".to_string()]
    );
}
