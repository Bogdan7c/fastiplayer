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
//! конфликт значений со значениями соседа, ошибка записи) — **не** ошибка транзакции:
//! изменения уже применены и остаются в памяти, файл не тронут, id запоминаются и
//! дописываются при следующей успешной записи (`deferral.rs`, `unsaved.rs`). Так же
//! запоминаются изменения, сделанные в восстановимом режиме «только на сеанс» (битый файл,
//! недоступный замок при старте…): когда «общая доска» прочитает нормальный файл, запрет
//! снимается (`note_disk_config_seen`) и они дописываются. Ошибкой (с откатом) остаются
//! отказ реестра вписать значение и сбой сериализации слитого документа (ошибки программы).
//!
//! Если файла на момент записи нет (удалили во время работы), он создаётся из документа
//! этой копии с наложенными изменениями, а не из defaults: иначе после перезапуска
//! пропали бы все остальные настройки пользователя
//! (`MissingConfigFileSeed::WriterDocument`).

use std::time::Duration;

use fastiplayer_config::{
    AppConfig, ConfigSaveTarget, ConfigWriteResumption, MissingConfigFileSeed, SharedConfigSave,
};
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
pub(crate) use unsaved::SharedFileKnowledge;
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
    /// Несохранённых изменений нет или запись сейчас запрещена режимом «только на сеанс»
    /// (снимается [`crate::AppConfigStore::note_disk_config_seen`]) — файл не трогали.
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
    /// Реестр отказался вписать значение или слитый документ не сериализуется (ошибки
    /// программы); несохранённое не забыто.
    Rejected(SettingsError),
}

/// Сохраняет изменения транзакции в общий config или подтверждает работу без записи.
pub(crate) fn persist_changed_settings(
    save_target: &ConfigSaveTarget,
    lock_wait: Duration,
    knowledge: &mut SharedFileKnowledge,
    request: PersistRequest<'_, AppConfig>,
) -> SettingsResult<PersistReport> {
    let registry = app_config_registry()?;
    let save = save_target.update_shared_file(
        lock_wait,
        // Файл удалили во время работы — создаём его из своего документа, а не из defaults.
        MissingConfigFileSeed::WriterDocument(request.document),
        |disk_config| {
            // Как разрешились несохранённые id, транзакции знать не нужно: после успешной
            // записи все они записаны или уступили более позднему значению соседа.
            overlay_changes(
                &registry,
                disk_config,
                request.changed_settings,
                knowledge,
                request.document,
            )
            .map(|_overlay| ())
        },
    );
    let update = match save {
        SharedConfigSave::Attempted(update) => update,
        // Запись в этот запуск запрещена решением загрузки: документ всё равно
        // проверяется, а изменения живут без отката транзакции.
        SharedConfigSave::KeptInMemoryOnly(reason) => {
            request
                .document
                .validate()
                .map_err(|error| SettingsError::access_failed(error.to_string()))?;
            // Запрет снимется, когда файл снова станет нормальным: тогда эти изменения
            // нужно дописать, поэтому id запоминаются как несохранённые. Запрет навсегда
            // (файл новой версии) — запоминать незачем, записи в этот запуск не будет.
            if reason.lifts_when_file_becomes_usable() {
                knowledge.remember_unwritten(&registry, request.changed_settings)?;
            }
            return Ok(PersistReport::kept_for_session_only());
        }
    };
    match classify_update(&registry, update) {
        SaveAttempt::Saved {
            written,
            durability_warning,
        } => {
            knowledge.note_written(written);
            let report = PersistReport::persisted();
            Ok(match durability_warning {
                Some(warning) => report.with_durability_warning(warning),
                None => report,
            })
        }
        SaveAttempt::Deferred(deferral) => {
            // Изменения уже применены и остаются в памяти; файл не тронут.
            knowledge.remember_unwritten(&registry, request.changed_settings)?;
            Ok(deferred_report(&deferral))
        }
        SaveAttempt::Rejected(program_error) => Err(program_error),
    }
}

/// Дописывает только несохранённые изменения, значения — из `current` (обычно committed).
pub(crate) fn save_unsaved_settings(
    save_target: &ConfigSaveTarget,
    lock_wait: Duration,
    knowledge: &mut SharedFileKnowledge,
    current: &AppConfig,
) -> UnsavedChangesSave {
    if !knowledge.has_unsaved() {
        return UnsavedChangesSave::NothingToSave;
    }
    let registry = match app_config_registry() {
        Ok(registry) => registry,
        Err(registry_error) => return UnsavedChangesSave::Rejected(registry_error),
    };
    let no_new_changes = SettingsDiff::new();
    let mut overlay_result = None;
    let save = save_target.update_shared_file(
        lock_wait,
        MissingConfigFileSeed::WriterDocument(current),
        |disk_config| {
            overlay_result = Some(overlay_changes(
                &registry,
                disk_config,
                &no_new_changes,
                knowledge,
                current,
            )?);
            Ok(())
        },
    );
    let update = match save {
        SharedConfigSave::Attempted(update) => update,
        SharedConfigSave::KeptInMemoryOnly(_reason) => return UnsavedChangesSave::NothingToSave,
    };
    match classify_update(&registry, update) {
        SaveAttempt::Saved {
            written,
            durability_warning,
        } => {
            knowledge.note_written(written);
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
        SaveAttempt::Rejected(program_error) => UnsavedChangesSave::Rejected(program_error),
    }
}

/// Копия прочитала нормальный документ диска: запомнить его как «виденный» и снять
/// восстановимый запрет записи «только на сеанс».
pub(crate) fn note_disk_config_seen(
    save_target: &mut ConfigSaveTarget,
    knowledge: &mut SharedFileKnowledge,
    disk_document: &AppConfig,
) -> SettingsResult<ConfigWriteResumption> {
    let registry = app_config_registry()?;
    knowledge.note_disk_config_seen(&registry, disk_document)?;
    Ok(save_target.resume_writing_after_usable_file())
}

#[cfg(test)]
mod tests;
