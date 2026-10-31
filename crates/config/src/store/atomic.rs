//! Запись config-файла на диск.
//!
//! Весь протокол атомарной записи (temp рядом с target, права 0600, fsync, rename,
//! sync каталога) принадлежит crate `atomic-file-store` — одному владельцу на весь
//! проект. Здесь остаются только решения config-слоя: что записать (validated TOML с
//! проверкой roundtrip), когда создать каталог и как сообщить об ошибке.
//!
//! Инвариант: и первое создание defaults, и обычный save идут через один протокол,
//! поэтому прерванная запись никогда не оставляет обрезанный `config.toml` — в худшем
//! случае рядом остаётся брошенный temp, который уберёт [`remove_stale_save_temp_files`].
//!
//! Config пишут несколько копий плеера (обычное окно и окна браузерного режима, решение
//! 135 моста). Взаимное исключение писателей принадлежит не этому модулю, а замку записи
//! `store/shared_file/lock.rs`: здесь только «как записать один готовый документ».

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::Path;

use atomic_file_store::{
    AtomicFileWriteOutcome, DirectorySyncError, remove_stale_temp_files, replace_file_atomically,
};
use tracing::{info, warn};

use crate::error::ConfigWriteFailure;
use crate::{AppConfig, ConfigError, ConfigResult};

/// Насколько надёжно записан config после успешной замены файла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfigWriteDurability {
    /// Файл заменён, и каталог подтвердил запись (переживёт выключение питания).
    Durable,
    /// Файл заменён и читается, но sync каталога не подтверждён.
    DurabilityUnconfirmed(DirectorySyncError),
}

/// Валидирует конфигурацию и атомарно заменяет целевой TOML-файл.
///
/// Неподтверждённый sync каталога здесь только пишется в лог; кому различие важно
/// (сохранение общей доски), зовёт [`save_validated_reporting_durability`].
pub(super) fn save_validated(path: &Path, config: &AppConfig) -> ConfigResult<()> {
    save_validated_reporting_durability(path, config).map(|_durability| ())
}

/// Валидирует конфигурацию, атомарно заменяет TOML-файл и сообщает надёжность записи.
pub(super) fn save_validated_reporting_durability(
    path: &Path,
    config: &AppConfig,
) -> ConfigResult<ConfigWriteDurability> {
    let toml_text = prepare_validated_toml_for_save(path, config)?;
    create_parent_dir_if_needed(path)?;
    let durability = replace_config_file(path, &toml_text)?;
    info!(path = %path.display(), "Fastiplayer configuration saved through atomic rename");
    Ok(durability)
}

/// Создаёт parent directory для нового или заменяемого config-файла.
pub(super) fn create_parent_dir_if_needed(path: &Path) -> ConfigResult<()> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    fs::create_dir_all(parent).map_err(|source| ConfigError::CreateConfigDir {
        path: parent.to_path_buf(),
        source,
    })
}

/// Атомарно записывает первый default config.
///
/// Раньше файл создавался сразу под настоящим именем без fsync: сбой посреди записи
/// оставлял обрезанный TOML, и плеер падал при каждом следующем запуске. Теперь
/// используется тот же протокол temp + fsync + rename, что и у обычного save.
/// От соседних писателей защищает вызывающий код: восстановление при старте создаёт
/// defaults под замком записи config-а (`store/recovery.rs`).
pub(super) fn write_initial_config(path: &Path, toml_text: &str) -> ConfigResult<()> {
    create_parent_dir_if_needed(path)?;
    replace_config_file(path, toml_text).map(|_durability| ())
}

/// Удаляет temp-файлы config-а, брошенные прошлыми прерванными записями.
///
/// Вызывать только под замком записи config-а (`store/shared_file/lock.rs`): каждый
/// писатель держит этот замок от создания своего temp до rename, поэтому под замком ни
/// одна живая запись в этот config идти не может и чужой temp «посреди записи» не
/// удаляется. Убираются два точных шаблона:
/// текущий шаблон `atomic-file-store` и прежний собственный шаблон config-а
/// `.config.toml.<pid>.<попытка>.tmp` (его могли оставить версии до этой правки).
/// Ошибки уборки не мешают запуску: они только пишутся в лог.
pub(super) fn remove_stale_save_temp_files(path: &Path) {
    match remove_stale_temp_files(path) {
        Ok(report) => {
            if report.removed_count > 0 {
                info!(
                    removed_count = report.removed_count,
                    "Removed temporary config files"
                );
            }
            for failure_kind in report.removal_failures {
                warn!(error_kind = ?failure_kind, "Could not remove the abandoned temporary configuration file");
            }
        }
        Err(scan_error) => {
            warn!(error = ?scan_error, "Cannot view the config directory to clean temporary files")
        }
    }
    remove_legacy_save_temp_files(path);
}

