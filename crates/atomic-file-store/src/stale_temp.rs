//! Уборка temp-файлов, брошенных прерванными прошлыми запусками.
//!
//! Если процесс упал (или выключили питание) между созданием temp и rename, рядом с
//! target остаётся `.<target>.save-<pid>-<nonce>.tmp`. Обычный протокол такие файлы
//! никогда не трогает: он удаляет только temp текущей попытки. Этот модуль — отдельная
//! явная операция для caller-а, который **гарантирует**, что сейчас никто другой не
//! пишет в этот target (например, удерживает lease единственного экземпляра).
//!
//! Шаблон имени принадлежит этому crate (см. `temp_file_name` в `lib.rs`), поэтому и
//! распознавание шаблона живёт здесь: caller не должен знать формат чужих temp-имён.
//!
//! Инварианты безопасности:
//! - сканируется только родительский каталог target-а, без рекурсии;
//! - удаляется только обычный файл (не каталог и не symlink), чьё имя **точно**
//!   совпадает с шаблоном именно этого target-а: `.<target>.save-<цифры>-<цифры>.tmp`;
//! - похожие, но не совпадающие имена (другой target, лишние символы) не трогаются.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::Path;

use crate::parent_directory;

/// Итог уборки брошенных temp-файлов одного target-а.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StaleTempCleanupReport {
    /// Сколько брошенных temp-файлов удалено.
    pub removed_count: usize,
    /// Безопасные классы ошибок для temp-файлов, которые удалить не удалось.
    pub removal_failures: Vec<io::ErrorKind>,
}

/// Ошибка, из-за которой каталог target-а вообще не удалось просмотреть.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaleTempScanError {
    /// Target path не содержит имени файла, шаблон построить нельзя.
    InvalidTargetPath,
    /// Родительский каталог не читается.
    ReadDirectory(io::ErrorKind),
}

/// Удаляет temp-файлы этого crate-а, брошенные прошлыми прерванными записями `target_path`.
///
/// Caller обязан гарантировать, что параллельно никто не пишет в тот же target: иначе
/// можно удалить temp живой записи. Отсутствующий каталог — штатный «ничего не найдено».
pub fn remove_stale_temp_files(
    target_path: &Path,
) -> Result<StaleTempCleanupReport, StaleTempScanError> {
    // Без имени target-а нельзя построить точный шаблон — ничего не трогаем.
    let Some(target_file_name) = target_path
        .file_name()
        .filter(|file_name| !file_name.is_empty())
    else {
        return Err(StaleTempScanError::InvalidTargetPath);
    };
    let directory_entries = match fs::read_dir(parent_directory(target_path)) {
        Ok(entries) => entries,
        // Каталога ещё нет — значит, и брошенных temp-файлов нет.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(StaleTempCleanupReport::default());
        }
        Err(error) => return Err(StaleTempScanError::ReadDirectory(error.kind())),
    };

    let mut report = StaleTempCleanupReport::default();
    for directory_entry in directory_entries {
        // Ошибка чтения одной записи не мешает уборке остальных, но учитывается.
        let directory_entry = match directory_entry {
            Ok(entry) => entry,
            Err(error) => {
                report.removal_failures.push(error.kind());
                continue;
            }
        };
        if !is_stale_temp_name_for(&directory_entry.file_name(), target_file_name) {
            continue;
        }
        // `file_type` у `DirEntry` не следует по symlink: удаляем только обычный файл.
        let is_regular_file = directory_entry
            .file_type()
            .is_ok_and(|file_type| file_type.is_file());
        if !is_regular_file {
            continue;
        }
        match fs::remove_file(directory_entry.path()) {
            Ok(()) => report.removed_count += 1,
            // Файл мог исчезнуть между чтением каталога и удалением — цель достигнута.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => report.removal_failures.push(error.kind()),
        }
    }
    Ok(report)
}

