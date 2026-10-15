//! Ограниченный просмотр одного каталога: лимит записей, отмена, учёт ошибок I/O.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::SidecarSearchError;
use crate::limits::SidecarSearchLimits;
use crate::outcome::{SidecarSearchReport, UnreadableDirectory};

/// Тип записи после разыменования символических ссылок.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    File,
    Directory,
}

/// Запись каталога, годная для дальнейшей классификации (прочие типы отброшены).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScannedEntry {
    pub(crate) name: OsString,
    pub(crate) path: PathBuf,
    pub(crate) kind: EntryKind,
}

/// Читает записи каталога, соблюдая лимиты.
///
/// * Каталог не открылся — пустой список и запись в `report.unreadable_directories`.
/// * Лимит записей исчерпан — просмотр останавливается, растёт счётчик в отчёте.
/// * Отмена проверяется перед каждой записью и даёт `Err(Cancelled)`.
/// * Порядок возврата — по имени (`OsString`), чтобы результат не зависел от порядка ФС.
pub(crate) fn scan_directory(
    directory: &Path,
    limits: &SidecarSearchLimits,
    is_cancelled: &dyn Fn() -> bool,
    report: &mut SidecarSearchReport,
) -> Result<Vec<ScannedEntry>, SidecarSearchError> {
    if is_cancelled() {
        return Err(SidecarSearchError::Cancelled);
    }
    let read_dir = match fs::read_dir(directory) {
        Ok(read_dir) => read_dir,
        Err(error) => {
            report.unreadable_directories.push(UnreadableDirectory {
                path: directory.to_path_buf(),
                error_kind: error.kind(),
            });
            return Ok(Vec::new());
        }
    };
    report.directories_scanned += 1;

    let mut entries = Vec::new();
    for (scanned_in_directory, entry_result) in read_dir.enumerate() {
        if is_cancelled() {
            return Err(SidecarSearchError::Cancelled);
        }
        if scanned_in_directory >= limits.max_directory_entries_scanned_per_directory {
            report.directories_truncated_by_entry_limit += 1;
            break;
        }
        report.entries_scanned += 1;
        match entry_result {
            Ok(entry) => classify_entry(entry, &mut entries, report),
            Err(_) => report.unreadable_entries += 1,
        }
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

/// Определяет тип записи с разыменованием ссылок; сломанная ссылка — нечитаемая запись.
fn classify_entry(
    entry: fs::DirEntry,
    entries: &mut Vec<ScannedEntry>,
    report: &mut SidecarSearchReport,
) {
    let path = entry.path();
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(_) => {
            report.unreadable_entries += 1;
            return;
        }
    };
    let kind = if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        return;
    };
    entries.push(ScannedEntry {
        name: entry.file_name(),
        path,
        kind,
    });
}
