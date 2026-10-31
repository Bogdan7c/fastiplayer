//! Восстановление после плохого config-файла при старте приложения.
//!
//! Решение владельца (UX edge cases, сессия 05, вариант 2а): плеер не должен отказываться
//! запускаться из-за config-а. Что делаем с файлом, решает config-хранилище:
//!
//! | Что с файлом | Действие | Запись в этот запуск |
//! |---|---|---|
//! | нет файла | создать defaults атомарно | да |
//! | корректный | загрузить как раньше | да |
//! | мусор / неизвестный ключ / значение вне диапазона / старая версия | переименовать в `.bak`, записать defaults | да |
//! | версия схемы новее нашей | не трогать, defaults в памяти | нет |
//! | не читается (права, диск) или это не файл | не трогать, defaults в памяти | нет |
//!
//! Главный инвариант: **повреждённый файл пользователя никогда не удаляется и не
//! перезаписывается**. Он только переименовывается в резервную копию; если это не
//! получилось, файл остаётся на месте, а запуск идёт без записи.
//!
//! Строгие `load_or_create_at` / `load_from_path` не меняются: восстановление — это
//! отдельный слой над тем же разбором.
//!
//! Config пишут несколько копий плеера (решение 135 моста), поэтому всё, что меняет
//! каталог config-а — уборка брошенных temp, создание defaults, перенос битого файла в
//! `.bak`, — идёт под замком записи `config.toml.lock`. Если замок взять не удалось
//! (сосед держит его дольше срока или замок подменён), каталог не трогаем: корректный
//! файл загружается как обычно, а вместо создания и переноса — defaults в памяти без
//! записи ([`ConfigSessionOnlyReason::ConfigLockUnavailable`]).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::{AppConfig, ConfigResult, config_write_lock_path_for};

use super::LoadedConfig;
use super::atomic;
use super::backup_name::BrokenConfigBackupName;
use super::disk_read::{DiskConfigState, read_disk_config};
use super::outcome::{
    BrokenConfigProblem, BrokenConfigRecovery, ConfigLoadOrigin, ConfigSavePolicy,
    ConfigSessionOnlyReason, ConfigWriteProblem,
};
use super::shared_file::{
    ConfigLockFailure, DEFAULT_SHARED_CONFIG_LOCK_WAIT, acquire_config_write_lock,
};

/// Сколько вариантов имени резервной копии пробуем, если имя с этой секундой уже занято.
const MAX_BACKUP_NAME_ATTEMPTS: u32 = 100;

/// Загружает config для запуска приложения, восстанавливаясь после плохого файла.
///
/// Уборка брошенных temp-файлов, создание defaults и переименование повреждённого
/// config-а выполняются под замком записи config-а (ждём не дольше
/// [`DEFAULT_SHARED_CONFIG_LOCK_WAIT`]): только так соседняя копия плеера не потеряет
/// temp посреди записи и не получит свой свежий файл переименованным в `.bak`.
/// `now` нужен для имени резервной копии и передаётся снаружи, чтобы тесты управляли
/// временем.
///
/// Ошибкой остаются только сбои подготовки defaults (ошибка программы, а не файла).
pub fn load_or_recover_at(path: impl AsRef<Path>, now: SystemTime) -> ConfigResult<LoadedConfig> {
    let path = path.as_ref().to_path_buf();
    let lock_path = config_write_lock_path_for(&path);
    // Замок держится до конца загрузки: проверка и изменение каталога — под ним.
    let _write_guard = match acquire_config_write_lock(&lock_path, DEFAULT_SHARED_CONFIG_LOCK_WAIT)
    {
        Ok(write_guard) => write_guard,
        Err(lock_failure) => return load_without_write_lock(path, lock_failure),
    };
    atomic::remove_stale_save_temp_files(&path);

    match read_disk_config(&path)? {
        DiskConfigState::Missing => {
            create_default_config_file(path, ConfigLoadOrigin::CreatedDefault)
        }
        DiskConfigState::Valid(config) => Ok(loaded_existing(path, *config)),
        DiskConfigState::Broken(problem) => recover_broken_config(path, problem, now),
        DiskConfigState::NewerSchema { found } => {
            defaults_in_memory(path, ConfigSessionOnlyReason::NewerSchemaVersion { found })
        }
        DiskConfigState::NotRegularFile => {
            defaults_in_memory(path, ConfigSessionOnlyReason::ConfigPathIsNotFile)
        }
        DiskConfigState::ReadFailed { error_kind } => defaults_in_memory(
            path,
            ConfigSessionOnlyReason::ConfigFileUnreadable { error_kind },
        ),
    }
}

