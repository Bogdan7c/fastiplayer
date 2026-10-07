//! Рекурсивный обход папки с бюджетами: чистый, отменяемый, без UI и без очереди.
//!
//! Владелец перечисления локальных файлов (drag & drop папки, сессия 12). Обход ничего
//! не знает о приложении, плейлисте и конфиге: лимиты и правило «что считать media»
//! приходят снаружи, результат — типизированный список путей и причина усечения.
//!
//! Правила обхода (решения владельца):
//! - скрытые записи (имя начинается с точки) пропускаются;
//! - символьные ссылки на папки не разворачиваются (нет циклов); ссылки на файлы
//!   считаются обычными файлами;
//! - внутри каждой папки сначала файлы, затем подпапки (в глубину), каждая группа в
//!   natural-порядке имён; путь не искажается (non-UTF-8 имена сохраняются как есть);
//! - при достижении лимита файлов обход прекращается и сохраняются первые N файлов;
//! - нечитаемая подпапка пропускается и учитывается в счётчике, а не роняет обход.

use std::cmp::Ordering;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use natural_sort_key::PreparedNaturalKey;
use source_core::CancellationToken;

use crate::manifest::is_hidden_filename;

/// Бюджеты одного обхода.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderWalkLimits {
    /// Сколько файлов максимум вернуть; лишние отбрасываются с пометкой усечения.
    max_files: NonZeroUsize,
    /// На сколько уровней вглубь заходить: `0` — только сама корневая папка.
    max_subfolder_depth: usize,
}

impl FolderWalkLimits {
    /// Создаёт бюджеты: максимум файлов и глубина подпапок (`0` — без подпапок).
    #[must_use]
    pub const fn new(max_files: NonZeroUsize, max_subfolder_depth: usize) -> Self {
        Self {
            max_files,
            max_subfolder_depth,
        }
    }

    /// Максимум возвращаемых файлов.
    #[must_use]
    pub const fn max_files(self) -> NonZeroUsize {
        self.max_files
    }

    /// Максимальная глубина подпапок.
    #[must_use]
    pub const fn max_subfolder_depth(self) -> usize {
        self.max_subfolder_depth
    }
}

/// Входные данные обхода. `accept_file` — правило «это media-кандидат» от владельца правила.
pub struct FolderWalkRequest<'walk, AcceptFile> {
    root: &'walk Path,
    limits: FolderWalkLimits,
    cancellation: &'walk CancellationToken,
    accept_file: AcceptFile,
}

impl<'walk, AcceptFile> FolderWalkRequest<'walk, AcceptFile>
where
    AcceptFile: Fn(&Path) -> bool,
{
    /// Собирает запрос обхода корневой папки.
    pub fn new(
        root: &'walk Path,
        limits: FolderWalkLimits,
        cancellation: &'walk CancellationToken,
        accept_file: AcceptFile,
    ) -> Self {
        Self {
            root,
            limits,
            cancellation,
            accept_file,
        }
    }
}

/// Почему результат неполный.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderWalkTruncation {
    /// Нашлось больше файлов, чем разрешает бюджет: оставлены первые по порядку.
    FileLimit,
    /// Подпапки глубже лимита не обходились (файлов в них могло быть сколько угодно).
    DepthLimit,
}

/// Итог успешного (возможно, усечённого) обхода.
#[derive(Debug, PartialEq, Eq)]
pub struct FolderWalkOutcome {
    /// Файлы в порядке «файлы папки, затем подпапки в глубину».
    pub files: Vec<PathBuf>,
    /// Причина неполноты; `None`, если обход дошёл до конца в пределах бюджетов.
    pub truncation: Option<FolderWalkTruncation>,
    /// Сколько подпапок не удалось прочитать (пропущены целиком).
    pub unreadable_directories: usize,
    /// Сколько записей внутри читаемых папок не удалось осмотреть (пропущены).
    pub unreadable_entries: usize,
}

/// Ошибки, из-за которых результата нет совсем.
#[derive(Debug, thiserror::Error)]
pub enum FolderWalkError {
    /// Корневую папку не удалось прочитать (нет прав, исчезла, не папка).
    #[error("не удалось прочитать корневую папку: {0:?}")]
    ReadRoot(io::ErrorKind),
    /// Владелец отменил обход; частичный результат не возвращается.
    #[error("обход папки отменён")]
    Cancelled,
}

/// Обходит папку по правилам модуля. См. описание модуля про порядок и пропуски.
pub fn walk_media_folder<AcceptFile>(
    request: FolderWalkRequest<'_, AcceptFile>,
) -> Result<FolderWalkOutcome, FolderWalkError>
where
    AcceptFile: Fn(&Path) -> bool,
{
    let mut walk = WalkState {
        limits: request.limits,
        cancellation: request.cancellation,
        accept_file: &request.accept_file,
        files: Vec::new(),
        file_limit_reached: false,
        directories_beyond_depth_limit: 0,
        unreadable_directories: 0,
        unreadable_entries: 0,
    };
    walk.visit_directory(request.root, 0, DirectoryRole::Root)?;
    Ok(walk.into_outcome())
}

/// Корень обязан читаться (иначе ошибка), вложенная папка при сбое просто пропускается.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DirectoryRole {
    Root,
    Nested,
}

