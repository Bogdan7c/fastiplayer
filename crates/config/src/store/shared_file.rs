//! Config как «общая доска» для нескольких копий плеера (решение владельца 135).
//!
//! Обычное окно и окна браузерного режима пишут один `config.toml`. Чтобы они не затирали
//! друг друга, запись идёт так: взять замок записи (`config.toml.lock`, миллисекунды) →
//! перечитать файл с диска тем же разбором, что при загрузке (с миграциями) → вписать в
//! прочитанный документ **только свои тронутые ключи** (решает функция слияния вызывающего
//! кода) → проверить → атомарно записать → отпустить замок.
//!
//! Инварианты:
//! - чужие ключи берутся с диска, поэтому изменение соседа не теряется;
//! - битый файл, файл от новой версии, занятый или подменённый замок — понятная ошибка,
//!   а файл на диске остаётся байт-в-байт прежним (никаких `.bak`, никаких defaults
//!   поверх: это решения восстановления при старте, а не записи);
//! - чтение для слежения ([`read_shared_config_at`]) идёт без замка: атомарный rename
//!   гарантирует, что читатель видит либо старый, либо новый файл целиком.

use std::path::Path;
use std::time::Duration;

use crate::{AppConfig, ConfigWriteProblem, config_write_lock_path_for};

use super::atomic::{self, ConfigWriteDurability};
use super::disk_read::{DiskConfigState, read_disk_config};

mod lock;
mod outcome;

#[cfg(test)]
pub(super) use lock::ConfigWriteGuard;
pub(super) use lock::acquire_config_write_lock;
pub use outcome::{
    ConfigLockFailure, SharedConfigLockProblem, SharedConfigLockStage, SharedConfigProblem,
    SharedConfigRead, SharedConfigUpdate,
};

/// Срок ожидания замка записи по умолчанию — 2 секунды.
///
/// Сосед держит замок миллисекунды, поэтому 2 секунды — это «сосед завис или очень
/// медленный диск», а не обычная очередь. Дольше ждать нельзя: запись идёт из окна
/// настроек, и пользователь должен быстро увидеть понятную ошибку, а не подвисание.
pub const DEFAULT_SHARED_CONFIG_LOCK_WAIT: Duration = Duration::from_secs(2);

/// Перечитывает config под замком, вписывает изменения через `merge` и записывает.
///
/// - Файла нет → основа — `AppConfig::default()`, файл создаётся.
/// - Файл битый, не файл, не читается или от новой версии → ошибка без записи.
/// - `merge` получает документ с диска и меняет в нём только свои ключи; ошибка `merge`
///   возвращается как [`SharedConfigUpdate::MergeRejected`] без записи.
/// - Документ после слияния проверяется целиком (`AppConfig::validate` + контроль
///   сериализации) и только потом атомарно записывается.
pub fn update_shared_config_at<E>(
    path: impl AsRef<Path>,
    lock_wait: Duration,
    merge: impl FnOnce(&mut AppConfig) -> Result<(), E>,
) -> SharedConfigUpdate<E> {
    let path = path.as_ref();
    let lock_path = config_write_lock_path_for(path);
    // Замок живёт до конца функции: перечитывание, слияние и запись — под ним.
    let _write_guard = match acquire_config_write_lock(&lock_path, lock_wait) {
        Ok(write_guard) => write_guard,
        Err(ConfigLockFailure::Busy { waited }) => return SharedConfigUpdate::LockBusy { waited },
        Err(ConfigLockFailure::Unavailable(problem)) => {
            return SharedConfigUpdate::LockUnavailable(problem);
        }
    };

    let mut disk_config = match base_document_for_update(path) {
        Ok(disk_config) => disk_config,
        Err(refusal) => return refusal,
    };
    if let Err(merge_error) = merge(&mut disk_config) {
        return SharedConfigUpdate::MergeRejected(merge_error);
    }
    write_merged_document(path, disk_config)
}

