//! Отключение браузера (`Disconnect`): убирает только своё и только ненужное.
//!
//! Порядок: файл(ы) подключения цели → посредник, если им больше не пользуется ни
//! одна подключённая цель → `.service`, если не осталось ни одной подключённой цели
//! и `Exec=` указывает на ЭТОТ плеер. Чужой файл под нашим именем не удаляется:
//! о нём сообщается предупреждением.

use std::path::{Path, PathBuf};

use desktop_integration::SessionBusReloadError;
use tracing::{info, warn};

use crate::catalog::{BrowserTarget, TARGETS};
use crate::environment::InstallEnvironment;
use crate::error::{InstallError, InstallErrorKind, InstallStep};
use crate::expected::ExpectedInstall;
use crate::inspection::{ManifestState, inspect_manifest, parse_service_player};
use crate::reloader::SessionBusReloader;

/// Предупреждение отключения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisconnectWarning {
    /// Файл под нашим именем не наш (чужой или битый): оставлен как есть.
    NotOurFileLeftInPlace {
        /// Путь файла.
        path: PathBuf,
    },
    /// `.service` будит другую копию плеера: оставлен, чтобы её не сломать.
    ServiceFileBelongsToAnotherCopy {
        /// Бинарь из `Exec=`.
        player: PathBuf,
    },
    /// Путь этого плеера неизвестен, поэтому чья `.service`, определить нельзя: оставлена.
    ServiceFileOwnerUnknown {
        /// Бинарь из `Exec=` службы.
        exec_player: PathBuf,
    },
    /// Шина не перечитала службы после удаления `.service`.
    BusReloadFailed(SessionBusReloadError),
}

/// Что сделало отключение (без статуса: его добавляет фасад).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct DisconnectReport {
    /// Удалённые файлы.
    pub removed_files: Vec<PathBuf>,
    /// Предупреждения.
    pub warnings: Vec<DisconnectWarning>,
    /// Удалённые пустые каталоги, которые создали мы.
    pub removed_directories: Vec<PathBuf>,
}

/// Отключает цель.
pub(crate) fn disconnect_target(
    environment: &InstallEnvironment,
    player: Option<&Path>,
    target: &BrowserTarget,
    reloader: &dyn SessionBusReloader,
) -> Result<DisconnectReport, InstallError> {
    let expected = ExpectedInstall::without_source(environment, target).map_err(|error| {
        InstallError::of(
            InstallStep::CheckTarget,
            InstallErrorKind::Expectation(error),
        )
    })?;
    let mut report = DisconnectReport::default();

    remove_own_manifests(&expected, &mut report)?;
    if !host_is_used_by_another_target(environment, target, &expected) {
        remove_if_exists(&expected.host_path, InstallStep::RemoveHost, &mut report)?;
        remove_own_empty_directories(&expected.host_path, &mut report);
    }
    if !any_target_connected(environment) {
        remove_service_file(&expected, player, reloader, &mut report)?;
    }
    info!(target_id = target.id, "Browser bridge disconnected");
    Ok(report)
}

/// Удаляет наши файлы подключения; чужие и битые не трогает.
fn remove_own_manifests(
    expected: &ExpectedInstall,
    report: &mut DisconnectReport,
) -> Result<(), InstallError> {
    for path in &expected.manifest_paths {
        match inspect_manifest(path, expected) {
            ManifestState::Identical | ManifestState::OursButDiffers => {
                remove_if_exists(path, InstallStep::RemoveManifest, report)?;
            }
            ManifestState::Foreign | ManifestState::Damaged => {
                warn!("Leaving a connection file that is not ours");
                report
                    .warnings
                    .push(DisconnectWarning::NotOurFileLeftInPlace { path: path.clone() });
            }
            ManifestState::Unreadable { kind } => {
                return Err(InstallError::io(
                    InstallStep::RemoveManifest,
                    path,
                    &std::io::Error::from(kind),
                ));
            }
            ManifestState::Absent => {}
        }
    }
    Ok(())
}

