//! Process bootstrap и platform-neutral владение единственным экземпляром приложения.
//!
//! Модуль сохраняет строгий порядок побочных эффектов: сначала разбираются аргументы,
//! затем определяются platform paths, после чего lease берётся до чтения config и любой
//! подготовки media. Linux-specific descriptor details изолированы в `linux`.

use std::ffi::{OsStr, OsString};
use std::fmt;

use rustiplayer_config::{ConfigError, ConfigPaths, LoadedConfig};
use thiserror::Error;

use crate::startup_media::{InitialMedia, resolve_initial_media_argument};

#[cfg(target_os = "linux")]
mod linux;

/// Уже разобранные process arguments без lossy UTF-8 преобразования media path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessArgs {
    /// Необязательный единственный media positional.
    initial_media: Option<OsString>,
}

impl ProcessArgs {
    /// Разбирает пользовательские аргументы без имени исполняемого файла.
    pub(crate) fn parse(
        arguments: impl IntoIterator<Item = OsString>,
    ) -> Result<Self, ProcessArgsError> {
        let mut initial_media = None;
        let mut options_ended = false;

        for argument in arguments {
            if !options_ended && argument == OsStr::new("--") {
                options_ended = true;
                continue;
            }

            if !options_ended && os_string_starts_with_dash(&argument) {
                return Err(ProcessArgsError::UnknownOption);
            }

            if initial_media.replace(argument).is_some() {
                return Err(ProcessArgsError::ExtraPositional);
            }
        }

        Ok(Self { initial_media })
    }

    /// Передаёт media intent следующему bootstrap-этапу без копирования или перекодировки.
    fn take_initial_media(&mut self) -> Option<OsString> {
        self.initial_media.take()
    }
}

