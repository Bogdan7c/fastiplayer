//! Обзор браузеров и статус подключения каждого.
//!
//! Статус — это сравнение «что записал бы этот плеер» ([`ExpectedInstall`]) с тем,
//! что лежит на диске (модуль `inspection`). Ничего не запоминается: удалили файл
//! руками — статус честно изменился.

use std::io;
use std::ops::Not;
use std::path::PathBuf;

use crate::catalog::{BrowserTarget, BrowserTargetId, Packaging, Verification};
use crate::detection::Installation;
use crate::environment::InstallEnvironment;
use crate::expected::{ExpectationError, ExpectedInstall};
use crate::flatpak::{FlatpakError, FlatpakTool, parse_bus_access};
use crate::inspection::{
    HostState, ManifestState, ServiceState, find_directory_problem, inspect_host, inspect_manifest,
    inspect_service, parse_service_player,
};
use crate::source::{InstallSource, SourceProblem};

/// Статус подключения одного браузера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserBridgeStatus {
    /// Всё на месте и совпадает с тем, что записал бы этот плеер.
    Connected,
    /// Файлы наши, но `.service` будит другую копию плеера (dev и установленная сборка).
    ConnectedToAnotherCopy {
        /// Бинарь плеера из `Exec=`.
        player: PathBuf,
        /// Существует ли ещё тот бинарь (нет — копию удалили, нужен Reconnect).
        player_exists: bool,
    },
    /// Нашего файла подключения нет.
    NotConnected,
    /// Файлы наши, но устарели: другая версия, посредник не исполняемый, служба пропала.
    Outdated,
    /// Flatpak-браузер: файлы на месте, но песочница не может позвонить на шину.
    NeedsFlatpakPermission,
    /// Под нашим именем лежит чужой или битый файл; Connect перезапишет его с предупреждением.
    ForeignFile,
    /// Flatpak-Firefox пока не поддерживается.
    FlatpakUnsupported,
    /// Snap-браузер не поддерживается.
    SnapUnsupported,
    /// Статус определить нельзя (понятная причина, не паника).
    Unavailable(UnavailableReason),
}

/// Почему статус определить нельзя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnavailableReason {
    /// Эта копия плеера не может быть источником установки.
    SourceProblem(SourceProblem),
    /// Ожидаемые файлы построить нельзя.
    Expectation(ExpectationError),
    /// Каталог или файл недоступен (висячий symlink, файл вместо каталога, нет прав чтения).
    Filesystem {
        /// Проблемный путь.
        path: PathBuf,
        /// Класс ошибки ОС.
        kind: io::ErrorKind,
    },
    /// Права Flatpak-приложения не удалось прочитать.
    FlatpakPermissions(FlatpakError),
}

/// Пакет или Flatpak (подсказка интерфейсу).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackagingKind {
    /// Системный пакет или распаковка.
    Package,
    /// Flatpak-приложение.
    Flatpak,
}

/// Строка обзора: один найденный браузер.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserReport {
    /// ID цели для действий.
    pub target: BrowserTargetId,
    /// Имя для интерфейса.
    pub display_name: &'static str,
    /// Проверена ли цель вживую.
    pub verification: Verification,
    /// Пакет или Flatpak.
    pub packaging: PackagingKind,
    /// Статус.
    pub status: BrowserBridgeStatus,
}

/// Результат обзора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSurvey {
    /// Годится ли эта копия плеера как источник установки.
    pub source: Result<(), SourceProblem>,
    /// Установленные браузеры (не установленные в обзор не попадают).
    pub browsers: Vec<BrowserReport>,
    /// Сбой утилиты `flatpak` (Flatpak-цели при этом не показываются).
    pub flatpak_problem: Option<FlatpakError>,
}

/// Почему обзор невозможен.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SurveyError {
    /// Установщик реализован только для Linux (на Windows будет реестр).
    #[error("Browser integration is not supported on this platform yet")]
    UnsupportedPlatform,
}

