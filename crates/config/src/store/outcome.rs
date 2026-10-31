//! Типизированный итог загрузки config-а и право записи на текущий запуск.
//!
//! Раньше итог загрузки описывался одним `created: bool`, а любая проблема с файлом
//! была фатальной ошибкой. Теперь config-хранилище само решает, что делать с плохим
//! файлом (см. `store/recovery.rs`), и сообщает приложению **что произошло**
//! ([`ConfigLoadOrigin`]) и **можно ли сохранять изменения** ([`ConfigSavePolicy`]).
//! Приложение только показывает это пользователю.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::ConfigWriteFailure;
use crate::{AppConfig, ConfigError, ConfigResult};

use super::atomic;
use super::shared_file::{
    ConfigLockFailure, MissingConfigFileSeed, SharedConfigUpdate, update_shared_config_at,
};

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
    /// Файл повреждён, а процесс браузерного режима его не чинит: переносить файл в `.bak`
    /// и писать defaults поверх — дело обычного плеера при старте (решение 135 моста).
    /// Окно браузерного режима работает на defaults в памяти, файл остаётся как был.
    /// Решает не загрузка файла, а процесс, поэтому значение собирает приложение.
    BrokenConfigLeftToRegularPlayer(BrokenConfigProblem),
    /// Не удалось определить каталог config-а пользователя: писать некуда.
    ConfigDirectoryUnknown,
    /// При старте не удалось взять замок записи config-а (сосед держит его дольше срока
    /// или замок подменён), а файла нет или он битый: создавать файл или переносить
    /// битый в `.bak` без замка небезопасно, поэтому файл не трогаем.
    ConfigLockUnavailable(ConfigLockFailure),
}

impl ConfigSessionOnlyReason {
    /// Снимается ли запрет записи, когда файл на диске снова читается как нормальный config.
    ///
    /// Запрет «только на сеанс» решён при загрузке по состоянию файла в тот момент. Если
    /// потом слежение прочитало файл нормально (`SharedConfigRead::Parsed`), часть причин
    /// уже неправда, и держать запрет до конца запуска — значит терять выбор пользователя:
    /// - битый файл (`BrokenConfigLeftToRegularPlayer`), неудачная копия `.bak`
    ///   (`BackupFailed`), незаписанные defaults (`DefaultConfigNotWritten`) — файл уже
    ///   нормальный, значит его починили;
    /// - замок при старте недоступен (`ConfigLockUnavailable`) — нормальный файл появился,
    ///   а сама запись всё равно идёт под замком и при занятом замке честно отложится;
    /// - файл не читался (`ConfigFileUnreadable`) или по пути был не файл
    ///   (`ConfigPathIsNotFile`) — нормальный разбор доказывает, что теперь там обычный
    ///   читаемый файл.
    ///
    /// Не снимается:
    /// - `NewerSchemaVersion` — файл принадлежит более новой версии плеера; она может
    ///   работать прямо сейчас и снова записать свою схему, а наша запись потеряла бы её
    ///   настройки (решение UX-05: такой файл в этот запуск не трогаем);
    /// - `ConfigDirectoryUnknown` — каталога config-а нет, следить и писать некуда.
    #[must_use]
    pub fn lifts_when_file_becomes_usable(&self) -> bool {
        match self {
            Self::BrokenConfigLeftToRegularPlayer(_)
            | Self::BackupFailed { .. }
            | Self::DefaultConfigNotWritten(_)
            | Self::ConfigLockUnavailable(_)
            | Self::ConfigFileUnreadable { .. }
            | Self::ConfigPathIsNotFile => true,
            Self::NewerSchemaVersion { .. } | Self::ConfigDirectoryUnknown => false,
        }
    }
}

/// Итог [`ConfigSaveTarget::resume_writing_after_usable_file`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWriteResumption {
    /// Запись и так была разрешена — ничего не изменилось.
    AlreadyWritable,
    /// Запрет «только на сеанс» снят: дальше изменения пишутся в файл.
    Resumed {
        /// Причина запрета, которая перестала действовать.
        lifted_reason: ConfigSessionOnlyReason,
    },
    /// Причина запрета от состояния файла не зависит (например, файл новой версии) —
    /// запрет остаётся до конца запуска.
    StillSessionOnly(ConfigSessionOnlyReason),
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

    /// Файл на диске только что прочитан как нормальный config — снять запрет записи,
    /// если его причина от этого перестала быть правдой.
    ///
    /// Звать только после успешного разбора файла (`SharedConfigRead::Parsed`): метод сам
    /// диск не читает. Какие причины снимаются — решает
    /// [`ConfigSessionOnlyReason::lifts_when_file_becomes_usable`].
    pub fn resume_writing_after_usable_file(&mut self) -> ConfigWriteResumption {
        let ConfigSavePolicy::SessionOnly(reason) = &self.policy else {
            return ConfigWriteResumption::AlreadyWritable;
        };
        if !reason.lifts_when_file_becomes_usable() {
            return ConfigWriteResumption::StillSessionOnly(reason.clone());
        }
        let lifted_reason = reason.clone();
        self.policy = ConfigSavePolicy::WriteToFile;
        ConfigWriteResumption::Resumed { lifted_reason }
    }

    /// Проверяет настройки и записывает документ **целиком**, если запись разрешена.
    ///
    /// Без замка и без перечитывания: годится только там, где у файла один писатель
    /// (утилиты, тесты). Настройки приложения сохраняются через
    /// [`ConfigSaveTarget::update_shared_file`] — иначе копии плеера затрут друг друга.
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

    /// Вписывает изменения в общий config-файл под замком, если запись разрешена.
    ///
    /// Запись разрешена — [`update_shared_config_at`]: перечитать файл, применить `merge`
    /// к документу с диска, проверить и записать (`missing_file` — из чего создать файл,
    /// если его нет). Запрещена в этот запуск — файл не читается и не пишется, `merge` не
    /// вызывается; проверку документа в памяти делает вызывающий код, у которого этот
    /// документ есть.
    pub fn update_shared_file<E>(
        &self,
        lock_wait: Duration,
        missing_file: MissingConfigFileSeed<'_>,
        merge: impl FnOnce(&mut AppConfig) -> Result<(), E>,
    ) -> SharedConfigSave<E> {
        match &self.policy {
            ConfigSavePolicy::WriteToFile => SharedConfigSave::Attempted(update_shared_config_at(
                &self.path,
                lock_wait,
                missing_file,
                merge,
            )),
            ConfigSavePolicy::SessionOnly(reason) => {
                SharedConfigSave::KeptInMemoryOnly(reason.clone())
            }
        }
    }
}

/// Итог [`ConfigSaveTarget::update_shared_file`]: была ли попытка записи и чем кончилась.
#[derive(Debug)]
#[must_use = "the outcome tells whether the change reached the settings file"]
pub enum SharedConfigSave<E> {
    /// Запись разрешена: итог перечитывания, слияния и записи под замком.
    Attempted(SharedConfigUpdate<E>),
    /// В этот запуск запись запрещена: файл не читался и не писался.
    KeptInMemoryOnly(ConfigSessionOnlyReason),
}
