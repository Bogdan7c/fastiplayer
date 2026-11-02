//! «Подсказка» браузеру: файл `External Extensions/<ID>.json` в папке профилей.
//!
//! Chromium-браузеры, читающие пользовательскую папку, при следующем запуске сами
//! ставят из магазина каждый экстеншен, на который тут есть подсказка. Содержимое
//! фиксировано: `{"external_update_url": "<сервер обновлений Chrome Web Store>"}`.
//!
//! Модуль владеет ровно одним файлом на браузер и его инвариантами:
//! - пишем атомарно, прежний файл при сбое остаётся нетронутым;
//! - удаляем ТОЛЬКО файл с нашим содержимым ([`HintState::Ours`]): чужое
//!   содержимое под нашим именем не трогаем и сообщаем об этом;
//! - пустой каталог `External Extensions/` убираем только если он настоящий каталог
//!   (не symlink) и в нём больше ничего нет; любой сбой уборки — не ошибка;
//! - ЧУЖОЕ не трогаем никогда: файл с другим содержимым и всё, что не обычный файл
//!   (FIFO, каталог, сокет), считается [`HintState::Foreign`] — не открывается, не
//!   удаляется, не перезаписывается.
//!
//! # Осознанно принятые риски (без кода)
//!
//! - Если `External Extensions` — symlink, запись идёт по нему: Chromium читает
//!   папку точно так же (через ссылку), поэтому подсказка всё равно сработает.
//! - Между проверкой подсказки и её удалением есть короткое окно (TOCTOU). В нём
//!   подменить файл может только процесс того же пользователя, а он и так вправе
//!   делать с этими файлами что угодно; защищаться от него смысла нет.
//!
//! # Что подсказка не умеет
//!
//! Подсказка только СТАВИТ экстеншен. Удаление файла уже установленный экстеншен не
//! убирает (проверено вживую на Chromium 156): убрать его может только человек на
//! странице расширений браузера. Поэтому [`remove_hint`] годится лишь для отмены
//! ещё не случившейся установки.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use atomic_file_store::{AtomicFileWriteOutcome, replace_file_atomically};
use serde_json::{Value, json};

use super::error::{ExtensionInstallError, HintStep};
use super::listing::CHROME_WEB_STORE_UPDATE_URL;

/// Подкаталог профильной папки браузера с подсказками.
const EXTERNAL_EXTENSIONS_DIRECTORY: &str = "External Extensions";
/// Ключ подсказки с адресом сервера обновлений.
const UPDATE_URL_KEY: &str = "external_update_url";
/// Подсказка весит десятки байт; всё, что больше, точно не наше.
const MAX_HINT_BYTES: u64 = 64 * 1024;

/// Что лежит под нашим именем подсказки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintState {
    /// Файла нет.
    Absent,
    /// Файл с ровно нашим содержимым (разбор смысловой, а не побайтовый).
    Ours,
    /// Файл есть, но содержимое другое (чужое, ручное, битое).
    Foreign,
}

/// Чем закончилось удаление подсказки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveHintOutcome {
    /// Наш файл удалён.
    Removed {
        /// Что стало с каталогом `External Extensions/`.
        directory: DirectoryCleanup,
    },
    /// Файла не было, делать нечего.
    NothingToRemove,
    /// Под нашим именем лежит чужое содержимое: оставлено как есть.
    ForeignHintKept,
}

/// Судьба каталога `External Extensions/` после удаления подсказки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryCleanup {
    /// Пустой каталог удалён.
    Removed,
    /// Каталог оставлен (в нём есть другие файлы, это symlink или удалить не вышло).
    Kept,
}

/// Путь подсказки для экстеншена `extension_id` в папке профилей `user_data`.
pub fn hint_path(user_data: &Path, extension_id: &str) -> PathBuf {
    external_extensions_directory(user_data).join(format!("{extension_id}.json"))
}

fn external_extensions_directory(user_data: &Path) -> PathBuf {
    user_data.join(EXTERNAL_EXTENSIONS_DIRECTORY)
}

/// Подсказку прочитать не удалось (нет прав, на её месте каталог, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintReadFailure {
    /// Путь подсказки.
    pub path: PathBuf,
    /// Класс ошибки ОС.
    pub kind: io::ErrorKind,
}

impl From<HintReadFailure> for ExtensionInstallError {
    fn from(failure: HintReadFailure) -> Self {
        Self::HintIo {
            step: HintStep::ReadHint,
            path: failure.path,
            kind: failure.kind,
        }
    }
}