/// Всё, что нужно для вычисления статуса.
pub(crate) struct StatusInputs<'a> {
    pub environment: &'a InstallEnvironment,
    pub source: &'a Result<InstallSource, SourceProblem>,
    pub flatpak: &'a dyn FlatpakTool,
}

/// Вид упаковки цели для отчёта.
pub(crate) fn packaging_kind(target: &BrowserTarget) -> PackagingKind {
    match target.packaging {
        Packaging::System { .. } => PackagingKind::Package,
        Packaging::Flatpak { .. } => PackagingKind::Flatpak,
    }
}

/// Статус установленной цели.
pub(crate) fn target_status(
    inputs: &StatusInputs<'_>,
    target: &BrowserTarget,
    installation: &Installation,
) -> BrowserBridgeStatus {
    match installation {
        Installation::Snap { .. } => return BrowserBridgeStatus::SnapUnsupported,
        Installation::Flatpak if target.is_flatpak_firefox_family() => {
            return BrowserBridgeStatus::FlatpakUnsupported;
        }
        Installation::Absent | Installation::System { .. } | Installation::Flatpak => {}
    }
    // Без годного источника статус всё равно строится по диску: видеть и убирать своё
    // можно всегда, заблокировано только подключение (проблема источника — в обзоре).
    let (expected, source_known) = match inputs.source {
        Ok(source) => (
            ExpectedInstall::build(inputs.environment, source, target),
            true,
        ),
        Err(_problem) => (
            ExpectedInstall::without_source(inputs.environment, target),
            false,
        ),
    };
    let expected = match expected {
        Ok(expected) => expected,
        Err(error) => return unavailable(UnavailableReason::Expectation(error)),
    };
    if let Some(reason) = filesystem_problem(&expected) {
        return unavailable(reason);
    }
    if let Some(status) = manifest_verdict(&expected) {
        return status;
    }
    let player_hint = match inputs.source {
        Ok(source) => Some(source.player_executable()),
        Err(problem) => problem.player_executable(),
    };
    let degraded_verdicts = if source_known {
        None
    } else {
        Some((
            degraded_service_verdict(&expected, player_hint),
            degraded_host_verdict(&expected),
        ))
    };
    let (service_status, host_status) = match degraded_verdicts {
        Some(verdicts) => verdicts,
        None => (service_verdict(&expected), host_verdict(&expected)),
    };
    if let Some(status) = service_status.or(host_status) {
        return status;
    }
    permission_verdict(inputs.flatpak, target)
}

fn unavailable(reason: UnavailableReason) -> BrowserBridgeStatus {
    BrowserBridgeStatus::Unavailable(reason)
}

/// Висячие ссылки и файлы вместо каталогов на путях назначения.
fn filesystem_problem(expected: &ExpectedInstall) -> Option<UnavailableReason> {
    let directories = expected
        .manifest_paths
        .iter()
        .chain([&expected.host_path, &expected.service_path])
        .filter_map(|path| path.parent());
    for directory in directories {
        if let Some(problem) = find_directory_problem(directory) {
            return Some(UnavailableReason::Filesystem {
                path: problem.path,
                kind: problem.kind,
            });
        }
    }
    None
}

/// Вердикт по файлам подключения; `None` — все на месте и совпадают.
fn manifest_verdict(expected: &ExpectedInstall) -> Option<BrowserBridgeStatus> {
    let states: Vec<(&PathBuf, ManifestState)> = expected
        .manifest_paths
        .iter()
        .map(|path| (path, inspect_manifest(path, expected)))
        .collect();
    if let Some((path, ManifestState::Unreadable { kind })) = states
        .iter()
        .find(|(_, state)| matches!(state, ManifestState::Unreadable { .. }))
    {
        return Some(unavailable(UnavailableReason::Filesystem {
            path: (*path).clone(),
            kind: *kind,
        }));
    }
    if states
        .iter()
        .any(|(_, state)| matches!(state, ManifestState::Foreign | ManifestState::Damaged))
    {
        return Some(BrowserBridgeStatus::ForeignFile);
    }
    if states
        .iter()
        .all(|(_, state)| *state == ManifestState::Absent)
    {
        return Some(BrowserBridgeStatus::NotConnected);
    }
    // Часть файлов нет (Firefox: один из двух путей) или версия другая.
    let all_identical = states
        .iter()
        .all(|(_, state)| *state == ManifestState::Identical);
    (!all_identical).then_some(BrowserBridgeStatus::Outdated)
}

