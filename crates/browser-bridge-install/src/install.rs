//! Подключение браузера (`Connect`): копирование посредника, служба, файл подключения.
//!
//! Порядок «выключатель последним»: браузер видит мост, только когда существует файл
//! подключения, а он пишется ПОСЛЕДНИМ. Если что-то упало раньше, браузер ничего не
//! заметил, а повторный Connect доделает остальное. Каталоги создаются до записи
//! любых файлов, чтобы типичная беда (нельзя писать в конфиг) не оставляла мусора.
//!
//! Повторный Connect не плодит дубликатов: пути те же, файлы заменяются атомарно.

use std::path::Path;

use atomic_file_store::{AtomicFileWriteOutcome, replace_file_atomically};
use tracing::{info, warn};

use crate::catalog::BrowserTarget;
use crate::detection::Installation;
use crate::environment::InstallEnvironment;
use crate::error::{
    InstallError, InstallErrorKind, InstallStep, InstallWarning, UnsupportedReason,
};
use crate::expected::ExpectedInstall;
use crate::fs_util::{EXECUTABLE_MODE, REGULAR_FILE_MODE, set_file_mode};
use crate::host_copy::{HostReplaceOutcome, replace_with_mode};
use crate::inspection::{ManifestState, ServiceState, inspect_manifest, inspect_service};
use crate::reloader::SessionBusReloader;
use crate::source::InstallSource;

/// Подключает цель. Возвращает предупреждения; ошибка = остановка на шаге.
pub(crate) fn connect_target(
    environment: &InstallEnvironment,
    source: &InstallSource,
    target: &BrowserTarget,
    installation: &Installation,
    reloader: &dyn SessionBusReloader,
) -> Result<Vec<InstallWarning>, InstallError> {
    ensure_target_is_connectable(target, installation)?;
    let expected = ExpectedInstall::build(environment, source, target).map_err(|error| {
        InstallError::of(
            InstallStep::CheckTarget,
            InstallErrorKind::Expectation(error),
        )
    })?;
    let mut warnings = Vec::new();

    prepare_directories(&expected)?;
    collect_manifest_warnings(&expected, &mut warnings)?;
    if let ServiceState::AnotherCopy { player } = inspect_service(&expected) {
        warnings.push(InstallWarning::TookOverFromAnotherCopy {
            previous_player: player,
        });
    }

    copy_host(&expected, &mut warnings)?;
    write_service_file(&expected, reloader, &mut warnings)?;
    write_manifests(&expected, &mut warnings)?;
    info!(target_id = target.id, "Browser bridge connected");
    Ok(warnings)
}

/// Можно ли вообще подключать цель в найденном состоянии.
fn ensure_target_is_connectable(
    target: &BrowserTarget,
    installation: &Installation,
) -> Result<(), InstallError> {
    let refusal = match installation {
        Installation::Absent => InstallErrorKind::TargetNotInstalled,
        Installation::Snap { .. } => InstallErrorKind::Unsupported(UnsupportedReason::Snap),
        Installation::Flatpak if target.is_flatpak_firefox_family() => {
            InstallErrorKind::Unsupported(UnsupportedReason::FlatpakFirefoxFamily)
        }
        Installation::System { .. } | Installation::Flatpak => return Ok(()),
    };
    Err(InstallError::of(InstallStep::CheckTarget, refusal))
}

/// Создаёт каталоги всех назначений (отсутствие профиля браузера — норма).
fn prepare_directories(expected: &ExpectedInstall) -> Result<(), InstallError> {
    let manifest_directories = expected
        .manifest_paths
        .iter()
        .filter_map(|path| path.parent());
    let other_directories = [&expected.host_path, &expected.service_path]
        .into_iter()
        .filter_map(|path| path.parent());
    for directory in manifest_directories.chain(other_directories) {
        std::fs::create_dir_all(directory).map_err(|error| {
            InstallError::io(InstallStep::PrepareDirectories, directory, &error)
        })?;
    }
    Ok(())
}

