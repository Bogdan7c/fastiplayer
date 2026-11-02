//! Замена копии посредника с правами, выставленными ДО подмены.
//!
//! Общий атомарный писатель создаёт файл с правами 0600 и меняет права уже после
//! `rename`: между этими шагами браузер, уже подключённый к мосту, мог бы запустить
//! неисполняемого посредника, а при сбое `chmod` файл так и остался бы 0600. Здесь
//! порядок другой: временный файл рядом → запись → fsync → права → `rename` → fsync
//! каталога. Прежняя копия остаётся нетронутой, пока новая не готова целиком.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Счётчик, чтобы имена временных файлов не повторялись внутри процесса.
static NEXT_TEMP_NUMBER: AtomicU64 = AtomicU64::new(0);

/// Чем закончилась замена.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HostReplaceOutcome {
    /// Файл заменён, каталог синхронизирован.
    Durable,
    /// Файл заменён, но запись каталога на диск не подтверждена.
    DurabilityUnconfirmed,
}

/// Удаляет временный файл, пока замена не завершилась успешно.
struct TempFileGuard {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.armed {
            // Файл создан этой попыткой; если удалить не удалось, он уже исчез.
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Заменяет `target` байтами `contents` с правами `mode`.
///
/// `apply_mode` — шов для тестов («chmod упал»); в продакшене это `set_file_mode`.
pub(crate) fn replace_with_mode(
    target: &Path,
    contents: &[u8],
    mode: u32,
    apply_mode: impl Fn(&Path, u32) -> io::Result<()>,
) -> io::Result<HostReplaceOutcome> {
    let directory = target.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let file_name = target.file_name().ok_or(io::ErrorKind::InvalidInput)?;
    let temp_path = directory.join(format!(
        ".{}.{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id(),
        NEXT_TEMP_NUMBER.fetch_add(1, Ordering::Relaxed)
    ));
    // `create_new`: чужой файл с таким именем не затираем.
    let mut temp_file: File = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    let mut guard = TempFileGuard {
        path: temp_path.clone(),
        armed: true,
    };
    temp_file.write_all(contents)?;
    temp_file.sync_all()?;
    drop(temp_file);
    apply_mode(&temp_path, mode)?;
    std::fs::rename(&temp_path, target)?;
    guard.armed = false;
    Ok(match atomic_file_store::sync_parent_directory(target) {
        Ok(()) => HostReplaceOutcome::Durable,
        Err(_sync_error) => HostReplaceOutcome::DurabilityUnconfirmed,
    })
}
