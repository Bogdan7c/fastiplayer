//! Почему изменения не записаны в файл сейчас и как это видит пользователь.
//!
//! Решение владельца 135: если общий config нельзя записать (занят соседом, битый, от
//! новой версии, замок недоступен, запись не удалась), изменение **живёт в памяти**, а
//! файл на диске не портится. Поэтому эти случаи — не ошибка транзакции (без отката), а
//! «сохраним позже» с понятной причиной. Модуль переводит итог записи из
//! `fastiplayer-config` в такую причину и даёт приложению типизированное прочтение
//! отчёта сохранения ([`settings_file_persist_outcome`]).

use std::io;
use std::time::Duration;

use fastiplayer_config::{
    AppConfig, BrokenConfigProblem, ConfigError, ConfigWriteProblem, SharedConfigLockProblem,
    SharedConfigLockStage, SharedConfigProblem, SharedConfigUpdate,
};
use settings_core::{PersistOutcome, PersistReport, SettingId, SettingsError, SettingsRegistry};

/// Хвост каждого сообщения «не сохранено»: что будет с изменениями дальше.
const CHANGES_STAY_IN_EFFECT: &str = "Your changes are in effect until you close Fastiplayer and will be saved with the next change.";

/// Почему изменения настроек пока не записаны в файл (файл на диске не тронут).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsFileSaveDeferral {
    /// Замок записи держит другая копия Fastiplayer дольше срока ожидания.
    FileBusy {
        /// Сколько ждали замок.
        waited: Duration,
    },
    /// Файл-замок рядом с config нельзя использовать (подменён, чужой, ошибка I/O).
    LockUnavailable(SharedConfigLockProblem),
    /// Файл на диске нельзя взять за основу (битый, не файл, не читается).
    FileUnusable(SharedConfigProblem),
    /// Файл записан более новой версией Fastiplayer.
    FileFromNewerVersion {
        /// Версия схемы в файле.
        found: i64,
    },
    /// Наши изменения вместе с тем, что уже записала другая копия, не проходят проверку.
    ConflictsWithSavedSettings {
        /// Понятное название настройки, на которой сломалась проверка (если известно).
        setting_label: Option<String>,
    },
    /// Запись файла не удалась.
    WriteFailed(ConfigWriteProblem),
}

impl SettingsFileSaveDeferral {
    /// Английское сообщение для пользователя: причина и судьба изменений.
    #[must_use]
    pub fn user_message(&self) -> String {
        format!("{} {CHANGES_STAY_IN_EFFECT}", self.cause_text())
    }

    /// Причина простыми словами, без путей и технических имён.
    fn cause_text(&self) -> String {
        match self {
            Self::FileBusy { .. } => {
                "The settings file is busy: another Fastiplayer window is saving it.".to_string()
            }
            Self::LockUnavailable(problem) => format!(
                "The settings lock file next to config.toml cannot be used ({}).",
                lock_problem_text(*problem)
            ),
            Self::FileUnusable(problem) => format!("{}.", disk_problem_text(problem)),
            Self::FileFromNewerVersion { .. } => {
                "The settings file was saved by a newer version of Fastiplayer and was left unchanged."
                    .to_string()
            }
            Self::ConflictsWithSavedSettings {
                setting_label: Some(label),
            } => format!(
                "Your changes conflict with settings saved by another Fastiplayer window (“{label}”)."
            ),
            Self::ConflictsWithSavedSettings {
                setting_label: None,
            } => "Your changes conflict with settings saved by another Fastiplayer window."
                .to_string(),
            Self::WriteFailed(problem) => format!(
                "Could not write the settings file ({}).",
                write_problem_text(*problem)
            ),
        }
    }
}

/// Итог одной попытки записи с точки зрения транзакции настроек.
pub(super) enum SaveAttempt {
    /// Записано; `durability_warning` — если каталог не подтвердил запись.
    Saved { durability_warning: Option<String> },
    /// Не записано, файл не тронут, изменение живёт в памяти.
    Deferred(SettingsFileSaveDeferral),
    /// Слияние отказалось (ошибка реестра) — настоящая ошибка транзакции.
    Rejected(SettingsError),
}