/// Записывает готовый TOML через единый протокол `atomic-file-store`.
fn replace_config_file(path: &Path, toml_text: &str) -> ConfigResult<ConfigWriteDurability> {
    match replace_file_atomically(path, toml_text.as_bytes()) {
        AtomicFileWriteOutcome::Durable => Ok(ConfigWriteDurability::Durable),
        // Файл уже заменён и читается; не подтверждена только устойчивость записи
        // каталога к выключению питания. Это предупреждение, а не провал save.
        AtomicFileWriteOutcome::ReplacedDurabilityUnconfirmed(sync_error) => {
            warn!(path = %path.display(), error = ?sync_error, "Config recorded, but directory sync not confirmed");
            Ok(ConfigWriteDurability::DurabilityUnconfirmed(sync_error))
        }
        AtomicFileWriteOutcome::NotReplaced(failure) => Err(ConfigError::ReplaceConfigFile {
            path: path.to_path_buf(),
            failure: ConfigWriteFailure(failure),
        }),
    }
}

fn prepare_validated_toml_for_save(path: &Path, config: &AppConfig) -> ConfigResult<String> {
    config
        .validate()
        .map_err(|source| ConfigError::ValidateConfigFile {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
    let toml_text = config.to_pretty_toml()?;
    let reparsed = toml::from_str::<AppConfig>(&toml_text).map_err(|source| {
        ConfigError::ParseSerializedConfig {
            path: path.to_path_buf(),
            source,
        }
    })?;
    if reparsed != *config {
        return Err(ConfigError::SerializedConfigRoundtripMismatch {
            path: path.to_path_buf(),
        });
    }
    Ok(toml_text)
}

/// Убирает temp-файлы прежнего собственного шаблона `.config.toml.<pid>.<попытка>.tmp`.
fn remove_legacy_save_temp_files(path: &Path) {
    let Some(target_file_name) = path.file_name() else {
        return;
    };
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory_entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            warn!(error = %error, "Cannot view the config directory to clean up old temporary files");
            return;
        }
    };
    for directory_entry in directory_entries.flatten() {
        if !is_legacy_save_temp_name(&directory_entry.file_name(), target_file_name) {
            continue;
        }
        // Только обычный файл: каталог или symlink с похожим именем не наш.
        if !directory_entry
            .file_type()
            .is_ok_and(|file_type| file_type.is_file())
        {
            continue;
        }
        match fs::remove_file(directory_entry.path()) {
            Ok(()) => info!("Old format config file deleted"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                warn!(error = %error, "Could not remove the abandoned temporary configuration file in the old format")
            }
        }
    }
}

/// Точное совпадение с прежним шаблоном `.<target>.<цифры>.<цифры>.tmp`.
fn is_legacy_save_temp_name(candidate_name: &OsStr, target_file_name: &OsStr) -> bool {
    let (Some(candidate_name), Some(target_file_name)) =
        (candidate_name.to_str(), target_file_name.to_str())
    else {
        return false;
    };
    let Some(counters) = candidate_name
        .strip_prefix('.')
        .and_then(|rest| rest.strip_prefix(target_file_name))
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| rest.strip_suffix(".tmp"))
    else {
        return false;
    };
    let Some((process_id, attempt)) = counters.split_once('.') else {
        return false;
    };
    [process_id, attempt]
        .iter()
        .all(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_schema_toml_roundtrip_is_textually_stable() {
        let expected_current_toml = include_str!("../../tests/fixtures/current_schema_v29.toml");
        let generated_current_toml = AppConfig::default()
            .to_pretty_toml()
            .expect("serialize current defaults");
        let parsed: AppConfig =
            toml::from_str(expected_current_toml).expect("parse golden current schema");
        let roundtripped_toml = parsed
            .to_pretty_toml()
            .expect("serialize parsed current schema");

        assert_eq!(generated_current_toml, expected_current_toml);
        assert_eq!(roundtripped_toml, expected_current_toml);
        assert_eq!(parsed, AppConfig::default());
    }
}