/// Изменяемое состояние одного обхода.
struct WalkState<'walk, AcceptFile> {
    limits: FolderWalkLimits,
    cancellation: &'walk CancellationToken,
    accept_file: &'walk AcceptFile,
    files: Vec<PathBuf>,
    file_limit_reached: bool,
    directories_beyond_depth_limit: usize,
    unreadable_directories: usize,
    unreadable_entries: usize,
}

impl<AcceptFile> WalkState<'_, AcceptFile>
where
    AcceptFile: Fn(&Path) -> bool,
{
    /// Обходит одну папку: сначала её файлы, затем подпапки по очереди (в глубину).
    fn visit_directory(
        &mut self,
        directory: &Path,
        depth: usize,
        role: DirectoryRole,
    ) -> Result<(), FolderWalkError> {
        self.ensure_not_cancelled()?;
        let listing = match list_directory(directory, self.cancellation) {
            Ok(listing) => listing,
            Err(ListDirectoryError::Cancelled) => return Err(FolderWalkError::Cancelled),
            Err(ListDirectoryError::Read(kind)) if role == DirectoryRole::Root => {
                return Err(FolderWalkError::ReadRoot(kind));
            }
            Err(ListDirectoryError::Read(_)) => {
                self.unreadable_directories += 1;
                return Ok(());
            }
        };
        self.unreadable_entries += listing.unreadable_entries;

        for file in listing.files {
            self.ensure_not_cancelled()?;
            if !(self.accept_file)(&file) {
                continue;
            }
            // Лимит срабатывает только когда нашёлся файл СВЕРХ бюджета: ровно N файлов
            // — это полный результат, а не усечение.
            if self.files.len() >= self.limits.max_files.get() {
                self.file_limit_reached = true;
                return Ok(());
            }
            self.files.push(file);
        }

        for subdirectory in listing.directories {
            if depth >= self.limits.max_subfolder_depth {
                self.directories_beyond_depth_limit += 1;
                continue;
            }
            self.visit_directory(&subdirectory, depth + 1, DirectoryRole::Nested)?;
            if self.file_limit_reached {
                return Ok(());
            }
        }
        Ok(())
    }

    fn ensure_not_cancelled(&self) -> Result<(), FolderWalkError> {
        if self.cancellation.is_cancelled() {
            Err(FolderWalkError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Сворачивает состояние в публичный итог; предел файлов важнее предела глубины.
    fn into_outcome(self) -> FolderWalkOutcome {
        let truncation = if self.file_limit_reached {
            Some(FolderWalkTruncation::FileLimit)
        } else if self.directories_beyond_depth_limit > 0 {
            Some(FolderWalkTruncation::DepthLimit)
        } else {
            None
        };
        FolderWalkOutcome {
            files: self.files,
            truncation,
            unreadable_directories: self.unreadable_directories,
            unreadable_entries: self.unreadable_entries,
        }
    }
}

/// Отсортированное содержимое одной папки.
struct DirectoryListing {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    unreadable_entries: usize,
}

enum ListDirectoryError {
    Read(io::ErrorKind),
    Cancelled,
}

/// Читает папку: делит записи на файлы и настоящие подпапки, режет скрытые, сортирует.
fn list_directory(
    directory: &Path,
    cancellation: &CancellationToken,
) -> Result<DirectoryListing, ListDirectoryError> {
    let entries =
        fs::read_dir(directory).map_err(|error| ListDirectoryError::Read(error.kind()))?;
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut unreadable_entries = 0usize;
    for entry_result in entries {
        if cancellation.is_cancelled() {
            return Err(ListDirectoryError::Cancelled);
        }
        let Ok(entry) = entry_result else {
            unreadable_entries += 1;
            continue;
        };
        let file_name = entry.file_name();
        if is_hidden_filename(&file_name) {
            continue;
        }
        // `DirEntry::file_type` НЕ разворачивает символьную ссылку: ссылка на папку не
        // попадёт в `is_dir`, поэтому циклы через symlink невозможны.
        let Ok(file_type) = entry.file_type() else {
            unreadable_entries += 1;
            continue;
        };
        if file_type.is_dir() {
            directories.push((file_name, entry.path()));
        } else if file_type.is_file() || symlink_points_to_file(&entry) {
            files.push((file_name, entry.path()));
        }
    }
    Ok(DirectoryListing {
        files: sorted_paths(files),
        directories: sorted_paths(directories),
        unreadable_entries,
    })
}

/// Символьная ссылка на файл считается файлом; на папку или битая — нет.
fn symlink_points_to_file(entry: &fs::DirEntry) -> bool {
    entry
        .file_type()
        .is_ok_and(|file_type| file_type.is_symlink())
        && fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_file())
}

/// Natural-порядок имён; при равенстве — точное сравнение имён, чтобы порядок был детерминирован.
fn sorted_paths(mut named_paths: Vec<(OsString, PathBuf)>) -> Vec<PathBuf> {
    let mut keyed: Vec<(PreparedNaturalKey, OsString, PathBuf)> = named_paths
        .drain(..)
        .map(|(name, path)| (PreparedNaturalKey::from_os_str(&name), name, path))
        .collect();
    keyed.sort_by(|(left_key, left_name, _), (right_key, right_name, _)| {
        match left_key.cmp(right_key) {
            Ordering::Equal => left_name.cmp(right_name),
            decided => decided,
        }
    });
    keyed.into_iter().map(|(_, _, path)| path).collect()
}

#[cfg(test)]
mod tests;