/// Сводит итог записи `fastiplayer-config` к [`SaveAttempt`].
pub(super) fn classify_update(
    registry: &SettingsRegistry<AppConfig>,
    update: SharedConfigUpdate<SettingsError>,
) -> SaveAttempt {
    let deferral = match update {
        SharedConfigUpdate::Written { .. } => {
            return SaveAttempt::Saved {
                durability_warning: None,
            };
        }
        SharedConfigUpdate::WrittenDurabilityUnconfirmed { sync_error, .. } => {
            return SaveAttempt::Saved {
                durability_warning: Some(format!(
                    "the settings folder did not confirm the write ({sync_error:?})"
                )),
            };
        }
        SharedConfigUpdate::MergeRejected(registry_error) => {
            return SaveAttempt::Rejected(registry_error);
        }
        SharedConfigUpdate::LockBusy { waited } => SettingsFileSaveDeferral::FileBusy { waited },
        SharedConfigUpdate::LockUnavailable(problem) => {
            SettingsFileSaveDeferral::LockUnavailable(problem)
        }
        SharedConfigUpdate::DiskConfigUnusable(problem) => {
            SettingsFileSaveDeferral::FileUnusable(problem)
        }
        SharedConfigUpdate::DiskConfigNewer { found } => {
            SettingsFileSaveDeferral::FileFromNewerVersion { found }
        }
        SharedConfigUpdate::MergedConfigRejected(error) => {
            SettingsFileSaveDeferral::ConflictsWithSavedSettings {
                setting_label: invalid_setting_label(registry, &error),
            }
        }
        SharedConfigUpdate::WriteFailed(problem) => SettingsFileSaveDeferral::WriteFailed(problem),
    };
    SaveAttempt::Deferred(deferral)
}

/// Отчёт транзакции для отложенной записи.
///
/// Контракт `AppConfigStore` (новых вариантов в `settings-core` не заводим): «отложено» —
/// это `PersistOutcome::SkippedSessionOnly` (изменения применены только в памяти, без
/// отката) **с** текстом причины в `durability_warning`. Режим «запись запрещена на весь
/// запуск» — тот же исход **без** текста. Различать их следует только через
/// [`settings_file_persist_outcome`], а не по полям отчёта.
pub(super) fn deferred_report(deferral: &SettingsFileSaveDeferral) -> PersistReport {
    PersistReport::kept_for_session_only().with_durability_warning(deferral.user_message())
}

/// Как сохранение настроек обошлось с файлом — типизированное прочтение `PersistReport`
/// от `AppConfigStore`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFilePersistOutcome<'report> {
    /// Записано и подтверждено.
    Saved,
    /// Записано, но каталог не подтвердил надёжность записи.
    SavedDurabilityUnconfirmed {
        /// Технический текст для журнала.
        warning: &'report str,
    },
    /// Не записано сейчас (файл занят, битый, новее, ошибка записи): изменения действуют
    /// в памяти и будут дописаны при следующем изменении.
    NotSavedYet {
        /// Английское сообщение для пользователя (причина и что будет дальше).
        notice: &'report str,
    },
    /// Запись запрещена решением загрузки на весь запуск (например, файл новой версии).
    SessionOnly,
    /// Изменений не было — записывать нечего.
    NothingToSave,
}

/// Читает отчёт сохранения `AppConfigStore` (см. [`deferred_report`]).
#[must_use]
pub fn settings_file_persist_outcome(report: &PersistReport) -> SettingsFilePersistOutcome<'_> {
    match (report.outcome, report.durability_warning.as_deref()) {
        (PersistOutcome::Persisted, None) => SettingsFilePersistOutcome::Saved,
        (PersistOutcome::Persisted, Some(warning)) => {
            SettingsFilePersistOutcome::SavedDurabilityUnconfirmed { warning }
        }
        (PersistOutcome::SkippedSessionOnly, Some(notice)) => {
            SettingsFilePersistOutcome::NotSavedYet { notice }
        }
        (PersistOutcome::SkippedSessionOnly, None) => SettingsFilePersistOutcome::SessionOnly,
        (PersistOutcome::SkippedNoChanges, _) => SettingsFilePersistOutcome::NothingToSave,
    }
}

