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

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::{AppConfig, CURRENT_SCHEMA_VERSION, ConfigError, ConfigResult};

use super::LoadedConfig;
use super::atomic;
use super::backup_name::BrokenConfigBackupName;
use super::outcome::{
    BrokenConfigProblem, BrokenConfigRecovery, ConfigLoadOrigin, ConfigSavePolicy,
    ConfigSessionOnlyReason, ConfigWriteProblem,
};

/// Сколько вариантов имени резервной копии пробуем, если имя с этой секундой уже занято.
const MAX_BACKUP_NAME_ATTEMPTS: u32 = 100;

/// Загружает config для запуска приложения, восстанавливаясь после плохого файла.
///
/// Вызывать только под lease единственного экземпляра: функция убирает брошенные
/// temp-файлы и переименовывает повреждённый config, что безопасно лишь тогда, когда
/// параллельно никто не пишет в тот же каталог. `now` нужен для имени резервной копии
/// и передаётся снаружи, чтобы тесты управляли временем.
///
/// Ошибкой остаются только сбои подготовки defaults (ошибка программы, а не файла).
pub fn load_or_recover_at(path: impl AsRef<Path>, now: SystemTime) -> ConfigResult<LoadedConfig> {
    let path = path.as_ref().to_path_buf();
    atomic::remove_stale_save_temp_files(&path);

    match read_config_bytes(&path) {
        ConfigFileRead::Missing => {
            create_default_config_file(path, ConfigLoadOrigin::CreatedDefault)
        }
        ConfigFileRead::Unusable(reason) => defaults_in_memory(path, reason),
        ConfigFileRead::Bytes(config_bytes) => match inspect_config_bytes(&path, &config_bytes)? {
            ConfigInspection::Valid(config) => Ok(LoadedConfig {
                config: *config,
                path,
                origin: ConfigLoadOrigin::LoadedExisting,
                save_policy: ConfigSavePolicy::WriteToFile,
            }),
            ConfigInspection::NewerSchema { found } => {
                defaults_in_memory(path, ConfigSessionOnlyReason::NewerSchemaVersion { found })
            }
            ConfigInspection::Broken(problem) => recover_broken_config(path, problem, now),
        },
    }
}

/// Что удалось прочитать по пути config-а.
enum ConfigFileRead {
    /// Файла нет.
    Missing,
    /// Файл есть, но использовать его нельзя; трогать его тоже нельзя.
    Unusable(ConfigSessionOnlyReason),
    /// Содержимое файла.
    Bytes(Vec<u8>),
}

/// Читает config-файл байтами: битый UTF-8 — тоже «повреждён», а не «не читается».
fn read_config_bytes(path: &Path) -> ConfigFileRead {
    match fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            return ConfigFileRead::Unusable(ConfigSessionOnlyReason::ConfigPathIsNotFile);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return ConfigFileRead::Missing,
        Err(error) => return unreadable(&error),
    }
    match fs::read(path) {
        Ok(config_bytes) => ConfigFileRead::Bytes(config_bytes),
        // Файл исчез между проверкой и чтением — ведём себя как при первом запуске.
        Err(error) if error.kind() == io::ErrorKind::NotFound => ConfigFileRead::Missing,
        Err(error) => unreadable(&error),
    }
}

/// Файл есть, но прочитать его нельзя.
fn unreadable(error: &io::Error) -> ConfigFileRead {
    ConfigFileRead::Unusable(ConfigSessionOnlyReason::ConfigFileUnreadable {
        error_kind: error.kind(),
    })
}

/// Вердикт по содержимому config-файла.
enum ConfigInspection {
    /// Корректный config (в куче: он намного больше остальных вариантов).
    Valid(Box<AppConfig>),
    /// Файл от более новой версии плеера.
    NewerSchema { found: i64 },
    /// Файл повреждён.
    Broken(BrokenConfigProblem),
}

/// Разбирает содержимое и классифицирует проблему.
///
/// Версию схемы читаем из «сырого» TOML **до** строгой проверки: файл новой версии
/// почти наверняка содержит незнакомые нам ключи, и строгий разбор назвал бы его
/// повреждённым, хотя он исправен — просто не для нас.
fn inspect_config_bytes(path: &Path, config_bytes: &[u8]) -> ConfigResult<ConfigInspection> {
    let Ok(toml_text) = std::str::from_utf8(config_bytes) else {
        return Ok(ConfigInspection::Broken(
            BrokenConfigProblem::InvalidTomlSyntax,
        ));
    };
    let Ok(raw_document) = toml::from_str::<toml::Table>(toml_text) else {
        return Ok(ConfigInspection::Broken(
            BrokenConfigProblem::InvalidTomlSyntax,
        ));
    };
    if let Some(toml::Value::Integer(found)) = raw_document.get("schema_version")
        && *found > i64::from(CURRENT_SCHEMA_VERSION)
    {
        return Ok(ConfigInspection::NewerSchema { found: *found });
    }
    match super::parse_config_text(path, toml_text) {
        Ok(config) => Ok(ConfigInspection::Valid(Box::new(config))),
        Err(error) => classify_parse_error(error).map(ConfigInspection::Broken),
    }
}

/// Переводит ошибку строгого разбора в проблему файла.
///
/// Неожиданная ошибка (не о содержимом файла) не маскируется под «повреждён»:
/// она пробрасывается как есть, чтобы не переименовать исправный файл.
fn classify_parse_error(error: ConfigError) -> ConfigResult<BrokenConfigProblem> {
    match error {
        ConfigError::ParseConfigFile { .. } => Ok(BrokenConfigProblem::SchemaMismatch),
        ConfigError::ValidateConfigFile { source, .. } => match *source {
            ConfigError::InvalidValue { field, .. } => {
                Ok(BrokenConfigProblem::InvalidValue { field })
            }
            other => Err(other),
        },
        other => Err(other),
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
/// вместе, но гонки нет: каталог config-а меняет только процесс, удерживающий lease.
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
