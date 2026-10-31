//! Сохранение настроек в общий config «только тронутые ключи» (решение владельца 135).
//!
//! Config делят обычное окно и окна браузерного режима. Поэтому сохранение не пишет
//! документ транзакции целиком: под замком записи (`fastiplayer-config`) файл
//! перечитывается с диска, и в прочитанный документ вписываются **только** изменённые
//! этой транзакцией настройки (`SettingsDiff`, значение `after` по id реестра) и ранее
//! несохранённые (`unsaved.rs`). Всё остальное — в том числе ключи, которые сосед записал
//! минуту назад, — остаётся как на диске.
//!
//! Поля `AppConfig`, которых нет в реестре настроек (их нельзя назвать по id), при
//! слиянии всегда берутся с диска: транзакция настроек не может их изменить, значит и
//! записывать их из памяти незачем. Тест `every_config_leaf_is_reachable_by_setting_id`
//! в `fastiplayer-config` следит, чтобы таких полей не было.
//!
//! Неудачная запись (замок занят или подменён, файл на диске битый или от новой версии,
//! конфликт со значениями соседа, ошибка записи) — **не** ошибка транзакции: изменения
//! уже применены и остаются в памяти, файл не тронут, id запоминаются и дописываются при
//! следующей успешной записи (`deferral.rs`, `unsaved.rs`). Ошибкой (с откатом) остаётся
//! только отказ реестра вписать значение.

use std::time::Duration;

use fastiplayer_config::{AppConfig, ConfigSaveTarget, SharedConfigSave};
use settings_core::{
    PersistReport, PersistRequest, SettingId, SettingsDiff, SettingsError, SettingsResult,
};

use crate::app_config_registry;

mod deferral;
mod unsaved;

use deferral::{SaveAttempt, classify_update, deferred_report};
pub use deferral::{
    SettingsFilePersistOutcome, SettingsFileSaveDeferral, settings_file_persist_outcome,
};
pub(crate) use unsaved::UnsavedSettingChanges;
use unsaved::overlay_changes;

/// Сколько ждать замок записи config-а при сохранении настроек — 2 секунды.
///
/// Соседняя копия плеера держит замок миллисекунды; 2 секунды — это «сосед завис».
/// Сохранение идёт из окна настроек на UI-потоке, поэтому ждать дольше нельзя: лучше
/// быстро сказать «файл занят» и дописать изменение при следующей записи.
pub const SETTINGS_FILE_LOCK_WAIT: Duration = Duration::from_secs(2);

/// Подтвердил ли каталог надёжность записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFileDurability {
    /// Запись подтверждена.
    Confirmed,
    /// Файл заменён и читается, но sync каталога не подтверждён.
    Unconfirmed,
}

/// Итог [`crate::AppConfigStore::save_unsaved_changes`].
#[derive(Debug, Clone, PartialEq)]
pub enum UnsavedChangesSave {
    /// Несохранённых изменений нет (или запись запрещена на весь запуск) — файл не трогали.
    NothingToSave,
    /// Несохранённые изменения разобраны под замком, файл записан.
    Saved {
        /// Дописаны значениями из памяти.
        written: Vec<SettingId>,
        /// На диске уже было то же значение, что в памяти.
        already_on_disk: Vec<SettingId>,
        /// Ключ после нашей неудачи записала другая копия — оставлено её значение.
        kept_from_disk: Vec<SettingId>,
        /// Надёжность записи.
        durability: SettingsFileDurability,
    },
    /// Снова не записано; изменения по-прежнему действуют в памяти и ждут следующей записи.
    StillUnsaved(SettingsFileSaveDeferral),
    /// Реестр отказался вписать значение (ошибка программы); несохранённое не забыто.
    Rejected(SettingsError),
}

/// Сохраняет изменения транзакции в общий config или подтверждает работу без записи.
pub(crate) fn persist_changed_settings(
    save_target: &ConfigSaveTarget,
    lock_wait: Duration,
    unsaved: &mut UnsavedSettingChanges,
    request: PersistRequest<'_, AppConfig>,
) -> SettingsResult<PersistReport> {
    let registry = app_config_registry()?;
    let save = save_target.update_shared_file(lock_wait, |disk_config| {
        // Как разрешились несохранённые id, транзакции знать не нужно: после успешной
        // записи все они записаны или уступили более позднему значению соседа.
        overlay_changes(
            &registry,
            disk_config,
            request.changed_settings,
            unsaved,
            request.document,
        )
        .map(|_overlay| ())
    });
    let update = match save {
        SharedConfigSave::Attempted(update) => update,
        // Запись в этот запуск запрещена решением загрузки: документ всё равно
        // проверяется, а изменения живут до выхода без отката транзакции.
        SharedConfigSave::KeptInMemoryOnly(_reason) => {
            request
                .document
                .validate()
                .map_err(|error| SettingsError::access_failed(error.to_string()))?;
            return Ok(PersistReport::kept_for_session_only());
        }
    };
    match classify_update(&registry, update) {
        SaveAttempt::Saved { durability_warning } => {
            unsaved.forget_all();
            let report = PersistReport::persisted();
            Ok(match durability_warning {
                Some(warning) => report.with_durability_warning(warning),
                None => report,
            })
        }
        SaveAttempt::Deferred(deferral) => {
            // Изменения уже применены и остаются в памяти; файл не тронут.
            unsaved.remember_failed_write(request.changed_settings);
            Ok(deferred_report(&deferral))
        }
        SaveAttempt::Rejected(registry_error) => Err(registry_error),
    }
}

/// Дописывает только несохранённые изменения, значения — из `current` (обычно committed).
pub(crate) fn save_unsaved_settings(
    save_target: &ConfigSaveTarget,
    lock_wait: Duration,
    unsaved: &mut UnsavedSettingChanges,
    current: &AppConfig,
) -> UnsavedChangesSave {
    if unsaved.is_empty() {
        return UnsavedChangesSave::NothingToSave;
    }
    let registry = match app_config_registry() {
        Ok(registry) => registry,
        Err(registry_error) => return UnsavedChangesSave::Rejected(registry_error),
    };
    let no_new_changes = SettingsDiff::new();
    let mut overlay_result = None;
    let save = save_target.update_shared_file(lock_wait, |disk_config| {
        overlay_result = Some(overlay_changes(
            &registry,
            disk_config,
            &no_new_changes,
            unsaved,
            current,
        )?);
        Ok(())
    });
    let update = match save {
        SharedConfigSave::Attempted(update) => update,
        SharedConfigSave::KeptInMemoryOnly(_reason) => return UnsavedChangesSave::NothingToSave,
    };
    match classify_update(&registry, update) {
        SaveAttempt::Saved { durability_warning } => {
            unsaved.forget_all();
            let overlay = overlay_result.unwrap_or_default();
            UnsavedChangesSave::Saved {
                written: overlay.written,
                already_on_disk: overlay.already_on_disk,
                kept_from_disk: overlay.kept_from_disk,
                durability: if durability_warning.is_some() {
                    SettingsFileDurability::Unconfirmed
                } else {
                    SettingsFileDurability::Confirmed
                },
            }
        }
        SaveAttempt::Deferred(deferral) => UnsavedChangesSave::StillUnsaved(deferral),
        SaveAttempt::Rejected(registry_error) => UnsavedChangesSave::Rejected(registry_error),
    }
}

#[cfg(test)]
mod tests;