/// Читает подсказку, ничего не меняя.
///
/// Тип файла проверяется ДО открытия (`metadata` идёт по symlink): FIFO, каталог
/// или сокет на месте подсказки — это чужое ([`HintState::Foreign`]), открывать
/// его нельзя, иначе чтение FIFO без писателя повисло бы навсегда.
pub fn inspect_hint(path: &Path) -> Result<HintState, HintReadFailure> {
    let read_failure = |error: io::Error| HintReadFailure {
        path: path.to_path_buf(),
        kind: error.kind(),
    };
    match std::fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => return Ok(HintState::Foreign),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HintState::Absent),
        Err(error) => return Err(read_failure(error)),
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HintState::Absent),
        Err(error) => return Err(read_failure(error)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_HINT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(read_failure)?;
    if bytes.len() as u64 > MAX_HINT_BYTES {
        return Ok(HintState::Foreign);
    }
    Ok(classify_hint(&bytes))
}

/// Наше ли содержимое: JSON-объект ровно с одним ключом `external_update_url`,
/// равным адресу Chrome Web Store. Пробелы и порядок не важны.
fn classify_hint(bytes: &[u8]) -> HintState {
    let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(bytes) else {
        return HintState::Foreign;
    };
    let is_ours = fields.len() == 1
        && fields.get(UPDATE_URL_KEY).and_then(Value::as_str) == Some(CHROME_WEB_STORE_UPDATE_URL);
    if is_ours {
        HintState::Ours
    } else {
        HintState::Foreign
    }
}

/// Записывает подсказку (создаёт `External Extensions/`, если его нет).
///
/// Свой прежний файл перезаписывается тем же содержимым (идемпотентно). Чужой
/// файл под этим именем НЕ заменяется никогда: ответ
/// [`ExtensionInstallError::ForeignHintInTheWay`], диск не тронут.
pub fn write_hint(user_data: &Path, extension_id: &str) -> Result<PathBuf, ExtensionInstallError> {
    let path = hint_path(user_data, extension_id);
    if inspect_hint(&path)? == HintState::Foreign {
        return Err(ExtensionInstallError::ForeignHintInTheWay { path });
    }
    let directory = external_extensions_directory(user_data);
    std::fs::create_dir_all(&directory).map_err(|error| ExtensionInstallError::HintIo {
        step: HintStep::CreateDirectory,
        path: directory.clone(),
        kind: error.kind(),
    })?;
    let contents =
        serde_json::to_vec_pretty(&json!({ UPDATE_URL_KEY: CHROME_WEB_STORE_UPDATE_URL }))
            .map_err(|_serialization_error| ExtensionInstallError::HintIo {
                step: HintStep::WriteHint,
                path: path.clone(),
                kind: io::ErrorKind::InvalidData,
            })?;
    match replace_file_atomically(&path, &contents) {
        AtomicFileWriteOutcome::Durable => {}
        AtomicFileWriteOutcome::ReplacedDurabilityUnconfirmed(error) => {
            // Файл на месте и виден браузеру; только запись на диск не подтверждена.
            tracing::warn!(?error, "extension hint written, durability unconfirmed");
        }
        AtomicFileWriteOutcome::NotReplaced(failure) => {
            return Err(ExtensionInstallError::HintAtomicWrite { path, failure });
        }
    }
    Ok(path)
}

/// Удаляет подсказку, только если она наша. Это ОТМЕНА ещё не случившейся
/// установки; уже поставленный экстеншен файл не удерживает (см. шапку модуля).
pub fn remove_hint(
    user_data: &Path,
    extension_id: &str,
) -> Result<RemoveHintOutcome, ExtensionInstallError> {
    let path = hint_path(user_data, extension_id);
    match inspect_hint(&path)? {
        HintState::Absent => Ok(RemoveHintOutcome::NothingToRemove),
        HintState::Foreign => Ok(RemoveHintOutcome::ForeignHintKept),
        HintState::Ours => {
            std::fs::remove_file(&path).map_err(|error| ExtensionInstallError::HintIo {
                step: HintStep::RemoveHint,
                path: path.clone(),
                kind: error.kind(),
            })?;
            Ok(RemoveHintOutcome::Removed {
                directory: remove_directory_if_empty(&external_extensions_directory(user_data)),
            })
        }
    }
}

/// Убирает пустой настоящий каталог. `remove_dir` сам отказывает на непустом,
/// поэтому гонка с браузером, положившим туда файл, ничего не теряет.
fn remove_directory_if_empty(directory: &Path) -> DirectoryCleanup {
    let is_real_directory = std::fs::symlink_metadata(directory)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false);
    if !is_real_directory {
        return DirectoryCleanup::Kept;
    }
    match std::fs::remove_dir(directory) {
        Ok(()) => DirectoryCleanup::Removed,
        Err(error) => {
            tracing::debug!(kind = ?error.kind(), "External Extensions directory kept");
            DirectoryCleanup::Kept
        }
    }
}