/// Находит чужие и битые файлы подключения, которые будут перезаписаны.
fn collect_manifest_warnings(
    expected: &ExpectedInstall,
    warnings: &mut Vec<InstallWarning>,
) -> Result<(), InstallError> {
    for path in &expected.manifest_paths {
        match inspect_manifest(path, expected) {
            ManifestState::Foreign => {
                warn!("Overwriting a foreign connection file");
                warnings.push(InstallWarning::OverwroteForeignFile { path: path.clone() });
            }
            ManifestState::Damaged => {
                warn!("Replacing a damaged connection file");
                warnings.push(InstallWarning::ReplacedDamagedFile { path: path.clone() });
            }
            ManifestState::Unreadable { kind } => {
                return Err(InstallError::io(
                    InstallStep::InspectManifest,
                    path,
                    &std::io::Error::from(kind),
                ));
            }
            ManifestState::Absent | ManifestState::Identical | ManifestState::OursButDiffers => {}
        }
    }
    Ok(())
}

/// Копирует посредника (байты источника, права 0755).
fn copy_host(
    expected: &ExpectedInstall,
    warnings: &mut Vec<InstallWarning>,
) -> Result<(), InstallError> {
    let host_bytes = std::fs::read(&expected.host_source)
        .map_err(|error| InstallError::io(InstallStep::CopyHost, &expected.host_source, &error))?;
    let outcome = replace_with_mode(
        &expected.host_path,
        &host_bytes,
        EXECUTABLE_MODE,
        set_file_mode,
    )
    .map_err(|error| InstallError::io(InstallStep::CopyHost, &expected.host_path, &error))?;
    if outcome == HostReplaceOutcome::DurabilityUnconfirmed {
        warnings.push(InstallWarning::DurabilityUnconfirmed {
            path: expected.host_path.clone(),
        });
    }
    Ok(())
}

/// Пишет `.service` и просит шину перечитать службы.
fn write_service_file(
    expected: &ExpectedInstall,
    reloader: &dyn SessionBusReloader,
    warnings: &mut Vec<InstallWarning>,
) -> Result<(), InstallError> {
    write_file(
        &expected.service_path,
        expected.service_text.as_bytes(),
        REGULAR_FILE_MODE,
        InstallStep::WriteServiceFile,
        warnings,
    )?;
    if let Err(error) = reloader.reload_service_files() {
        warn!(%error, "The session bus did not reload service files");
        warnings.push(InstallWarning::BusReloadFailed(error));
    }
    Ok(())
}

/// Пишет файл(ы) подключения: «выключатель» моста.
fn write_manifests(
    expected: &ExpectedInstall,
    warnings: &mut Vec<InstallWarning>,
) -> Result<(), InstallError> {
    for path in &expected.manifest_paths {
        write_file(
            path,
            expected.manifest_text.as_bytes(),
            REGULAR_FILE_MODE,
            InstallStep::WriteManifest,
            warnings,
        )?;
    }
    Ok(())
}

/// Атомарная запись с правами; неподтверждённая сохранность — предупреждение.
fn write_file(
    path: &Path,
    contents: &[u8],
    mode: u32,
    step: InstallStep,
    warnings: &mut Vec<InstallWarning>,
) -> Result<(), InstallError> {
    match replace_file_atomically(path, contents) {
        AtomicFileWriteOutcome::NotReplaced(failure) => {
            return Err(InstallError::of(
                step,
                InstallErrorKind::AtomicWrite {
                    path: path.to_path_buf(),
                    failure,
                },
            ));
        }
        AtomicFileWriteOutcome::ReplacedDurabilityUnconfirmed(_sync_error) => {
            warnings.push(InstallWarning::DurabilityUnconfirmed {
                path: path.to_path_buf(),
            });
        }
        AtomicFileWriteOutcome::Durable => {}
    }
    set_file_mode(path, mode).map_err(|error| InstallError::io(step, path, &error))
}
