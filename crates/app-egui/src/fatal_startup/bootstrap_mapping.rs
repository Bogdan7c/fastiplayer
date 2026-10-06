//! Перевод ошибок process bootstrap (аргументы, папка настроек, lease, config)
//! в причину фатальной ошибки запуска.
//!
//! Типы ошибок lease остаются у своего владельца (`app_instance`) и сохраняют
//! техническую точность для лога; здесь решается только, что сказать человеку.

use std::io::ErrorKind;

use fastiplayer_config::ConfigPaths;

use crate::app_instance::{
    AppInstanceLeaseError, ProcessBootstrapError, UnsafeAppInstanceArtifact,
};

use super::{
    ConfigDirectoryLocation, ConfigDirectoryProblem, FatalStartupError, FatalStartupReason,
};

impl FatalStartupError {
    /// Bootstrap не дошёл до окна: аргументы, папка настроек, lease или config.
    pub(crate) fn from_bootstrap_error(error: &ProcessBootstrapError) -> Self {
        let reason = match error {
            ProcessBootstrapError::Arguments(arguments_error) => {
                FatalStartupReason::InvalidArguments(*arguments_error)
            }
            ProcessBootstrapError::DiscoverPaths(_) => FatalStartupReason::ConfigLocationUnknown,
            ProcessBootstrapError::Lease {
                lease_error,
                config_dir,
            } => lease_failure_reason(*lease_error, &ConfigPaths::from_config_dir(config_dir)),
            ProcessBootstrapError::LoadConfig { config_dir, .. } => {
                FatalStartupReason::ConfigFileUnusable {
                    location: ConfigDirectoryLocation::for_current_user(config_dir),
                }
            }
        };
        Self::new(reason, error.to_string())
    }
}

fn lease_failure_reason(
    lease_error: AppInstanceLeaseError,
    config_paths: &ConfigPaths,
) -> FatalStartupReason {
    let problem = match lease_error {
        AppInstanceLeaseError::AlreadyRunning => return FatalStartupReason::AlreadyRunning,
        AppInstanceLeaseError::UnsupportedPlatform => {
            return FatalStartupReason::UnsupportedPlatform;
        }
        AppInstanceLeaseError::UnsafeArtifact { reason } => {
            unsafe_artifact_problem(reason, config_paths)
        }
        AppInstanceLeaseError::Io { kind, .. } => io_problem(kind),
    };
    FatalStartupReason::ConfigDirectoryUnusable {
        problem,
        location: ConfigDirectoryLocation::for_current_user(config_paths.config_dir()),
    }
}

/// Небезопасный файл/папка: владелец, тип или подмена во время проверки.
fn unsafe_artifact_problem(
    reason: UnsafeAppInstanceArtifact,
    config_paths: &ConfigPaths,
) -> ConfigDirectoryProblem {
    match reason {
        // Чужой владелец у папки или у файла блокировки лечится одной и той же
        // рекурсивной сменой владельца папки.
        UnsafeAppInstanceArtifact::ConfigDirectoryOwnerMismatch
        | UnsafeAppInstanceArtifact::LockArtifactOwnerMismatch => {
            ConfigDirectoryProblem::OwnedByAnotherUser
        }
        UnsafeAppInstanceArtifact::ConfigDirectoryIsNotDirectory => {
            ConfigDirectoryProblem::NotADirectory
        }
        UnsafeAppInstanceArtifact::LockArtifactIsNotRegularFile => {
            ConfigDirectoryProblem::LockFileNotRegularFile {
                lock_file_name: lock_file_name(config_paths),
            }
        }
        UnsafeAppInstanceArtifact::LockArtifactIdentityChanged => {
            ConfigDirectoryProblem::LockFileReplacedDuringStartup
        }
    }
}

/// Системная ошибка ввода-вывода: различаем только то, что пользователь может исправить.
fn io_problem(kind: ErrorKind) -> ConfigDirectoryProblem {
    match kind {
        ErrorKind::PermissionDenied => ConfigDirectoryProblem::AccessDenied,
        ErrorKind::ReadOnlyFilesystem => ConfigDirectoryProblem::ReadOnlyStorage,
        ErrorKind::StorageFull | ErrorKind::QuotaExceeded => ConfigDirectoryProblem::StorageFull,
        _ => ConfigDirectoryProblem::SystemError,
    }
}

/// Имя файла блокировки без пути — его пользователь должен удалить сам.
fn lock_file_name(config_paths: &ConfigPaths) -> String {
    config_paths
        .app_instance_lock_file()
        .file_name()
        .map(|file_name| file_name.to_string_lossy().into_owned())
        // Путь всегда строится как `config_dir.join(имя)`, так что имя есть;
        // запасной текст лишь не даёт сообщению остаться с пустым местом.
        .unwrap_or_else(|| "блокировки".to_owned())
}
