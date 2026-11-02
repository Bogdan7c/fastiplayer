//! Только чтение: есть ли наш экстеншен в профилях браузера.
//!
//! Это единственный источник статуса «установлен / отключён» (решение владельца):
//! мы смотрим на файлы профилей, которые ведёт сам браузер, и НИЧЕГО в них не пишем.
//! Файл, без которого ответ был бы выдумкой (нечитаемый, слишком большой, битый
//! JSON, не обычный файл), даёт [`Unreadable`], а не «не установлен». Отсутствие
//! корня, профиля или файла — нормальное «не установлен»: у пользователя полно
//! пустых папок. Пустой (нулевой длины) файл считается отсутствующим: браузер
//! создаёт такие при первом запуске.
//!
//! # Деградация по профилям
//!
//! Один битый файл одного профиля не отравляет весь браузер: профили оцениваются
//! независимо, а [`Unreadable`] возвращается, только если провалились ВСЕ
//! существующие файлы-кандидаты и ни один не прочитался (`ReadEvidence`).

mod chromium;
mod firefox;

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

pub use chromium::{ChromiumExtensionIds, UserUninstall, read_chromium_presence};
pub use firefox::read_firefox_presence;

/// Предел размера читаемого файла профиля: настоящие `Preferences` и
/// `extensions.json` весят мегабайты; больше — подозрительно, читать не будем.
const MAX_PROFILE_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Есть ли экстеншен в профиле и включён ли он.
///
/// Порядок вариантов = «насколько он есть»: при нескольких профилях побеждает
/// максимум ([`Presence::strongest`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Presence {
    /// Ни в одном профиле.
    Absent,
    /// Стоит, но отключён (человеком, политикой или браузером).
    Disabled,
    /// Стоит и включён.
    Enabled,
}

impl Presence {
    /// Лучшее из двух: включённый в одном профиле важнее отключённого в другом.
    #[must_use]
    pub fn strongest(self, other: Self) -> Self {
        self.max(other)
    }
}

/// Почему файл профиля прочитать не удалось.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadableKind {
    /// Ошибка ОС (нет прав, это каталог, ...).
    Io(io::ErrorKind),
    /// Файл больше предела в 64 МиБ (`MAX_PROFILE_FILE_BYTES`).
    TooLarge,
    /// Файл не разбирается как JSON.
    MalformedJson,
    /// На месте файла каталог, FIFO, сокет и т. п.: открывать такое нельзя
    /// (чтение FIFO без писателя повисло бы навсегда).
    NotARegularFile,
}

/// Файл, без которого статус определить нельзя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    /// Проблемный файл или каталог.
    pub path: PathBuf,
    /// Почему.
    pub kind: UnreadableKind,
}

impl Unreadable {
    fn io(path: &Path, error: &io::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: UnreadableKind::Io(error.kind()),
        }
    }
}

/// Читает файл с пределом размера. `None` — файла нет.
fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, Unreadable> {
    // `metadata` идёт по symlink: ссылка на обычный файл допустима. Тип проверяем
    // ДО `open`, иначе открытие FIFO без писателя заблокирует поток.
    match std::fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(Unreadable {
                path: path.to_path_buf(),
                kind: UnreadableKind::NotARegularFile,
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Unreadable::io(path, &error)),
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Unreadable::io(path, &error)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_PROFILE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| Unreadable::io(path, &error))?;
    if bytes.len() as u64 > MAX_PROFILE_FILE_BYTES {
        return Err(Unreadable {
            path: path.to_path_buf(),
            kind: UnreadableKind::TooLarge,
        });
    }
    Ok(Some(bytes))
}

/// Читает JSON-файл с пределом размера. `None` — файла нет или он пустой.
fn read_json(path: &Path) -> Result<Option<Value>, Unreadable> {
    let Some(bytes) = read_bounded(path)? else {
        return Ok(None);
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_parse_error| Unreadable {
            path: path.to_path_buf(),
            kind: UnreadableKind::MalformedJson,
        })
}

/// Накопитель итогов чтения файлов одного браузера: реализует правило «битый файл
/// одного профиля не отравляет остальные».
///
/// Инвариант: [`Unreadable`] наружу выходит только из [`ReadEvidence::finish`], и
/// только если был хотя бы один сбой и НИ ОДИН файл не прочитался.
#[derive(Debug, Default)]
pub(super) struct ReadEvidence {
    /// Сколько существующих файлов прочиталось и разобралось.
    readable_files: usize,
    /// Первый сбой в порядке обхода (порядок детерминирован сортировкой профилей).
    first_failure: Option<Unreadable>,
}

impl ReadEvidence {
    /// Учитывает результат чтения: успех копится как «прочитано», сбой запоминается.
    /// Возвращает значение, если файл существовал и прочитался.
    pub(super) fn record<T>(&mut self, outcome: Result<Option<T>, Unreadable>) -> Option<T> {
        match outcome {
            Ok(Some(value)) => {
                self.readable_files += 1;
                Some(value)
            }
            Ok(None) => None,
            Err(failure) => {
                self.record_failure(failure);
                None
            }
        }
    }

    /// Запоминает сбой, не связанный с чтением JSON (каталог, `profiles.ini`).
    pub(super) fn record_failure(&mut self, failure: Unreadable) {
        self.first_failure.get_or_insert(failure);
    }

    /// Итог: ошибка только если не прочиталось НИЧЕГО, а сбой был.
    pub(super) fn finish(self) -> Result<(), Unreadable> {
        match self.first_failure {
            Some(failure) if self.readable_files == 0 => Err(failure),
            _ => Ok(()),
        }
    }
}