/// Typed CLI errors не включают потенциально секретное значение аргумента.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum ProcessArgsError {
    /// До `--` встретилась неизвестная option-подобная строка.
    #[error("неизвестная опция; локальный путь с ведущим '-' передавайте после '--'")]
    UnknownOption,

    /// Передано больше одного media positional.
    #[error("ожидался максимум один media argument")]
    ExtraPositional,
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
    #[error("другой экземпляр rustiplayer уже запущен")]
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
#[derive(Debug, Error)]
pub(crate) enum ProcessBootstrapError {
    #[error("некорректные аргументы запуска: {0}")]
    Arguments(#[from] ProcessArgsError),

    #[error("не удалось определить platform config paths: {0}")]
    DiscoverPaths(ConfigError),

    #[error("не удалось получить право запуска: {0}")]
    Lease(#[from] AppInstanceLeaseError),

    #[error("не удалось загрузить config rustiplayer: {0}")]
    LoadConfig(ConfigError),
}

/// Выполняет обязательный bootstrap order над реальными process dependencies.
pub(crate) fn bootstrap_process() -> Result<ProcessBootstrap, ProcessBootstrapError> {
    let platform = NativeAppInstanceLeasePlatform;
    let bootstrap = bootstrap_with(
        std::env::args_os().skip(1),
        ConfigPaths::discover,
        &platform,
        |paths| rustiplayer_config::load_or_create_at(paths.config_file()),
        |process_args, _paths, loaded_config| {
            resolve_initial_media_argument(process_args.take_initial_media(), &loaded_config.config)
        },
    )?;
    let (initial_media, startup_error) = bootstrap.prepared;

    Ok(ProcessBootstrap {
        config_paths: bootstrap.paths,
        instance_lease: bootstrap.lease,
        loaded_config: bootstrap.config,
        initial_media,
        startup_error,
    })
}

/// Внутренний generic harness закрепляет ordering без реального home/config I/O.
fn bootstrap_with<Config, Prepared>(
    arguments: impl IntoIterator<Item = OsString>,
    discover_paths: impl FnOnce() -> Result<ConfigPaths, ConfigError>,
    platform: &impl AppInstanceLeasePlatform,
    load_config: impl FnOnce(&ConfigPaths) -> Result<Config, ConfigError>,
    prepare_after_load: impl FnOnce(&mut ProcessArgs, &ConfigPaths, &Config) -> Prepared,
) -> Result<BootstrapValues<Config, Prepared>, ProcessBootstrapError> {
    let mut process_args = ProcessArgs::parse(arguments)?;
    let paths = discover_paths().map_err(ProcessBootstrapError::DiscoverPaths)?;
    let lease = platform.acquire(&paths)?;
    let config = load_config(&paths).map_err(ProcessBootstrapError::LoadConfig)?;
    let prepared = prepare_after_load(&mut process_args, &paths, &config);

    Ok(BootstrapValues {
        paths,
        lease,
        config,
        prepared,
    })
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
mod tests {
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::rc::Rc;

    use rustiplayer_config::ConfigPaths;

    use super::{
        AppInstanceLease, AppInstanceLeaseError, AppInstanceLeaseIoOperation,
        AppInstanceLeasePlatform, ProcessArgs, ProcessArgsError, ProcessBootstrapError,
        UnsafeAppInstanceArtifact, bootstrap_with,
    };

    #[derive(Debug)]
    struct FakeGuard;

    struct FakePlatform {
        calls: Rc<RefCell<Vec<&'static str>>>,
        outcome: Result<(), AppInstanceLeaseError>,
    }

    impl AppInstanceLeasePlatform for FakePlatform {
        fn acquire(&self, _paths: &ConfigPaths) -> Result<AppInstanceLease, AppInstanceLeaseError> {
            self.calls.borrow_mut().push("acquire-lease");
            self.outcome?;
            Ok(AppInstanceLease::from_guard(FakeGuard))
        }
    }

    #[test]
    fn process_args_accepts_empty_and_one_media() {
        assert_eq!(
            ProcessArgs::parse(Vec::<OsString>::new()).expect("empty args"),
            ProcessArgs {
                initial_media: None
            }
        );
        assert_eq!(
            ProcessArgs::parse([OsString::from("movie.mkv")]).expect("one media"),
            ProcessArgs {
                initial_media: Some(OsString::from("movie.mkv"))
            }
        );
    }

    #[test]
    fn process_args_rejects_unknown_option_and_extra_positional() {
        assert_eq!(
            ProcessArgs::parse([OsString::from("--unknown")]),
            Err(ProcessArgsError::UnknownOption)
        );
        assert_eq!(
            ProcessArgs::parse([OsString::from("one"), OsString::from("two")]),
            Err(ProcessArgsError::ExtraPositional)
        );
    }

    #[test]
    fn process_args_double_dash_allows_leading_dash_local_path() {
        assert_eq!(
            ProcessArgs::parse([OsString::from("--"), OsString::from("-movie.mkv")])
                .expect("path after delimiter"),
            ProcessArgs {
                initial_media: Some(OsString::from("-movie.mkv"))
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_args_preserves_non_utf8_local_path() {
        use std::os::unix::ffi::OsStringExt;

        let media = OsString::from_vec(b"movie-\xFF.mkv".to_vec());
        let parsed = ProcessArgs::parse([media.clone()]).expect("native local path");

        assert_eq!(parsed.initial_media, Some(media));
    }

    #[test]
    fn bootstrap_calls_discover_lease_load_and_prepare_in_exact_order() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let platform = FakePlatform {
            calls: calls.clone(),
            outcome: Ok(()),
        };

        let result = bootstrap_with(
            [OsString::from("movie.mkv")],
            {
                let calls = calls.clone();
                move || {
                    calls.borrow_mut().push("discover-paths");
                    Ok(ConfigPaths::from_config_dir("/explicit/test-root"))
                }
            },
            &platform,
            {
                let calls = calls.clone();
                move |_| {
                    calls.borrow_mut().push("load-config");
                    Ok(())
                }
            },
            {
                let calls = calls.clone();
                move |_, _, _| calls.borrow_mut().push("prepare-media")
            },
        );

        assert!(result.is_ok());
        assert_eq!(
            *calls.borrow(),
            [
                "discover-paths",
                "acquire-lease",
                "load-config",
                "prepare-media"
            ]
        );
    }

    #[test]
    fn invalid_args_stop_before_path_discovery() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let platform = FakePlatform {
            calls: calls.clone(),
            outcome: Ok(()),
        };
        let result = bootstrap_with(
            [OsString::from("--unknown")],
            {
                let calls = calls.clone();
                move || {
                    calls.borrow_mut().push("discover-paths");
                    Ok(ConfigPaths::from_config_dir("/unused"))
                }
            },
            &platform,
            |_| Ok(()),
            |_, _, _| (),
        );

        assert!(matches!(
            result,
            Err(ProcessBootstrapError::Arguments(
                ProcessArgsError::UnknownOption
            ))
        ));
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn unsupported_platform_stops_before_post_lease_side_effects() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let platform = FakePlatform {
            calls: calls.clone(),
            outcome: Err(AppInstanceLeaseError::UnsupportedPlatform),
        };
        let result = bootstrap_with(
            Vec::<OsString>::new(),
            {
                let calls = calls.clone();
                move || {
                    calls.borrow_mut().push("discover-paths");
                    Ok(ConfigPaths::from_config_dir("/explicit/test-root"))
                }
            },
            &platform,
            {
                let calls = calls.clone();
                move |_| {
                    calls.borrow_mut().push("load-config");
                    Ok(())
                }
            },
            {
                let calls = calls.clone();
                move |_, _, _| calls.borrow_mut().push("prepare-media")
            },
        );

        assert!(matches!(
            result,
            Err(ProcessBootstrapError::Lease(
                AppInstanceLeaseError::UnsupportedPlatform
            ))
        ));
        assert_eq!(*calls.borrow(), ["discover-paths", "acquire-lease"]);
    }

    #[test]
    fn fake_platform_errors_keep_shared_typed_mapping() {
        let expected_errors = [
            AppInstanceLeaseError::AlreadyRunning,
            AppInstanceLeaseError::Io {
                operation: AppInstanceLeaseIoOperation::AcquireLock,
                kind: std::io::ErrorKind::PermissionDenied,
            },
            AppInstanceLeaseError::UnsafeArtifact {
                reason: UnsafeAppInstanceArtifact::LockArtifactOwnerMismatch,
            },
            AppInstanceLeaseError::UnsupportedPlatform,
        ];

        for expected_error in expected_errors {
            let platform = FakePlatform {
                calls: Rc::new(RefCell::new(Vec::new())),
                outcome: Err(expected_error),
            };
            let result = bootstrap_with(
                Vec::<OsString>::new(),
                || Ok(ConfigPaths::from_config_dir("/explicit/test-root")),
                &platform,
                |_| Ok(()),
                |_, _, _| (),
            );

            assert!(matches!(
                result,
                Err(ProcessBootstrapError::Lease(actual_error))
                    if actual_error == expected_error
            ));
        }
    }
}
