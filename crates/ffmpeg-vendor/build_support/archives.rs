//! Получение, проверка и распаковка архивов с исходниками.
//!
//! Порядок всегда одинаковый: найти архив (локальный по env или скачанный),
//! проверить SHA-256 и только потом распаковать. Непроверенный архив до
//! компилятора не доходит.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::build_error::VendoredBuildError;
use crate::checksum::verify_sha256;
use crate::commands::{self, DOWNLOAD_TOOL};
use crate::sources::PinnedSource;
use crate::vendored_build::VendoredLayout;

/// Возвращает путь к проверенному архиву с исходниками.
///
/// Архив из переменной окружения (`FASTIPLAYER_*_ARCHIVE`) используется как
/// есть — это путь для офлайн-сборки. Иначе архив скачивается один раз и
/// кешируется в `OUT_DIR`; кеш перепроверяется при каждом использовании.
pub fn verified_archive(
    source: &PinnedSource,
    layout: &VendoredLayout,
) -> Result<PathBuf, VendoredBuildError> {
    if let Some(local_archive) = env::var_os(source.local_archive_env) {
        let local_archive = PathBuf::from(local_archive);
        verify_sha256(&local_archive, source.sha256).map_err(|checksum_error| {
            VendoredBuildError::Checksum {
                library_name: source.library_name,
                details: checksum_error.to_string(),
            }
        })?;
        return Ok(local_archive);
    }

    let cached_archive = layout.downloads_directory.join(source.archive_file_name);
    // Ранее скачанный архив переиспользуем, только если он всё ещё совпадает.
    if cached_archive.is_file() && verify_sha256(&cached_archive, source.sha256).is_ok() {
        return Ok(cached_archive);
    }

    download_archive(source, layout, &cached_archive)?;
    Ok(cached_archive)
}

/// Скачивает архив во временный файл, проверяет и только затем переименовывает.
fn download_archive(
    source: &PinnedSource,
    layout: &VendoredLayout,
    cached_archive: &Path,
) -> Result<(), VendoredBuildError> {
    commands::ensure_tools_available(&[DOWNLOAD_TOOL])?;
    create_directory(&layout.downloads_directory)?;

    let partial_archive = cached_archive.with_extension("partial");
    let mut curl_command = Command::new(DOWNLOAD_TOOL.executable);
    curl_command
        // --fail: HTTP-ошибка не превращается в «архив» с HTML-страницей.
        .arg("--fail")
        .arg("--location")
        .arg("--silent")
        .arg("--show-error")
        // Только HTTPS даже после редиректов.
        .args(["--proto", "=https", "--proto-redir", "=https"])
        .arg("--output")
        .arg(&partial_archive)
        .arg(source.archive_url);
    commands::run_logged(
        curl_command,
        &format!("download-{}", source.slug),
        &layout.logs_directory,
    )
    .map_err(|_| VendoredBuildError::Download {
        library_name: source.library_name,
        archive_url: source.archive_url,
        local_archive_env: source.local_archive_env,
    })?;

    verify_sha256(&partial_archive, source.sha256).map_err(|checksum_error| {
        // Подозрительный файл не оставляем в кеше. Если удалить не вышло,
        // дописываем это к главной ошибке, а не теряем молча.
        let cleanup_note = match fs::remove_file(&partial_archive) {
            Ok(()) => String::new(),
            Err(cleanup_error) => format!(
                "; also failed to remove {}: {cleanup_error}",
                partial_archive.display()
            ),
        };
        VendoredBuildError::Checksum {
            library_name: source.library_name,
            details: format!("{checksum_error}{cleanup_note}"),
        }
    })?;

    fs::rename(&partial_archive, cached_archive).map_err(|io_error| VendoredBuildError::Io {
        action: format!("move verified archive to {}", cached_archive.display()),
        source: io_error,
    })
}

/// Распаковывает проверенный архив заново и возвращает каталог с исходниками.
pub fn unpack_archive(
    source: &PinnedSource,
    archive_path: &Path,
    layout: &VendoredLayout,
) -> Result<PathBuf, VendoredBuildError> {
    let unpacked_directory = layout
        .sources_directory
        .join(source.unpacked_directory_name);
    // Старые распакованные файлы удаляем: сборка не должна видеть остатки прошлой версии.
    if unpacked_directory.exists() {
        fs::remove_dir_all(&unpacked_directory).map_err(|io_error| VendoredBuildError::Io {
            action: format!("remove stale sources {}", unpacked_directory.display()),
            source: io_error,
        })?;
    }
    create_directory(&layout.sources_directory)?;

    let mut tar_command = Command::new("tar");
    tar_command
        .arg("-xf")
        .arg(archive_path)
        .arg("-C")
        .arg(&layout.sources_directory);
    commands::run_logged(
        tar_command,
        &format!("unpack-{}", source.slug),
        &layout.logs_directory,
    )?;

    if unpacked_directory.is_dir() {
        Ok(unpacked_directory)
    } else {
        Err(VendoredBuildError::UnexpectedArchiveLayout {
            library_name: source.library_name,
            expected_directory: unpacked_directory,
        })
    }
}

/// Создаёт каталог вместе с родителями, сохраняя путь в ошибке.
pub fn create_directory(directory: &Path) -> Result<(), VendoredBuildError> {
    fs::create_dir_all(directory).map_err(|io_error| VendoredBuildError::Io {
        action: format!("create directory {}", directory.display()),
        source: io_error,
    })
}
