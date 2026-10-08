//! Process bootstrap и platform-neutral владение единственным экземпляром приложения.
//!
//! Модуль сохраняет строгий порядок побочных эффектов: сначала разбираются аргументы,
//! затем определяются platform paths, после чего lease берётся до чтения config и любой
//! подготовки media. Linux-specific descriptor details изолированы в `linux`.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;

use fastiplayer_config::{ConfigError, ConfigPaths, LoadedConfig};
use thiserror::Error;

use crate::startup_media::{InitialMedia, resolve_initial_media_arguments};

mod forwarding;
#[cfg(target_os = "linux")]
mod linux;

pub(crate) use forwarding::{
    ForwardedPayload, RunningInstanceForwardingError, RunningInstanceForwardingFailure,
};
// Сквозной тест «второй запуск → первый» живёт у получателя (`instance_forwarding`).
#[cfg(test)]
pub(crate) use forwarding::{
    ForwardingEnvironment, RunningInstanceForwarder, forward_arguments_to_running_instance,
};

/// Уже разобранные process arguments без lossy UTF-8 преобразования media path.
///
/// Грамматика: ноль или больше позиционных media-аргументов; option-подобная строка
/// до `--` — ошибка `UnknownOption`, после `--` любой аргумент считается media.
/// Несколько аргументов — обычный случай «Открыть с помощью» на нескольких выделенных
/// файлах в файловом менеджере (решение владельца, сессия 13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessArgs {
    /// Media positionals в исходном порядке, байт-в-байт как их передала ОС.
    initial_media_arguments: Vec<OsString>,
}

impl ProcessArgs {
    /// Разбирает пользовательские аргументы без имени исполняемого файла.
    pub(crate) fn parse(
        arguments: impl IntoIterator<Item = OsString>,
    ) -> Result<Self, ProcessArgsError> {
        let mut initial_media_arguments = Vec::new();
        let mut options_ended = false;

        for argument in arguments {
            if !options_ended && argument == OsStr::new("--") {
                options_ended = true;
                continue;
            }

            if !options_ended && os_string_starts_with_dash(&argument) {
                return Err(ProcessArgsError::UnknownOption);
            }

            initial_media_arguments.push(argument);
        }

        Ok(Self {
            initial_media_arguments,
        })
    }

    /// Сырые media-аргументы в исходном порядке без перекодировки (только чтение).
    ///
    /// Нужны пересылке запроса уже запущенному экземпляру: туда уходят ровно те же
    /// аргументы, что получил процесс, а классифицирует их получатель.
    pub(crate) fn initial_media_arguments(&self) -> &[OsString] {
        &self.initial_media_arguments
    }

    /// Передаёт media-аргументы следующему bootstrap-этапу без копирования или перекодировки.
    ///
    /// После вызова в `ProcessArgs` аргументов не остаётся: классификация владеет ими сама.
    pub(crate) fn take_initial_media_arguments(&mut self) -> Vec<OsString> {
        std::mem::take(&mut self.initial_media_arguments)
    }
}

/// Typed CLI errors не включают потенциально секретное значение аргумента.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum ProcessArgsError {
    /// До `--` встретилась неизвестная option-подобная строка.
    #[error("неизвестная опция; локальный путь с ведущим '-' передавайте после '--'")]
    UnknownOption,
}

/// Проверяет первый native code unit без преобразования всего пути в UTF-8.
fn os_string_starts_with_dash(argument: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        argument.as_bytes().first() == Some(&b'-')
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        argument.encode_wide().next() == Some(u16::from(b'-'))
    }

    #[cfg(not(any(unix, windows)))]
    {
        argument
            .to_str()
            .is_some_and(|value| value.starts_with('-'))
    }
}

/// Platform-neutral process lease; concrete descriptor остаётся внутри adapter guard.
pub(crate) struct AppInstanceLease {
    _guard: Box<dyn AppInstanceLeaseGuard>,
}

impl fmt::Debug for AppInstanceLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppInstanceLease")
            .finish_non_exhaustive()
    }
}

impl AppInstanceLease {
    /// Принимает opaque platform guard и тем самым не выпускает OS types наружу.
    fn from_guard(guard: impl AppInstanceLeaseGuard + 'static) -> Self {
        Self {
            _guard: Box::new(guard),
        }
    }
}

/// Marker для конкретного ресурса, чей Drop освобождает lease.
trait AppInstanceLeaseGuard: Send {}

impl<T: Send> AppInstanceLeaseGuard for T {}

/// Crate-private intent adapter для Linux-v1 и будущих платформ.
pub(crate) trait AppInstanceLeasePlatform {
    /// Берёт lease по путям, принадлежащим одному trusted `ConfigPaths` owner-у.
    fn acquire(&self, paths: &ConfigPaths) -> Result<AppInstanceLease, AppInstanceLeaseError>;
}