/// Читает config без замка и без побочных эффектов (для слежения соседних копий).
#[must_use]
pub fn read_shared_config_at(path: impl AsRef<Path>) -> SharedConfigRead {
    match SharedDiskView::read(path.as_ref()) {
        SharedDiskView::Usable(config) => SharedConfigRead::Parsed(config),
        SharedDiskView::Missing => SharedConfigRead::Missing,
        SharedDiskView::Newer { found } => SharedConfigRead::Newer { found },
        SharedDiskView::Unusable(problem) => SharedConfigRead::Unreadable { problem },
    }
}

/// Документ, поверх которого вписываются изменения: файл с диска или defaults.
fn base_document_for_update<E>(path: &Path) -> Result<AppConfig, SharedConfigUpdate<E>> {
    match SharedDiskView::read(path) {
        SharedDiskView::Usable(config) => Ok(*config),
        // Файла нет (первый запуск, пользователь удалил файл) — начинаем с defaults:
        // чужих ключей, которые можно потерять, на диске просто нет.
        SharedDiskView::Missing => Ok(AppConfig::default()),
        SharedDiskView::Newer { found } => Err(SharedConfigUpdate::DiskConfigNewer { found }),
        SharedDiskView::Unusable(problem) => Err(SharedConfigUpdate::DiskConfigUnusable(problem)),
    }
}

/// Вердикт о файле на диске с точки зрения «общей доски»: всё, что нельзя взять за
/// основу, собрано в одну типизированную проблему.
enum SharedDiskView {
    /// Корректный config.
    Usable(Box<AppConfig>),
    /// Файла нет.
    Missing,
    /// Файл от более новой версии плеера.
    Newer { found: i64 },
    /// Файл нельзя использовать.
    Unusable(SharedConfigProblem),
}

impl SharedDiskView {
    /// Читает файл общим разбором (`store/disk_read.rs`) и сводит вердикт.
    fn read(path: &Path) -> Self {
        match read_disk_config(path) {
            Ok(DiskConfigState::Valid(config)) => Self::Usable(config),
            Ok(DiskConfigState::Missing) => Self::Missing,
            Ok(DiskConfigState::NewerSchema { found }) => Self::Newer { found },
            Ok(DiskConfigState::Broken(problem)) => {
                Self::Unusable(SharedConfigProblem::Broken(problem))
            }
            Ok(DiskConfigState::NotRegularFile) => {
                Self::Unusable(SharedConfigProblem::NotRegularFile)
            }
            Ok(DiskConfigState::ReadFailed { error_kind }) => {
                Self::Unusable(SharedConfigProblem::ReadFailed { error_kind })
            }
            // Восстановление при старте пробрасывает такую ошибку, чтобы не переименовать
            // исправный файл; здесь файл тоже не трогаем, но сообщаем типизированно.
            Err(_unclassified_error) => {
                Self::Unusable(SharedConfigProblem::UnclassifiedParseFailure)
            }
        }
    }
}

/// Проверяет слитый документ и атомарно записывает его.
fn write_merged_document<E>(path: &Path, merged_config: AppConfig) -> SharedConfigUpdate<E> {
    match atomic::save_validated_reporting_durability(path, &merged_config) {
        Ok(ConfigWriteDurability::Durable) => SharedConfigUpdate::Written {
            written: Box::new(merged_config),
        },
        Ok(ConfigWriteDurability::DurabilityUnconfirmed(sync_error)) => {
            SharedConfigUpdate::WrittenDurabilityUnconfirmed {
                written: Box::new(merged_config),
                sync_error,
            }
        }
        Err(error) => match ConfigWriteProblem::from_write_error(&error) {
            Some(write_problem) => SharedConfigUpdate::WriteFailed(write_problem),
            // Не ошибка записи файла, а отказ проверки/сериализации документа.
            None => SharedConfigUpdate::MergedConfigRejected(error),
        },
    }
}
