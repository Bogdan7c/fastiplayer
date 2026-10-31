//! Сохранение настроек в общий config «только тронутые ключи» (решение владельца 135).
//!
//! Config делят обычное окно и окна браузерного режима. Поэтому сохранение не пишет
//! документ транзакции целиком: под замком записи (`fastiplayer-config`) файл
//! перечитывается с диска, и в прочитанный документ вписываются **только** изменённые
//! этой транзакцией настройки (`SettingsDiff`, значение `after` по id реестра). Всё
//! остальное — в том числе ключи, которые сосед записал минуту назад, — остаётся как на
//! диске.
//!
//! Поля `AppConfig`, которых нет в реестре настроек (их нельзя назвать по id), при
//! слиянии всегда берутся с диска: транзакция настроек не может их изменить, значит и
//! записывать их из памяти незачем. Тест `every_config_leaf_is_reachable_by_setting_id`
//! в `fastiplayer-config` следит, чтобы таких полей не было.
//!
//! Ошибки (замок занят или подменён, файл на диске битый или от новой версии, запись не
//! удалась) возвращаются как `SettingsError` с английским текстом для пользователя.
//! Контроллер настроек при ошибке сохранения откатывает уже применённое к рантайму, а
//! черновик остаётся — изменение не теряется молча и его можно применить снова.

use std::io;
use std::time::Duration;

use fastiplayer_config::{
    AppConfig, BrokenConfigProblem, ConfigError, ConfigSaveTarget, ConfigWriteProblem,
    SharedConfigLockProblem, SharedConfigLockStage, SharedConfigProblem, SharedConfigSave,
    SharedConfigUpdate,
};
use settings_core::{
    PersistReport, PersistRequest, SettingsDiff, SettingsError, SettingsRegistry, SettingsResult,
};

use crate::app_config_registry;

/// Сколько ждать замок записи config-а при сохранении настроек — 2 секунды.
///
/// Соседняя копия плеера держит замок миллисекунды; 2 секунды — это «сосед завис».
/// Сохранение идёт из окна настроек на UI-потоке, поэтому ждать дольше нельзя: лучше
/// быстро показать понятную ошибку «файл занят» и сохранить черновик для повтора.
pub const SETTINGS_FILE_LOCK_WAIT: Duration = Duration::from_secs(2);

/// Сохраняет изменения транзакции в общий config или подтверждает работу без записи.
pub(crate) fn persist_changed_settings(
    save_target: &ConfigSaveTarget,
    lock_wait: Duration,
    request: PersistRequest<'_, AppConfig>,
) -> SettingsResult<PersistReport> {
    let registry = app_config_registry()?;
    let save = save_target.update_shared_file(lock_wait, |disk_config| {
        overlay_changed_settings(&registry, disk_config, request.changed_settings)
    });
    match save {
        SharedConfigSave::Attempted(update) => persist_report_from_update(update),
        // Запись в этот запуск запрещена решением загрузки: документ всё равно
        // проверяется, а изменения живут до выхода без отката транзакции.
        SharedConfigSave::KeptInMemoryOnly(_reason) => {
            request
                .document
                .validate()
                .map_err(|error| SettingsError::access_failed(error.to_string()))?;
            Ok(PersistReport::kept_for_session_only())
        }
    }
}

/// Вписывает в документ с диска только изменённые настройки (значение `after`).
fn overlay_changed_settings(
    registry: &SettingsRegistry<AppConfig>,
    disk_config: &mut AppConfig,
    changed_settings: &SettingsDiff,
) -> SettingsResult<()> {
    for change in changed_settings.changes() {
        registry.set_value(disk_config, &change.id, change.after.clone())?;
    }
    Ok(())
}

/// Переводит итог записи в отчёт транзакции или понятную пользователю ошибку.
fn persist_report_from_update(
    update: SharedConfigUpdate<SettingsError>,
) -> SettingsResult<PersistReport> {
    let failure_reason = match update {
        SharedConfigUpdate::Written { .. } => return Ok(PersistReport::persisted()),
        SharedConfigUpdate::WrittenDurabilityUnconfirmed { sync_error, .. } => {
            return Ok(PersistReport::persisted().with_durability_warning(format!(
                "the settings folder did not confirm the write ({sync_error:?})"
            )));
        }
        // Ошибка реестра уже типизирована (например, значение вне диапазона) — как есть.
        SharedConfigUpdate::MergeRejected(registry_error) => return Err(registry_error),
        SharedConfigUpdate::LockBusy { waited } => format!(
            "another Fastiplayer window kept the settings file busy for {:.1} s; try Apply again",
            waited.as_secs_f64()
        ),
        SharedConfigUpdate::LockUnavailable(problem) => format!(
            "the settings lock file next to config.toml cannot be used ({})",
            lock_problem_text(problem)
        ),
        SharedConfigUpdate::DiskConfigUnusable(problem) => format!(
            "{}; the file was left unchanged",
            disk_problem_text(&problem)
        ),
        SharedConfigUpdate::DiskConfigNewer { found } => format!(
            "the settings file was created by a newer version of Fastiplayer (schema {found}) and was left unchanged"
        ),
        SharedConfigUpdate::MergedConfigRejected(error) => format!(
            "together with changes from another Fastiplayer window the settings are invalid ({})",
            merged_config_error_text(&error)
        ),
        SharedConfigUpdate::WriteFailed(problem) => format!(
            "could not write the settings file ({})",
            write_problem_text(problem)
        ),
    };
    Err(SettingsError::access_failed(format!(
        "Settings were not saved: {failure_reason}."
    )))
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
            "the settings file on disk has a syntax error".to_string()
        }
        SharedConfigProblem::Broken(BrokenConfigProblem::SchemaMismatch) => {
            "the settings file on disk contains an unknown or invalid setting".to_string()
        }
        SharedConfigProblem::Broken(BrokenConfigProblem::InvalidValue { field }) => {
            format!("the settings file on disk has an invalid value of {field}")
        }
        SharedConfigProblem::NotRegularFile => {
            "the settings file path does not point to a regular file".to_string()
        }
        SharedConfigProblem::ReadFailed { error_kind } => format!(
            "could not read the settings file ({})",
            io::Error::from(*error_kind)
        ),
        SharedConfigProblem::UnclassifiedParseFailure => {
            "the settings file on disk could not be checked".to_string()
        }
    }
}

/// Ошибка проверки слитого документа без пути к файлу.
fn merged_config_error_text(error: &ConfigError) -> String {
    match error {
        ConfigError::ValidateConfigFile { source, .. } => source.to_string(),
        other => other.to_string(),
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

#[cfg(test)]
mod tests;
