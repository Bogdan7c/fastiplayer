//! Чтение config-файла с диска и вердикт «что в нём лежит» без побочных эффектов.
//!
//! Один разбор на всех читателей: восстановление при старте (`store/recovery.rs`),
//! запись «общей доски» под замком и слежение соседних копий (`store/shared_file.rs`).
//! Модуль ничего не пишет, не переименовывает и не удаляет — решения о файле
//! принимают вызывающие модули по вердикту [`DiskConfigState`].

use std::fs;
use std::io;
use std::path::Path;

use crate::{AppConfig, CURRENT_SCHEMA_VERSION, ConfigError, ConfigResult};

use super::outcome::BrokenConfigProblem;

/// Что лежит по пути config-а.
pub(super) enum DiskConfigState {
    /// Файла нет.
    Missing,
    /// По пути лежит не обычный файл (каталог, FIFO и т. п.).
    NotRegularFile,
    /// Файл есть, но прочитать его нельзя (права, ошибка диска).
    ReadFailed {
        /// Безопасный класс I/O ошибки (без пути).
        error_kind: io::ErrorKind,
    },
    /// Файл записан более новой версией плеера.
    NewerSchema {
        /// Версия схемы, найденная в файле.
        found: i64,
    },
    /// Файл повреждён: синтаксис, неизвестный ключ, значение вне диапазона.
    Broken(BrokenConfigProblem),
    /// Корректный config (в куче: он намного больше остальных вариантов).
    Valid(Box<AppConfig>),
}

/// Читает config-файл и классифицирует его содержимое.
///
/// `Err` — только ошибка, не относящаяся к содержимому файла (ошибка программы):
/// её не маскируем под «повреждён», чтобы восстановление не переименовало исправный файл.
pub(super) fn read_disk_config(path: &Path) -> ConfigResult<DiskConfigState> {
    let config_bytes = match fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => return Ok(DiskConfigState::NotRegularFile),
        Ok(_) => match fs::read(path) {
            Ok(config_bytes) => config_bytes,
            // Файл исчез между проверкой и чтением — ведём себя как при отсутствии файла.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(DiskConfigState::Missing);
            }
            Err(error) => {
                return Ok(DiskConfigState::ReadFailed {
                    error_kind: error.kind(),
                });
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(DiskConfigState::Missing);
        }
        Err(error) => {
            return Ok(DiskConfigState::ReadFailed {
                error_kind: error.kind(),
            });
        }
    };
    inspect_config_bytes(path, &config_bytes)
}

/// Разбирает содержимое и классифицирует проблему.
///
/// Версию схемы читаем из «сырого» TOML **до** строгой проверки: файл новой версии
/// почти наверняка содержит незнакомые нам ключи, и строгий разбор назвал бы его
/// повреждённым, хотя он исправен — просто не для нас. Битый UTF-8 — тоже
/// «повреждён», а не «не читается».
fn inspect_config_bytes(path: &Path, config_bytes: &[u8]) -> ConfigResult<DiskConfigState> {
    let Ok(toml_text) = std::str::from_utf8(config_bytes) else {
        return Ok(DiskConfigState::Broken(
            BrokenConfigProblem::InvalidTomlSyntax,
        ));
    };
    let Ok(raw_document) = toml::from_str::<toml::Table>(toml_text) else {
        return Ok(DiskConfigState::Broken(
            BrokenConfigProblem::InvalidTomlSyntax,
        ));
    };
    if let Some(toml::Value::Integer(found)) = raw_document.get("schema_version")
        && *found > i64::from(CURRENT_SCHEMA_VERSION)
    {
        return Ok(DiskConfigState::NewerSchema { found: *found });
    }
    match super::parse_config_text(path, toml_text) {
        Ok(config) => Ok(DiskConfigState::Valid(Box::new(config))),
        Err(error) => classify_parse_error(error).map(DiskConfigState::Broken),
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