/// Проверяет точное совпадение имени с шаблоном `.<target>.save-<цифры>-<цифры>.tmp`.
fn is_stale_temp_name_for(candidate_name: &OsStr, target_file_name: &OsStr) -> bool {
    // Не-UTF-8 имя не могло быть создано нашим шаблоном для UTF-8 target-а и наоборот.
    let (Some(candidate_name), Some(target_file_name)) =
        (candidate_name.to_str(), target_file_name.to_str())
    else {
        return false;
    };
    let Some(counters) = candidate_name
        .strip_prefix('.')
        .and_then(|rest| rest.strip_prefix(target_file_name))
        .and_then(|rest| rest.strip_prefix(".save-"))
        .and_then(|rest| rest.strip_suffix(".tmp"))
    else {
        return false;
    };
    // Между префиксом и суффиксом — ровно `<pid>-<nonce>`, оба только из цифр.
    let Some((process_id, nonce)) = counters.split_once('-') else {
        return false;
    };
    is_ascii_number(process_id) && is_ascii_number(nonce)
}

/// Непустая строка только из ASCII-цифр.
fn is_ascii_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{is_stale_temp_name_for, remove_stale_temp_files};
    use crate::{AtomicFileWriteOutcome, replace_file_atomically};
    use std::ffi::OsStr;

    /// Брошенный temp удаляется, а target, чужие temp и похожие имена остаются.
    #[test]
    fn removes_only_exact_stale_temp_files_of_target() {
        let directory = tempfile::tempdir().expect("temp directory доступен");
        let target_path = directory.path().join("config.toml");
        fs::write(&target_path, b"target").expect("target создан");
        // Брошенный temp прошлого запуска — ровно наш шаблон.
        let stale_temp = directory.path().join(".config.toml.save-4242-7.tmp");
        fs::write(&stale_temp, b"half-written").expect("stale temp создан");
        // Имена, которые похожи, но не принадлежат этому target-у или шаблону.
        let foreign_names = [
            ".playlist-state.json.save-4242-7.tmp",
            ".config.toml.4242.0.tmp",
            ".config.toml.save-4242-7.tmp.keep",
            ".config.toml.save-abc-7.tmp",
            "config.toml.save-4242-7.tmp",
            ".config.toml.save-4242.tmp",
        ];
        for foreign_name in foreign_names {
            fs::write(directory.path().join(foreign_name), b"foreign").expect("foreign создан");
        }

        let report = remove_stale_temp_files(&target_path).expect("каталог читается");

        assert_eq!(report.removed_count, 1);
        assert!(report.removal_failures.is_empty());
        assert!(!stale_temp.exists());
        assert_eq!(fs::read(&target_path).expect("target цел"), b"target");
        for foreign_name in foreign_names {
            assert!(
                directory.path().join(foreign_name).exists(),
                "чужой файл {foreign_name} не должен удаляться"
            );
        }
    }

    /// Каталог с именем temp-шаблона не удаляется: убираем только обычные файлы.
    #[test]
    fn directory_matching_pattern_is_not_removed() {
        let directory = tempfile::tempdir().expect("temp directory доступен");
        let target_path = directory.path().join("config.toml");
        let directory_with_temp_name = directory.path().join(".config.toml.save-1-2.tmp");
        fs::create_dir(&directory_with_temp_name).expect("каталог создан");

        let report = remove_stale_temp_files(&target_path).expect("каталог читается");

        assert_eq!(report.removed_count, 0);
        assert!(directory_with_temp_name.is_dir());
    }

    /// Отсутствующий каталог — штатный «ничего не найдено», а не ошибка.
    #[test]
    fn missing_directory_reports_nothing_removed() {
        let directory = tempfile::tempdir().expect("temp directory доступен");
        let target_path = directory.path().join("absent").join("config.toml");

        let report = remove_stale_temp_files(&target_path).expect("отсутствие каталога штатно");

        assert_eq!(report.removed_count, 0);
    }

    /// Шаблон распознавания совпадает с тем, что реально создаёт протокол записи.
    #[test]
    fn recognizer_matches_name_produced_by_write_protocol() {
        let produced_name = crate::temp_file_name(OsStr::new("config.toml"), 17);

        assert!(is_stale_temp_name_for(
            &produced_name,
            OsStr::new("config.toml")
        ));
        assert!(!is_stale_temp_name_for(
            &produced_name,
            OsStr::new("config.tom")
        ));
        // Успешная запись не оставляет ничего, что уборка могла бы найти.
        let directory = tempfile::tempdir().expect("temp directory доступен");
        let target_path = directory.path().join("config.toml");
        assert_eq!(
            replace_file_atomically(&target_path, b"payload"),
            AtomicFileWriteOutcome::Durable
        );
        let report = remove_stale_temp_files(&target_path).expect("каталог читается");
        assert_eq!(report.removed_count, 0);
    }
}
