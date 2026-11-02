//! Установлен ли браузер: исполняемый файл в `PATH` или Flatpak-приложение.
//!
//! Одна лишь папка профиля НЕ признак: у пользователей полно пустых папок от давно
//! удалённых браузеров, и показывать их как установленные было бы враньём.

use std::path::{Path, PathBuf};

use crate::catalog::{BrowserTarget, Packaging};
use crate::environment::InstallEnvironment;
use crate::fs_util::is_executable_file;

/// Предел шагов по цепочке symlink-ов (защита от петли).
const MAX_SYMLINK_HOPS: usize = 16;

/// Что нашлось для цели.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installation {
    /// Браузера нет.
    Absent,
    /// Обычная установка: исполняемый файл найден.
    System {
        /// Найденный исполняемый файл.
        executable: PathBuf,
    },
    /// Snap: найденный файл лежит в каталоге snapd или ведёт туда по symlink.
    Snap {
        /// Найденный файл.
        executable: PathBuf,
    },
    /// Flatpak-приложение установлено.
    Flatpak,
}

/// Определяет, как установлена цель.
///
/// `flatpak_apps` — app ID из `flatpak list`; для пакетных целей не используется.
pub fn detect_installation(
    target: &BrowserTarget,
    environment: &InstallEnvironment,
    flatpak_apps: &[String],
) -> Installation {
    match target.packaging {
        Packaging::Flatpak { app_id } => {
            if flatpak_apps.iter().any(|installed| installed == app_id) {
                Installation::Flatpak
            } else {
                Installation::Absent
            }
        }
        Packaging::System { executables } => detect_system(executables, environment),
    }
}

/// Ищет первый подходящий исполняемый файл: сначала `PATH`, затем каталоги snapd
/// (snap-браузер может не быть в `PATH` сеанса).
fn detect_system(executables: &[&str], environment: &InstallEnvironment) -> Installation {
    let search_dirs = environment
        .executable_search_dirs
        .iter()
        .chain(environment.snap_bin_dirs.iter());
    for directory in search_dirs {
        for name in executables {
            let candidate = directory.join(name);
            if !is_executable_file(&candidate) {
                continue;
            }
            return if path_passes_through_snap(&candidate, environment) {
                Installation::Snap {
                    executable: candidate,
                }
            } else {
                Installation::System {
                    executable: candidate,
                }
            };
        }
    }
    Installation::Absent
}

/// Лежит ли файл в каталоге snapd или любое звено цепочки его symlink-ов.
///
/// `/snap/bin/chromium` обычно symlink на `/usr/bin/snap`, поэтому одной
/// канонизации мало: смотрим каждое промежуточное звено.
fn path_passes_through_snap(start: &Path, environment: &InstallEnvironment) -> bool {
    let mut current = start.to_path_buf();
    for _ in 0..MAX_SYMLINK_HOPS {
        if environment.is_snap_path(&current) {
            return true;
        }
        let Ok(link_target) = std::fs::read_link(&current) else {
            return false;
        };
        current = if link_target.is_absolute() {
            link_target
        } else {
            current
                .parent()
                .map(|parent| parent.join(&link_target))
                .unwrap_or(link_target)
        };
    }
    false
}
