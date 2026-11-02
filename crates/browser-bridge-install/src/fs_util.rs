//! Мелкие файловые помощники установщика, одинаковые на всех платформах.
//!
//! Права доступа имеют смысл только в Unix; на остальных платформах бит
//! исполняемости не проверяется и не ставится (там исполняемость задаёт расширение).

use std::io;
use std::path::Path;

/// Права исполняемого файла посредника: владелец пишет и исполняет, остальные читают и исполняют.
pub(crate) const EXECUTABLE_MODE: u32 = 0o755;
/// Права обычных файлов установки (манифест, `.service`).
pub(crate) const REGULAR_FILE_MODE: u32 = 0o644;

/// Обычный файл с битом исполнения (symlink разыменовывается, как это делает оболочка).
pub(crate) fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    has_execute_bit(&metadata)
}

/// Есть ли у файла хотя бы один бит исполнения.
#[cfg(unix)]
pub(crate) fn has_execute_bit(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

/// Вне Unix бита нет: любой обычный файл считается исполняемым.
#[cfg(not(unix))]
pub(crate) fn has_execute_bit(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// Ставит права файлу (Unix); вне Unix ничего не делает.
#[cfg(unix)]
pub(crate) fn set_file_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Вне Unix права не ставятся.
#[cfg(not(unix))]
pub(crate) fn set_file_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}