/// Загрузка без замка записи: каталог config-а не трогаем ни в каком случае.
///
/// Корректный файл загружается с правом записи: каждая следующая запись сама берёт
/// замок и при неудаче сообщит понятную ошибку. Нет файла или файл битый — defaults в
/// памяти без записи: создать файл или перенести битый в `.bak` без замка небезопасно.
fn load_without_write_lock(
    path: PathBuf,
    lock_failure: ConfigLockFailure,
) -> ConfigResult<LoadedConfig> {
    match read_disk_config(&path)? {
        DiskConfigState::Valid(config) => Ok(loaded_existing(path, *config)),
        DiskConfigState::Missing | DiskConfigState::Broken(_) => defaults_in_memory(
            path,
            ConfigSessionOnlyReason::ConfigLockUnavailable(lock_failure),
        ),
        // Эти случаи и под замком решаются без изменения каталога — причина та же.
        DiskConfigState::NewerSchema { found } => {
            defaults_in_memory(path, ConfigSessionOnlyReason::NewerSchemaVersion { found })
        }
        DiskConfigState::NotRegularFile => {
            defaults_in_memory(path, ConfigSessionOnlyReason::ConfigPathIsNotFile)
        }
        DiskConfigState::ReadFailed { error_kind } => defaults_in_memory(
            path,
            ConfigSessionOnlyReason::ConfigFileUnreadable { error_kind },
        ),
    }
}

/// Существующий корректный файл: обычный режим с записью.
fn loaded_existing(path: PathBuf, config: AppConfig) -> LoadedConfig {
    LoadedConfig {
        config,
        path,
        origin: ConfigLoadOrigin::LoadedExisting,
        save_policy: ConfigSavePolicy::WriteToFile,
    }
}

/// Сохраняет повреждённый файл в резервную копию и записывает defaults.
fn recover_broken_config(
    path: PathBuf,
    problem: BrokenConfigProblem,
    now: SystemTime,
) -> ConfigResult<LoadedConfig> {
    let backup_file_name = match move_to_backup(&path, now) {
        Ok(backup_file_name) => backup_file_name,
        // Копию сделать не удалось — исходный файл не тронут, запись запрещаем,
        // иначе следующий save перезаписал бы единственный экземпляр данных.
        Err(error_kind) => {
            return defaults_in_memory(path, ConfigSessionOnlyReason::BackupFailed { error_kind });
        }
    };
    let origin = ConfigLoadOrigin::RecoveredFromBroken(BrokenConfigRecovery {
        problem,
        backup_file_name,
    });
    create_default_config_file(path, origin)
}

/// Переименовывает файл в свободное имя резервной копии рядом с ним.
///
/// Rename сохраняет файл целиком (тот же inode, те же байты), поэтому исходные данные
/// пользователя никогда не существуют «наполовину». Занятое имя не перезаписывается:
/// пробуем следующий вариант с суффиксом. Проверка «имя свободно» и rename не атомарны
/// вместе, но гонки нет: каталог config-а меняет только писатель, держащий замок записи.
fn move_to_backup(path: &Path, now: SystemTime) -> Result<String, io::ErrorKind> {
    let Some(directory) = path.parent() else {
        return Err(io::ErrorKind::InvalidInput);
    };
    let config_file_name = path
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .ok_or(io::ErrorKind::InvalidInput)?;
    let backup_name = BrokenConfigBackupName::new(config_file_name, now);
    for attempt in 1..=MAX_BACKUP_NAME_ATTEMPTS {
        let candidate_name = backup_name.candidate(attempt);
        let candidate_path = directory.join(&candidate_name);
        match fs::symlink_metadata(&candidate_path) {
            // Имя занято чем угодно (файлом, каталогом, ссылкой) — не трогаем.
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.kind()),
        }
        return fs::rename(path, &candidate_path)
            .map(|()| candidate_name)
            .map_err(|error| error.kind());
    }
    Err(io::ErrorKind::AlreadyExists)
}

/// Записывает defaults атомарно; если записать нельзя — работаем без записи.
fn create_default_config_file(
    path: PathBuf,
    origin: ConfigLoadOrigin,
) -> ConfigResult<LoadedConfig> {
    let (config, toml_text) = validated_default_document()?;
    let save_policy = match atomic::write_initial_config(&path, &toml_text) {
        Ok(()) => ConfigSavePolicy::WriteToFile,
        Err(error) => match ConfigWriteProblem::from_write_error(&error) {
            Some(problem) => ConfigSavePolicy::SessionOnly(
                ConfigSessionOnlyReason::DefaultConfigNotWritten(problem),
            ),
            None => return Err(error),
        },
    };
    // Если файл восстанавливали, но defaults не записались, итог остаётся
    // «восстановлен» (копия уже сделана), а запрет записи несёт `save_policy`.
    let origin = match (&origin, &save_policy) {
        (ConfigLoadOrigin::CreatedDefault, ConfigSavePolicy::SessionOnly(_)) => {
            ConfigLoadOrigin::DefaultsInMemory
        }
        _ => origin,
    };
    Ok(LoadedConfig {
        config,
        path,
        origin,
        save_policy,
    })
}

/// Defaults только в памяти: файл на диске не трогаем и не пишем.
fn defaults_in_memory(
    path: PathBuf,
    reason: ConfigSessionOnlyReason,
) -> ConfigResult<LoadedConfig> {
    let (config, _toml_text) = validated_default_document()?;
    Ok(LoadedConfig {
        config,
        path,
        origin: ConfigLoadOrigin::DefaultsInMemory,
        save_policy: ConfigSavePolicy::SessionOnly(reason),
    })
}

/// Проверенные defaults и их TOML-текст.
fn validated_default_document() -> ConfigResult<(AppConfig, String)> {
    let config = AppConfig::default();
    config.validate()?;
    let toml_text = config.to_pretty_toml()?;
    Ok((config, toml_text))
}