/// Удаляет `.service`, если его `Exec=` указывает на ЭТОТ плеер, и просит шину
/// перечитать службы. Решение принимается по пути из самого файла, а не по пересборке
/// ожидаемого текста: так оно работает и когда путь плеера нельзя записать в `Exec=`.
fn remove_service_file(
    expected: &ExpectedInstall,
    player: Option<&Path>,
    reloader: &dyn SessionBusReloader,
    report: &mut DisconnectReport,
) -> Result<(), InstallError> {
    let service_text = match std::fs::read_to_string(&expected.service_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => String::new(),
        Err(error) => {
            return Err(InstallError::io(
                InstallStep::RemoveServiceFile,
                &expected.service_path,
                &error,
            ));
        }
    };
    let not_ours = DisconnectWarning::NotOurFileLeftInPlace {
        path: expected.service_path.clone(),
    };
    match (parse_service_player(&service_text), player) {
        (Some(exec_player), Some(this_player)) if exec_player == this_player => {
            remove_if_exists(
                &expected.service_path,
                InstallStep::RemoveServiceFile,
                report,
            )?;
            if let Err(error) = reloader.reload_service_files() {
                warn!(%error, "The session bus did not reload service files");
                report
                    .warnings
                    .push(DisconnectWarning::BusReloadFailed(error));
            }
        }
        (Some(exec_player), None) => report
            .warnings
            .push(DisconnectWarning::ServiceFileOwnerUnknown { exec_player }),
        (Some(exec_player), Some(_)) => {
            report
                .warnings
                .push(DisconnectWarning::ServiceFileBelongsToAnotherCopy {
                    player: exec_player,
                })
        }
        (None, _) => report.warnings.push(not_ours),
    }
    Ok(())
}

/// Убирает каталоги `browser-host/` и `fastiplayer/`, если они пусты. Только их: они
/// созданы нами; каталоги браузеров и `dbus-1/services` не трогаются никогда.
///
/// Уборка строго «по возможности»: симлинк (stow, dotfiles) не удаляется никогда, а
/// любая неудача просто останавливает подъём. Она не вправе сорвать отключение.
fn remove_own_empty_directories(host_path: &Path, report: &mut DisconnectReport) {
    let host_directory = host_path.parent();
    let application_directory = host_directory.and_then(Path::parent);
    for directory in [host_directory, application_directory]
        .into_iter()
        .flatten()
    {
        let is_real_directory = std::fs::symlink_metadata(directory)
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false);
        if !is_real_directory || std::fs::remove_dir(directory).is_err() {
            // Симлинк, чужое содержимое, нет прав: оставляем и выше не идём.
            break;
        }
        report.removed_directories.push(directory.to_path_buf());
    }
}

/// Пользуется ли тем же файлом посредника другая подключённая цель.
fn host_is_used_by_another_target(
    environment: &InstallEnvironment,
    target: &BrowserTarget,
    expected: &ExpectedInstall,
) -> bool {
    TARGETS
        .iter()
        .filter(|other| other.id != target.id)
        .filter_map(|other| ExpectedInstall::without_source(environment, other).ok())
        .filter(|other_expected| other_expected.host_path == expected.host_path)
        .any(|other_expected| has_own_manifest(&other_expected))
}

/// Подключена ли хоть одна цель каталога (по наличию нашего файла подключения).
fn any_target_connected(environment: &InstallEnvironment) -> bool {
    TARGETS
        .iter()
        .filter_map(|target| ExpectedInstall::without_source(environment, target).ok())
        .any(|expected| has_own_manifest(&expected))
}

/// Есть ли среди файлов подключения хотя бы один наш.
fn has_own_manifest(expected: &ExpectedInstall) -> bool {
    expected
        .manifest_paths
        .iter()
        .any(|path| inspect_manifest(path, expected).is_ours())
}

/// Удаляет файл; его отсутствие — не ошибка (отключение идемпотентно).
fn remove_if_exists(
    path: &Path,
    step: InstallStep,
    report: &mut DisconnectReport,
) -> Result<(), InstallError> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            report.removed_files.push(path.to_path_buf());
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(InstallError::io(step, path, &error)),
    }
}
