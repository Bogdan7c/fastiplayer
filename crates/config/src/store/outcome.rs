//! Типизированный итог загрузки config-а и право записи на текущий запуск.
//!
//! Раньше итог загрузки описывался одним `created: bool`, а любая проблема с файлом
//! была фатальной ошибкой. Теперь config-хранилище само решает, что делать с плохим
//! файлом (см. `store/recovery.rs`), и сообщает приложению **что произошло**
//! ([`ConfigLoadOrigin`]) и **можно ли сохранять изменения** ([`ConfigSavePolicy`]).
//! Приложение только показывает это пользователю.

use std::io;
use std::path::{Path, PathBuf};

use crate::error::ConfigWriteFailure;
use crate::{AppConfig, ConfigError, ConfigResult};

use super::atomic;

/// Откуда взялась конфигурация текущего запуска.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigLoadOrigin {
    /// Существующий файл прочитан и прошёл проверку.
    LoadedExisting,
    /// Файла не было — создан файл с настройками по умолчанию.
    CreatedDefault,
    /// Файл был повреждён: он сохранён в резервную копию, работаем на defaults.
    RecoveredFromBroken(BrokenConfigRecovery),
    /// Работаем на defaults только в памяти; файл на диске не тронут.
    ///
    /// Причина и запрет записи лежат в [`ConfigSavePolicy::SessionOnly`].
    DefaultsInMemory,
}

/// Подробности восстановления повреждённого config-файла.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenConfigRecovery {
    /// Что именно было не так с файлом.
    pub problem: BrokenConfigProblem,
    /// Имя резервной копии (только имя, без каталога) — его видит пользователь.
    pub backup_file_name: String,
}

/// Почему config-файл признан повреждённым.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokenConfigProblem {
    /// Файл не является корректным TOML (опечатка, лишняя скобка, не UTF-8).
    InvalidTomlSyntax,
    /// TOML корректен, но не совпадает со схемой: неизвестный ключ, не тот тип значения.
    SchemaMismatch,
    /// Значение поля вне допустимого диапазона (в том числе слишком старая версия схемы).
    InvalidValue {
        /// TOML-путь поля, например `audio.volume`.
        field: &'static str,
    },
}

/// Можно ли в этот запуск сохранять изменения настроек в файл.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSavePolicy {
    /// Обычный режим: изменения пишутся в config-файл.
    WriteToFile,
    /// Изменения действуют только до выхода; файл на диске не трогаем.
    SessionOnly(ConfigSessionOnlyReason),
}

/// Почему изменения настроек в этот запуск не записываются на диск.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSessionOnlyReason {
    /// Файл создан более новой версией плеера; перезапись потеряла бы её настройки.
    NewerSchemaVersion {
        /// Версия схемы, найденная в файле.
        found: i64,
    },
    /// Файл существует, но прочитать его нельзя (нет прав, ошибка диска).
    ConfigFileUnreadable {
        /// Безопасный класс I/O ошибки (без пути).
        error_kind: io::ErrorKind,
    },
    /// По пути config-а лежит не обычный файл (например, каталог).
    ConfigPathIsNotFile,
    /// Повреждённый файл не удалось переименовать в резервную копию — он не тронут.
    BackupFailed {
        /// Безопасный класс I/O ошибки (без пути).
        error_kind: io::ErrorKind,
    },
    /// Файл с настройками по умолчанию записать не удалось.
    DefaultConfigNotWritten(ConfigWriteProblem),
}

/// Почему не удалось записать файл с настройками по умолчанию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigWriteProblem {
    /// Не создан каталог config-а.
    CreateDirectory(io::ErrorKind),
    /// Протокол атомарной записи не дошёл до замены файла.
    Write(ConfigWriteFailure),
}

impl ConfigWriteProblem {
    /// Переводит ошибку записи config-слоя в безопасную причину без пути.
    ///
    /// `None` — ошибка не относится к записи файла (ошибка подготовки defaults);
    /// такую вызывающий код не скрывает, а пробрасывает как есть.
    pub(super) fn from_write_error(error: &ConfigError) -> Option<Self> {
        match error {
            ConfigError::CreateConfigDir { source, .. } => {
                Some(Self::CreateDirectory(source.kind()))
            }
            ConfigError::ReplaceConfigFile { failure, .. } => Some(Self::Write(*failure)),
            _ => None,
        }
    }
}

/// Итог одной попытки сохранить настройки через [`ConfigSaveTarget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSaveOutcome {
    /// Настройки записаны в файл.
    Saved,
    /// Настройки проверены, но сознательно не записаны: в этот запуск запись запрещена.
    KeptInMemoryOnly(ConfigSessionOnlyReason),
}

/// Куда и можно ли сохранять настройки в текущий запуск.
///
/// Решение о запрете записи принимает config-хранилище при загрузке; этот тип
/// переносит его до места сохранения, чтобы вызывающий код не мог его обойти.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSaveTarget {
    path: PathBuf,
    policy: ConfigSavePolicy,
}

impl ConfigSaveTarget {
    /// Обычная цель сохранения без ограничений (для тестов и утилит).
    #[must_use]
    pub fn writable(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            policy: ConfigSavePolicy::WriteToFile,
        }
    }

    /// Цель, в которую в этот запуск сохранять нельзя (решение загрузки).
    pub(super) fn session_only(path: PathBuf, reason: ConfigSessionOnlyReason) -> Self {
        Self {
            path,
            policy: ConfigSavePolicy::SessionOnly(reason),
        }
    }

    /// Путь config-файла.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Право записи в этот запуск.
    #[must_use]
    pub fn policy(&self) -> &ConfigSavePolicy {
        &self.policy
    }

    /// Проверяет настройки и записывает их, если запись в этот запуск разрешена.
    ///
    /// Проверка выполняется в обоих режимах: некорректный документ отклоняется
    /// одинаково, независимо от того, пишем мы файл или нет.
    pub fn save(&self, config: &AppConfig) -> ConfigResult<ConfigSaveOutcome> {
        match &self.policy {
            ConfigSavePolicy::WriteToFile => {
                atomic::save_validated(&self.path, config)?;
                Ok(ConfigSaveOutcome::Saved)
            }
            ConfigSavePolicy::SessionOnly(reason) => {
                config
                    .validate()
                    .map_err(|source| ConfigError::ValidateConfigFile {
                        path: self.path.clone(),
                        source: Box::new(source),
                    })?;
                Ok(ConfigSaveOutcome::KeptInMemoryOnly(reason.clone()))
            }
        }
    }
}