/// Этап I/O, который завершился до получения или проверки lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppInstanceLeaseIoOperation {
    CreateConfigDirectory,
    InspectConfigDirectory,
    HardenConfigDirectory,
    InspectLockArtifact,
    OpenLockArtifact,
    InspectLockDescriptor,
    HardenLockArtifact,
    SetCloseOnExec,
    AcquireLock,
    RevalidateLockIdentity,
}

/// Причина отказа от небезопасного filesystem artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnsafeAppInstanceArtifact {
    ConfigDirectoryIsNotDirectory,
    ConfigDirectoryOwnerMismatch,
    LockArtifactIsNotRegularFile,
    LockArtifactOwnerMismatch,
    LockArtifactIdentityChanged,
}

/// Ошибки lease сохраняют различие contention, I/O, unsafe artifact и platform support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum AppInstanceLeaseError {
    /// Другой процесс уже удерживает stable lock inode.
    #[error("другой экземпляр fastiplayer уже запущен")]
    AlreadyRunning,

    /// Безопасная операция завершилась системной I/O ошибкой.
    #[error("не удалось получить instance lease на этапе {operation:?}: {kind:?}")]
    Io {
        operation: AppInstanceLeaseIoOperation,
        kind: std::io::ErrorKind,
    },

    /// Artifact не соответствует обязательному типу, owner-у или identity.
    #[error("небезопасный instance-lock artifact: {reason:?}")]
    UnsafeArtifact { reason: UnsafeAppInstanceArtifact },

    /// Эта сборка пока не имеет adapter-а с тем же строгим contract.
    #[cfg_attr(
        target_os = "linux",
        allow(
            dead_code,
            reason = "Linux build keeps the shared cross-platform error vocabulary"
        )
    )]
    #[error("single-instance lease не поддерживается на этой платформе")]
    UnsupportedPlatform,
}

/// Полностью подготовленные process-owned значения для передачи в `AppShell`.
pub(crate) struct ProcessBootstrap {
    /// Trusted paths сохраняются для state owner-а, не попадая в AppConfig TOML.
    pub(crate) config_paths: ConfigPaths,
    /// Lease передаётся shell-у и переживает renderer suspend.
    pub(crate) instance_lease: AppInstanceLease,
    /// Config читается или создаётся только после успешного lease.
    pub(crate) loaded_config: LoadedConfig,
    /// Классифицированный ID-less media intent для существующего startup flow.
    pub(crate) initial_media: Option<InitialMedia>,
    /// Secret-safe ошибка классификации media, показываемая существующим UI path.
    pub(crate) startup_error: Option<String>,
}