/// Название настройки, на которой слитый документ не прошёл проверку.
fn invalid_setting_label(
    registry: &SettingsRegistry<AppConfig>,
    error: &ConfigError,
) -> Option<String> {
    let field = match error {
        ConfigError::ValidateConfigFile { source, .. } => match source.as_ref() {
            ConfigError::InvalidValue { field, .. } => *field,
            _ => return None,
        },
        ConfigError::InvalidValue { field, .. } => *field,
        _ => return None,
    };
    registry
        .descriptor(&SettingId::from(field))
        .map(|descriptor| descriptor.text.label.fallback_ru.clone())
}

/// Проблема файла-замка простыми словами.
fn lock_problem_text(problem: SharedConfigLockProblem) -> String {
    match problem {
        SharedConfigLockProblem::NotRegularFile => "it is not a regular file".to_string(),
        SharedConfigLockProblem::OwnerMismatch => "it belongs to another user".to_string(),
        SharedConfigLockProblem::IdentityChanged => "it was replaced while saving".to_string(),
        SharedConfigLockProblem::Io { stage, error_kind } => format!(
            "{}: {}",
            lock_stage_text(stage),
            io::Error::from(error_kind)
        ),
    }
}

/// Шаг взятия замка простыми словами.
fn lock_stage_text(stage: SharedConfigLockStage) -> &'static str {
    match stage {
        SharedConfigLockStage::CreateDirectory => "could not create the settings folder",
        SharedConfigLockStage::InspectPath | SharedConfigLockStage::InspectDescriptor => {
            "could not check it"
        }
        SharedConfigLockStage::Open => "could not open it",
        SharedConfigLockStage::RestrictPermissions => "could not make it private",
        SharedConfigLockStage::Acquire => "could not lock it",
        SharedConfigLockStage::RevalidatePath => "could not re-check it after locking",
    }
}

/// Почему файл на диске нельзя взять за основу — простыми словами.
fn disk_problem_text(problem: &SharedConfigProblem) -> String {
    match problem {
        SharedConfigProblem::Broken(BrokenConfigProblem::InvalidTomlSyntax) => {
            "The settings file is corrupted (syntax error) and was left unchanged".to_string()
        }
        SharedConfigProblem::Broken(BrokenConfigProblem::SchemaMismatch) => {
            "The settings file is corrupted (unknown or invalid setting) and was left unchanged"
                .to_string()
        }
        // Путь поля (`ui.sidebar…`) — технический id, в текст пользователя он не идёт.
        SharedConfigProblem::Broken(BrokenConfigProblem::InvalidValue { .. }) => {
            "The settings file is corrupted (a setting has an invalid value) and was left unchanged"
                .to_string()
        }
        SharedConfigProblem::NotRegularFile => {
            "The settings file path does not point to a regular file".to_string()
        }
        SharedConfigProblem::ReadFailed { error_kind } => format!(
            "Could not read the settings file ({})",
            io::Error::from(*error_kind)
        ),
        SharedConfigProblem::UnclassifiedParseFailure => {
            "The settings file could not be checked and was left unchanged".to_string()
        }
    }
}

/// Причина неудачной записи простыми словами.
fn write_problem_text(problem: ConfigWriteProblem) -> String {
    match problem {
        ConfigWriteProblem::CreateDirectory(error_kind) => format!(
            "could not create the settings folder: {}",
            io::Error::from(error_kind)
        ),
        ConfigWriteProblem::Write(failure) => failure.to_string(),
    }
}
