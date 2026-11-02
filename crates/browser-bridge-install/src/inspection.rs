//! Чтение и классификация того, что реально лежит на диске.
//!
//! Модуль только читает. Его результаты — единый словарь для статуса, установки и
//! отключения: все трое должны одинаково понимать, что такое «наш» файл.

use std::io;
use std::path::{Path, PathBuf};

use desktop_integration::{BROWSER_SESSION_ARGUMENT, BROWSER_SESSION_BUS_NAME};

use crate::expected::ExpectedInstall;
use crate::fs_util::has_execute_bit;
use crate::manifest::{ManifestOwnership, classify_manifest};

/// Состояние одного файла подключения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestState {
    /// Файла нет.
    Absent,
    /// Ровно то, что записал бы этот плеер.
    Identical,
    /// Наш, но другой по содержимому.
    OursButDiffers,
    /// Чужой корректный JSON под нашим именем.
    Foreign,
    /// Не JSON.
    Damaged,
    /// Прочитать не удалось.
    Unreadable {
        /// Класс ошибки ОС.
        kind: io::ErrorKind,
    },
}

impl ManifestState {
    /// Наш ли файл (в том числе устаревший): отключение удаляет только такие.
    pub fn is_ours(self) -> bool {
        matches!(self, Self::Identical | Self::OursButDiffers)
    }
}

/// Состояние копии посредника.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostState {
    /// Копии нет.
    Missing,
    /// Те же байты, исполняемый файл.
    Identical,
    /// Другие байты (другая версия плеера).
    BytesDiffer,
    /// Байты те, но бита исполнения нет.
    NotExecutable,
    /// Копию или источник прочитать не удалось.
    Unreadable {
        /// Класс ошибки ОС.
        kind: io::ErrorKind,
    },
}

/// Состояние `.service`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceState {
    /// Файла нет.
    Missing,
    /// Ровно то, что записал бы этот плеер.
    Identical,
    /// Наша служба, но будит другой бинарь плеера.
    AnotherCopy {
        /// Бинарь из `Exec=`.
        player: PathBuf,
    },
    /// Файл есть, но это не наша служба в нашем формате.
    Unrecognized,
    /// Прочитать не удалось.
    Unreadable {
        /// Класс ошибки ОС.
        kind: io::ErrorKind,
    },
}

/// Проверяет один файл подключения.
pub fn inspect_manifest(path: &Path, expected: &ExpectedInstall) -> ManifestState {
    match std::fs::read_to_string(path) {
        Ok(text) => match classify_manifest(&text, &expected.manifest_text, &expected.host_path) {
            ManifestOwnership::Identical => ManifestState::Identical,
            ManifestOwnership::OursButDiffers => ManifestState::OursButDiffers,
            ManifestOwnership::Foreign => ManifestState::Foreign,
            ManifestOwnership::Damaged => ManifestState::Damaged,
        },
        Err(error) => match error.kind() {
            io::ErrorKind::NotFound => ManifestState::Absent,
            // Не UTF-8 внутри — это порча файла, а не сбой чтения.
            io::ErrorKind::InvalidData => ManifestState::Damaged,
            kind => ManifestState::Unreadable { kind },
        },
    }
}

/// Проверяет копию посредника.
pub fn inspect_host(expected: &ExpectedInstall) -> HostState {
    let installed_metadata = match std::fs::metadata(&expected.host_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return HostState::Missing,
        Err(error) => return HostState::Unreadable { kind: error.kind() },
    };
    let source_bytes = match std::fs::read(&expected.host_source) {
        Ok(bytes) => bytes,
        Err(error) => return HostState::Unreadable { kind: error.kind() },
    };
    // Длина — дешёвый признак «другая версия» без чтения копии.
    if installed_metadata.len() != source_bytes.len() as u64 {
        return HostState::BytesDiffer;
    }
    match std::fs::read(&expected.host_path) {
        Ok(installed_bytes) if installed_bytes != source_bytes => HostState::BytesDiffer,
        Ok(_same_bytes) if !has_execute_bit(&installed_metadata) => HostState::NotExecutable,
        Ok(_same_bytes) => HostState::Identical,
        Err(error) => HostState::Unreadable { kind: error.kind() },
    }
}

/// Проверяет `.service`.
pub fn inspect_service(expected: &ExpectedInstall) -> ServiceState {
    match std::fs::read_to_string(&expected.service_path) {
        Ok(text) if text == expected.service_text => ServiceState::Identical,
        Ok(text) => match parse_service_player(&text) {
            Some(player) => ServiceState::AnotherCopy { player },
            None => ServiceState::Unrecognized,
        },
        Err(error) => match error.kind() {
            io::ErrorKind::NotFound => ServiceState::Missing,
            io::ErrorKind::InvalidData => ServiceState::Unrecognized,
            kind => ServiceState::Unreadable { kind },
        },
    }
}

/// Достаёт путь плеера из `Exec='<путь>' --browser-session` нашей службы.
///
/// Формат тот, что пишет `browser_session_activation_service_file`; всё остальное
/// (другое имя службы, иные кавычки) — «не наш формат», `None`.
pub fn parse_service_player(service_text: &str) -> Option<PathBuf> {
    let is_our_service = service_text
        .lines()
        .any(|line| line.trim() == format!("Name={BROWSER_SESSION_BUS_NAME}"));
    if !is_our_service {
        return None;
    }
    let exec_value = service_text
        .lines()
        .find_map(|line| line.trim().strip_prefix("Exec="))?;
    let after_opening_quote = exec_value.strip_prefix('\'')?;
    let (player, rest) = after_opening_quote.split_once('\'')?;
    (rest.trim() == BROWSER_SESSION_ARGUMENT).then(|| PathBuf::from(player))
}

/// Почему в каталог нельзя класть файлы (найдено без попытки записи).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryProblem {
    /// Путь, на котором споткнулись.
    pub path: PathBuf,
    /// Класс причины.
    pub kind: io::ErrorKind,
}

/// Ищет очевидную беду на пути к каталогу: symlink в никуда или файл вместо каталога.
///
/// Идёт от каталога вверх до первого существующего звена. Отсутствие каталога —
/// норма (профиль браузера может быть ещё не создан), проблемой не считается.
pub fn find_directory_problem(directory: &Path) -> Option<DirectoryProblem> {
    for ancestor in directory.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Some(DirectoryProblem {
                    path: ancestor.to_path_buf(),
                    kind: error.kind(),
                });
            }
            Ok(_link_metadata) => {
                // `metadata` разыменовывает symlink: ошибка = висячая ссылка.
                return match std::fs::metadata(ancestor) {
                    Err(error) => Some(DirectoryProblem {
                        path: ancestor.to_path_buf(),
                        kind: error.kind(),
                    }),
                    Ok(metadata) if !metadata.is_dir() => Some(DirectoryProblem {
                        path: ancestor.to_path_buf(),
                        kind: io::ErrorKind::NotADirectory,
                    }),
                    Ok(_directory) => None,
                };
            }
        }
    }
    None
}