/// Typed bootstrap errors фиксируют этап, не раскрывая CLI media или lock path.
///
/// Ошибки после определения путей несут папку настроек: текст для пользователя
/// (`fatal_startup`) называет папку, которую нужно исправить. В `Display`
/// (технический лог) путь по-прежнему не попадает. `ConfigError` в `Box`, чтобы
/// `Result` bootstrap-а не раздувался ради редкого пути ошибки.
#[derive(Debug, Error)]
pub(crate) enum ProcessBootstrapError {
    #[error("некорректные аргументы запуска: {0}")]
    Arguments(#[from] ProcessArgsError),

    #[error("не удалось определить platform config paths: {0}")]
    DiscoverPaths(ConfigError),

    #[error("не удалось получить право запуска: {lease_error}")]
    Lease {
        lease_error: AppInstanceLeaseError,
        config_dir: PathBuf,
    },

    #[error("не удалось загрузить config fastiplayer: {config_error}")]
    LoadConfig {
        config_error: Box<ConfigError>,
        config_dir: PathBuf,
    },

    /// Lease занят, но передать аргументы запущенному экземпляру не удалось.
    #[error("плеер уже запущен, передать ему запрос не удалось: {0}")]
    ForwardToRunningInstance(RunningInstanceForwardingError),
}

/// Чем закончился bootstrap процесса без ошибки.
pub(crate) enum ProcessStart {
    /// Этот процесс — единственный экземпляр: дальше создаются окно и плеер.
    /// В `Box`, потому что второй вариант пустой (clippy `large_enum_variant`).
    Primary(Box<ProcessBootstrap>),
    /// Плеер уже запущен, и он принял запрос этого процесса: штатный выход с кодом 0.
    ForwardedToRunningInstance,
}

/// Выполняет обязательный bootstrap order над реальными process dependencies.
pub(crate) fn bootstrap_process() -> Result<ProcessStart, ProcessBootstrapError> {
    let platform = NativeAppInstanceLeasePlatform;
    let outcome = bootstrap_with(
        std::env::args_os().skip(1),
        ConfigPaths::discover,
        &platform,
        // Под уже взятым lease: восстановление переименовывает битый config и убирает
        // брошенные temp, что безопасно только при единственном экземпляре.
        |paths| {
            fastiplayer_config::load_or_recover_at(
                paths.config_file(),
                std::time::SystemTime::now(),
            )
        },
        |process_args, _paths, loaded_config| {
            resolve_initial_media_arguments(
                process_args.take_initial_media_arguments(),
                &loaded_config.config,
            )
        },
        |process_args| {
            forwarding::forward_arguments_to_running_instance(
                process_args.initial_media_arguments(),
                forwarding::ForwardingEnvironment::of_current_process(),
                &forwarding::DesktopRunningInstanceForwarder,
            )
        },
    )?;
    let bootstrap = match outcome {
        BootstrapOutcome::Primary(bootstrap) => bootstrap,
        BootstrapOutcome::ForwardedToRunningInstance => {
            return Ok(ProcessStart::ForwardedToRunningInstance);
        }
    };
    let (initial_media, startup_error) = bootstrap.prepared;

    Ok(ProcessStart::Primary(Box::new(ProcessBootstrap {
        config_paths: bootstrap.paths,
        instance_lease: bootstrap.lease,
        loaded_config: bootstrap.config,
        initial_media,
        startup_error,
    })))
}

/// Внутренний generic harness закрепляет ordering без реального home/config I/O.
///
/// Если lease занят другим экземпляром, вызывается `forward_to_running_instance`
/// с ещё не тронутыми аргументами, и bootstrap на этом заканчивается: config этого
/// процесса не читается, media не классифицируется.
fn bootstrap_with<Config, Prepared>(
    arguments: impl IntoIterator<Item = OsString>,
    discover_paths: impl FnOnce() -> Result<ConfigPaths, ConfigError>,
    platform: &impl AppInstanceLeasePlatform,
    load_config: impl FnOnce(&ConfigPaths) -> Result<Config, ConfigError>,
    prepare_after_load: impl FnOnce(&mut ProcessArgs, &ConfigPaths, &Config) -> Prepared,
    forward_to_running_instance: impl FnOnce(&ProcessArgs) -> Result<(), RunningInstanceForwardingError>,
) -> Result<BootstrapOutcome<Config, Prepared>, ProcessBootstrapError> {
    let mut process_args = ProcessArgs::parse(arguments)?;
    let paths = discover_paths().map_err(ProcessBootstrapError::DiscoverPaths)?;
    let lease = match platform.acquire(&paths) {
        Ok(lease) => lease,
        Err(AppInstanceLeaseError::AlreadyRunning) => {
            forward_to_running_instance(&process_args)
                .map_err(ProcessBootstrapError::ForwardToRunningInstance)?;
            return Ok(BootstrapOutcome::ForwardedToRunningInstance);
        }
        Err(lease_error) => {
            return Err(ProcessBootstrapError::Lease {
                lease_error,
                config_dir: paths.config_dir().to_path_buf(),
            });
        }
    };
    let config = load_config(&paths).map_err(|config_error| ProcessBootstrapError::LoadConfig {
        config_error: Box::new(config_error),
        config_dir: paths.config_dir().to_path_buf(),
    })?;
    let prepared = prepare_after_load(&mut process_args, &paths, &config);

    Ok(BootstrapOutcome::Primary(BootstrapValues {
        paths,
        lease,
        config,
        prepared,
    }))
}

/// Generic-аналог [`ProcessStart`] для fake-able harness-а.
enum BootstrapOutcome<Config, Prepared> {
    Primary(BootstrapValues<Config, Prepared>),
    ForwardedToRunningInstance,
}

/// Generic result существует только для fake-able ordering harness-а.
struct BootstrapValues<Config, Prepared> {
    paths: ConfigPaths,
    lease: AppInstanceLease,
    config: Config,
    prepared: Prepared,
}

/// Выбирает adapter compile-time, не ослабляя contract на других ОС.
struct NativeAppInstanceLeasePlatform;

#[cfg(target_os = "linux")]
impl AppInstanceLeasePlatform for NativeAppInstanceLeasePlatform {
    fn acquire(&self, paths: &ConfigPaths) -> Result<AppInstanceLease, AppInstanceLeaseError> {
        linux::LinuxAppInstanceLeasePlatform.acquire(paths)
    }
}

#[cfg(not(target_os = "linux"))]
impl AppInstanceLeasePlatform for NativeAppInstanceLeasePlatform {
    fn acquire(&self, _paths: &ConfigPaths) -> Result<AppInstanceLease, AppInstanceLeaseError> {
        Err(AppInstanceLeaseError::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests;
