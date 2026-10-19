//! Открытие файла субтитров только если это обычный файл.
//!
//! Брошенный или выбранный путь может оказаться FIFO, сокетом или устройством. Блокирующий
//! `open`/`read` такого файла (FIFO без писателя) зависает навсегда: фоновый поток не
//! завершается, а выход приложения ждёт его. Поэтому до чтения убеждаемся по метаданным
//! (со следованием по ссылкам), что это обычный файл, и проверяем это ещё раз по уже
//! открытому дескриптору: между проверкой и открытием путь могли подменить.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use crate::state::subtitles::sidecar_job::SubtitleLoadFailure;

/// Открывает обычный файл на чтение; всё прочее — отказ «файл не открывается».
pub(super) fn open_regular_file(path: &Path) -> Result<File, SubtitleLoadFailure> {
    // `metadata` следует по символическим ссылкам: ссылка на обычный файл — допустима.
    let path_metadata = std::fs::metadata(path).map_err(read_failed)?;
    if !path_metadata.is_file() {
        return Err(not_a_regular_file());
    }
    let file = open_without_blocking(path).map_err(read_failed)?;
    // Путь могли подменить между проверкой и открытием: смотрим на сам дескриптор.
    let file_metadata = file.metadata().map_err(read_failed)?;
    if !file_metadata.is_file() {
        return Err(not_a_regular_file());
    }
    Ok(file)
}

/// Открытие, которое не ждёт писателя у FIFO (на обычные файлы флаг не влияет).
#[cfg(unix)]
fn open_without_blocking(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

/// На платформах без FIFO в этом смысле достаточно проверки метаданных.
#[cfg(not(unix))]
fn open_without_blocking(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

/// Причина для всего, что не обычный файл: своя, а не выдуманная ошибка ввода-вывода.
const fn not_a_regular_file() -> SubtitleLoadFailure {
    SubtitleLoadFailure::NotARegularFile
}

/// Ошибка ввода-вывода без пути в причине.
fn read_failed(error: io::Error) -> SubtitleLoadFailure {
    SubtitleLoadFailure::ReadFailed { kind: error.kind() }
}