/// Вердикт по `.service`; `None` — служба в порядке.
fn service_verdict(expected: &ExpectedInstall) -> Option<BrowserBridgeStatus> {
    match inspect_service(expected) {
        ServiceState::Identical => None,
        ServiceState::AnotherCopy { player } => {
            let player_exists = player.exists();
            Some(BrowserBridgeStatus::ConnectedToAnotherCopy {
                player,
                player_exists,
            })
        }
        ServiceState::Missing | ServiceState::Unrecognized => Some(BrowserBridgeStatus::Outdated),
        ServiceState::Unreadable { kind } => Some(unavailable(UnavailableReason::Filesystem {
            path: expected.service_path.clone(),
            kind,
        })),
    }
}

/// Служба без источника: сравнить текст нельзя, поэтому смотрим на путь из `Exec=`.
fn degraded_service_verdict(
    expected: &ExpectedInstall,
    player_hint: Option<&std::path::Path>,
) -> Option<BrowserBridgeStatus> {
    let text = match std::fs::read_to_string(&expected.service_path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Some(BrowserBridgeStatus::Outdated);
        }
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            return Some(BrowserBridgeStatus::Outdated);
        }
        Err(error) => {
            return Some(unavailable(UnavailableReason::Filesystem {
                path: expected.service_path.clone(),
                kind: error.kind(),
            }));
        }
    };
    match (parse_service_player(&text), player_hint) {
        (None, _) => Some(BrowserBridgeStatus::Outdated),
        // Путь плеера неизвестен: нечего сравнивать, считаем службу в порядке.
        (Some(_), None) => None,
        (Some(exec_player), Some(this_player)) if exec_player == this_player => None,
        (Some(exec_player), Some(_)) => {
            let player_exists = exec_player.exists();
            Some(BrowserBridgeStatus::ConnectedToAnotherCopy {
                player: exec_player,
                player_exists,
            })
        }
    }
}

/// Посредник без источника: байты сравнить не с чем, проверяем наличие и бит исполнения.
fn degraded_host_verdict(expected: &ExpectedInstall) -> Option<BrowserBridgeStatus> {
    crate::fs_util::is_executable_file(&expected.host_path)
        .not()
        .then_some(BrowserBridgeStatus::Outdated)
}

/// Вердикт по копии посредника; `None` — копия в порядке.
fn host_verdict(expected: &ExpectedInstall) -> Option<BrowserBridgeStatus> {
    match inspect_host(expected) {
        HostState::Identical => None,
        HostState::Missing | HostState::BytesDiffer | HostState::NotExecutable => {
            Some(BrowserBridgeStatus::Outdated)
        }
        HostState::Unreadable { kind } => Some(unavailable(UnavailableReason::Filesystem {
            path: expected.host_path.clone(),
            kind,
        })),
    }
}

/// Последний шаг: у Flatpak-песочницы должен быть доступ к имени на шине.
pub(crate) fn permission_verdict(
    flatpak: &dyn FlatpakTool,
    target: &BrowserTarget,
) -> BrowserBridgeStatus {
    let Some(app_id) = target.flatpak_app_id() else {
        return BrowserBridgeStatus::Connected;
    };
    match flatpak.permissions(app_id) {
        Ok(text) if parse_bus_access(&text).allows_bridge() => BrowserBridgeStatus::Connected,
        Ok(_text) => BrowserBridgeStatus::NeedsFlatpakPermission,
        Err(error) => unavailable(UnavailableReason::FlatpakPermissions(error)),
    }
}
