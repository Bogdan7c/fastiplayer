//! Linux-v1 adapter: stable flock inode, user-only modes и explicit CLOEXEC.

use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

use fastiplayer_config::ConfigPaths;

use super::{
    AppInstanceLease, AppInstanceLeaseError, AppInstanceLeaseIoOperation, AppInstanceLeasePlatform,
    UnsafeAppInstanceArtifact,
};

/// Linux implementation скрывает fd, uid и mode от bootstrap/AppShell boundary.
pub(super) struct LinuxAppInstanceLeasePlatform;

impl AppInstanceLeasePlatform for LinuxAppInstanceLeasePlatform {
    fn acquire(&self, paths: &ConfigPaths) -> Result<AppInstanceLease, AppInstanceLeaseError> {
        acquire_linux_guard(paths).map(AppInstanceLease::from_guard)
    }
}

/// Guard держит единственный открытый descriptor до process shutdown.
struct LinuxLeaseGuard {
    /// Само владение descriptor-ом удерживает advisory lock до Drop/process exit.
    _file: File,
}

/// Выполняет Linux-specific проверки до публикации platform-neutral lease.
fn acquire_linux_guard(paths: &ConfigPaths) -> Result<LinuxLeaseGuard, AppInstanceLeaseError> {
    ensure_private_config_directory(paths)?;
    let lock_path = paths.app_instance_lock_file();
    reject_known_unsafe_lock_artifact(&lock_path)?;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock_path)
        .map_err(|error| io_error(AppInstanceLeaseIoOperation::OpenLockArtifact, error))?;

    let effective_user_id = effective_user_id();
    let descriptor_metadata = file
        .metadata()
        .map_err(|error| io_error(AppInstanceLeaseIoOperation::InspectLockDescriptor, error))?;
    validate_lock_metadata(&descriptor_metadata, effective_user_id)?;

    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| io_error(AppInstanceLeaseIoOperation::HardenLockArtifact, error))?;
    set_close_on_exec(&file)?;

    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(AppInstanceLeaseError::AlreadyRunning),
        Err(TryLockError::Error(error)) => {
            return Err(io_error(AppInstanceLeaseIoOperation::AcquireLock, error));
        }
    }

    revalidate_stable_lock_identity(&lock_path, &descriptor_metadata, effective_user_id)?;

    Ok(LinuxLeaseGuard { _file: file })
}

/// Создаёт final config directory с 0700 и проверяет существующий descriptor-neutral artifact.
fn ensure_private_config_directory(paths: &ConfigPaths) -> Result<(), AppInstanceLeaseError> {
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(paths.config_dir())
        .map_err(|error| io_error(AppInstanceLeaseIoOperation::CreateConfigDirectory, error))?;

    let metadata = fs::symlink_metadata(paths.config_dir())
        .map_err(|error| io_error(AppInstanceLeaseIoOperation::InspectConfigDirectory, error))?;
    if !metadata.file_type().is_dir() {
        return Err(AppInstanceLeaseError::UnsafeArtifact {
            reason: UnsafeAppInstanceArtifact::ConfigDirectoryIsNotDirectory,
        });
    }
    if metadata.uid() != effective_user_id() {
        return Err(AppInstanceLeaseError::UnsafeArtifact {
            reason: UnsafeAppInstanceArtifact::ConfigDirectoryOwnerMismatch,
        });
    }

    fs::set_permissions(paths.config_dir(), fs::Permissions::from_mode(0o700))
        .map_err(|error| io_error(AppInstanceLeaseIoOperation::HardenConfigDirectory, error))
}

/// Отсекает известный special-file/symlink случай до `open`, сохраняя fd revalidation после него.
fn reject_known_unsafe_lock_artifact(
    lock_path: &std::path::Path,
) -> Result<(), AppInstanceLeaseError> {
    match fs::symlink_metadata(lock_path) {
        Ok(metadata) => validate_lock_metadata(&metadata, effective_user_id()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(
            AppInstanceLeaseIoOperation::InspectLockArtifact,
            error,
        )),
    }
}

/// Проверяет regular-file и effective-user invariants одинаково для path и descriptor metadata.
fn validate_lock_metadata(
    metadata: &fs::Metadata,
    effective_user_id: u32,
) -> Result<(), AppInstanceLeaseError> {
    if !metadata.file_type().is_file() {
        return Err(AppInstanceLeaseError::UnsafeArtifact {
            reason: UnsafeAppInstanceArtifact::LockArtifactIsNotRegularFile,
        });
    }
    if metadata.uid() != effective_user_id {
        return Err(AppInstanceLeaseError::UnsafeArtifact {
            reason: UnsafeAppInstanceArtifact::LockArtifactOwnerMismatch,
        });
    }

    Ok(())
}

/// После flock повторно связывает pathname с тем же descriptor inode; artifact не удаляется.
fn revalidate_stable_lock_identity(
    lock_path: &std::path::Path,
    descriptor_metadata: &fs::Metadata,
    effective_user_id: u32,
) -> Result<(), AppInstanceLeaseError> {
    let path_metadata = fs::symlink_metadata(lock_path)
        .map_err(|error| io_error(AppInstanceLeaseIoOperation::RevalidateLockIdentity, error))?;
    validate_lock_metadata(&path_metadata, effective_user_id)?;
    if path_metadata.dev() != descriptor_metadata.dev()
        || path_metadata.ino() != descriptor_metadata.ino()
    {
        return Err(AppInstanceLeaseError::UnsafeArtifact {
            reason: UnsafeAppInstanceArtifact::LockArtifactIdentityChanged,
        });
    }

    Ok(())
}

/// Явно ставит и проверяет FD_CLOEXEC, не полагаясь только на `O_CLOEXEC` при open.
fn set_close_on_exec(file: &File) -> Result<(), AppInstanceLeaseError> {
    let descriptor = file.as_raw_fd();
    // SAFETY: `descriptor` принадлежит живому `File`; F_GETFD не меняет память Rust.
    let current_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if current_flags == -1 {
        return Err(io_error(
            AppInstanceLeaseIoOperation::SetCloseOnExec,
            io::Error::last_os_error(),
        ));
    }

    // SAFETY: F_SETFD принимает целочисленные descriptor flags для того же живого fd.
    let set_result =
        unsafe { libc::fcntl(descriptor, libc::F_SETFD, current_flags | libc::FD_CLOEXEC) };
    if set_result == -1 {
        return Err(io_error(
            AppInstanceLeaseIoOperation::SetCloseOnExec,
            io::Error::last_os_error(),
        ));
    }

    // SAFETY: повторный F_GETFD читает flags того же валидного descriptor-а.
    let verified_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if verified_flags == -1 || verified_flags & libc::FD_CLOEXEC == 0 {
        return Err(io_error(
            AppInstanceLeaseIoOperation::SetCloseOnExec,
            if verified_flags == -1 {
                io::Error::last_os_error()
            } else {
                io::Error::other("FD_CLOEXEC verification failed")
            },
        ));
    }

    Ok(())
}

/// libc getuid не имеет error channel и возвращает effective credentials процесса.
fn effective_user_id() -> u32 {
    // SAFETY: `geteuid` не принимает указатели и не меняет Rust-owned память.
    unsafe { libc::geteuid() }
}

/// Стирает path/source из публичной ошибки, сохраняя stage и `ErrorKind`.
fn io_error(operation: AppInstanceLeaseIoOperation, error: io::Error) -> AppInstanceLeaseError {
    AppInstanceLeaseError::Io {
        operation,
        kind: error.kind(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{BufRead, BufReader};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::process::{Command, Stdio};
    use std::sync::{Mutex, MutexGuard};
    use std::time::Duration;

    use fastiplayer_config::ConfigPaths;
    use tempfile::TempDir;

    use super::{acquire_linux_guard, effective_user_id, validate_lock_metadata};
    use crate::app_instance::{AppInstanceLeaseError, UnsafeAppInstanceArtifact};

    /// Сериализует Linux lease tests внутри одного многопоточного test process.
    ///
    /// Иначе параллельный `fork -> exec` другого теста на короткое время наследует
    /// чужой descriptor до применения `FD_CLOEXEC` в `exec` и создаёт искусственный
    /// `AlreadyRunning` сразу после `drop(owner)`.
    static LEASE_TEST_SERIALIZATION: Mutex<()> = Mutex::new(());

    /// Однозначный pipe-сигнал: owner уже выполнил первый retry после захвата lease.
    const ORDERED_CONTENDER_OWNER_READY: &str = "fastiplayer-ordered-contender-owner-ready";

    fn serialize_lease_test() -> MutexGuard<'static, ()> {
        LEASE_TEST_SERIALIZATION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn new_and_existing_artifacts_are_private_and_inode_is_stable() {
        let _lease_test_guard = serialize_lease_test();
        let root = TempDir::new().expect("temp root");
        let paths = ConfigPaths::from_config_dir(root.path().join("config"));
        let first = acquire_linux_guard(&paths).expect("first lease");
        let lock_path = paths.app_instance_lock_file();
        let first_metadata = fs::metadata(&lock_path).expect("lock metadata");

        assert_eq!(
            fs::metadata(paths.config_dir())
                .expect("config metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(first_metadata.permissions().mode() & 0o777, 0o600);

        drop(first);
        assert!(
            lock_path.exists(),
            "stable lock file must never be unlinked"
        );
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o666))
            .expect("weaken existing mode for hardening test");
        let second = acquire_linux_guard(&paths).expect("second lease");
        let second_metadata = fs::metadata(&lock_path).expect("second metadata");

        assert_eq!(first_metadata.ino(), second_metadata.ino());
        assert_eq!(second_metadata.permissions().mode() & 0o777, 0o600);
        drop(second);
    }

    #[test]
    fn contention_is_typed_and_release_allows_reacquire() {
        let _lease_test_guard = serialize_lease_test();
        let root = TempDir::new().expect("temp root");
        let paths = ConfigPaths::from_config_dir(root.path().join("config"));
        let owner = acquire_linux_guard(&paths).expect("owner lease");

        assert!(matches!(
            acquire_linux_guard(&paths),
            Err(AppInstanceLeaseError::AlreadyRunning)
        ));
        drop(owner);
        assert!(acquire_linux_guard(&paths).is_ok());
    }

    #[test]
    fn unsafe_lock_type_and_owner_mismatch_are_distinct() {
        let _lease_test_guard = serialize_lease_test();
        let root = TempDir::new().expect("temp root");
        let paths = ConfigPaths::from_config_dir(root.path().join("config"));
        fs::create_dir_all(paths.app_instance_lock_file()).expect("directory artifact");

        assert!(matches!(
            acquire_linux_guard(&paths),
            Err(AppInstanceLeaseError::UnsafeArtifact {
                reason: UnsafeAppInstanceArtifact::LockArtifactIsNotRegularFile
            })
        ));

        fs::remove_dir(paths.app_instance_lock_file()).expect("remove directory artifact");
        let symlink_target = root.path().join("symlink-target");
        fs::write(&symlink_target, []).expect("symlink target");
        std::os::unix::fs::symlink(&symlink_target, paths.app_instance_lock_file())
            .expect("symlink artifact");
        assert!(matches!(
            acquire_linux_guard(&paths),
            Err(AppInstanceLeaseError::UnsafeArtifact {
                reason: UnsafeAppInstanceArtifact::LockArtifactIsNotRegularFile
            })
        ));

        let metadata = fs::metadata(root.path()).expect("owned metadata");
        assert!(matches!(
            validate_lock_metadata(&metadata, effective_user_id().wrapping_add(1)),
            Err(AppInstanceLeaseError::UnsafeArtifact {
                reason: UnsafeAppInstanceArtifact::LockArtifactIsNotRegularFile
            })
        ));
        let regular_path = root.path().join("regular");
        fs::write(&regular_path, []).expect("regular file");
        let regular_metadata = fs::metadata(regular_path).expect("regular metadata");
        assert!(matches!(
            validate_lock_metadata(&regular_metadata, effective_user_id().wrapping_add(1)),
            Err(AppInstanceLeaseError::UnsafeArtifact {
                reason: UnsafeAppInstanceArtifact::LockArtifactOwnerMismatch
            })
        ));
    }

    #[test]
    fn explicit_roots_have_independent_leases() {
        let _lease_test_guard = serialize_lease_test();
        let first_root = TempDir::new().expect("first root");
        let second_root = TempDir::new().expect("second root");
        let first_paths = ConfigPaths::from_config_dir(first_root.path().join("config"));
        let second_paths = ConfigPaths::from_config_dir(second_root.path().join("config"));

        let _first = acquire_linux_guard(&first_paths).expect("first lease");
        let _second = acquire_linux_guard(&second_paths).expect("second lease");
    }

    #[test]
    fn descriptor_is_closed_across_exec() {
        let _lease_test_guard = serialize_lease_test();
        let root = TempDir::new().expect("temp root");
        let paths = ConfigPaths::from_config_dir(root.path().join("config"));
        let guard = acquire_linux_guard(&paths).expect("lease");
        let descriptor = guard._file.as_raw_fd().to_string();
        let status = Command::new("sh")
            .arg("-c")
            .arg("test ! -e /proc/self/fd/$LEASE_FD")
            .env("LEASE_FD", descriptor)
            .status()
            .expect("exec child");

        assert!(status.success(), "lease fd leaked into exec child");
    }

    #[test]
    fn abnormal_process_exit_releases_lock_without_unlink() {
        let _lease_test_guard = serialize_lease_test();
        let root = TempDir::new().expect("temp root");
        let config_dir = root.path().join("config");
        let status = subprocess_command(&config_dir, "abort")
            .status()
            .expect("abort helper");

        assert!(!status.success());
        let paths = ConfigPaths::from_config_dir(config_dir);
        assert!(paths.app_instance_lock_file().exists());
        assert!(acquire_linux_guard(&paths).is_ok());
    }

    #[test]
    fn simultaneous_cold_start_has_exactly_one_owner() {
        let _lease_test_guard = serialize_lease_test();
        let root = TempDir::new().expect("temp root");
        let config_dir = root.path().join("config");
        let start_file = root.path().join("start");
        let contender_done_file = root.path().join("contender-done");
        let mut first = subprocess_command(&config_dir, "contend")
            .env("FASTIPLAYER_TEST_START_FILE", &start_file)
            .env("FASTIPLAYER_TEST_CONTENDER_DONE", &contender_done_file)
            .spawn()
            .expect("first contender");
        let mut second = subprocess_command(&config_dir, "contend")
            .env("FASTIPLAYER_TEST_START_FILE", &start_file)
            .env("FASTIPLAYER_TEST_CONTENDER_DONE", &contender_done_file)
            .spawn()
            .expect("second contender");
        fs::write(&start_file, []).expect("release contenders");

        let first_status = first.wait().expect("first status");
        let second_status = second.wait().expect("second status");
        let statuses = [first_status.code(), second_status.code()];

        assert_eq!(
            statuses.iter().filter(|status| **status == Some(0)).count(),
            1
        );
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == Some(23))
                .count(),
            1
        );
        assert!(!config_dir.join("config.toml").exists());
        assert!(!config_dir.join("playlist-state.json").exists());
    }

    #[test]
    fn ordered_contention_retries_owner_before_typed_rejection() {
        let _lease_test_guard = serialize_lease_test();
        let root = TempDir::new().expect("temp root");
        let config_dir = root.path().join("config");
        let start_file = root.path().join("start");
        let contender_done_file = root.path().join("contender-done");
        let mut owner = subprocess_command(&config_dir, "ordered-contender-owner")
            .env("FASTIPLAYER_TEST_CONTENDER_DONE", &contender_done_file)
            .stderr(Stdio::piped())
            .spawn()
            .expect("ordered owner");
        let owner_stderr = owner.stderr.take().expect("ordered owner stderr");
        let mut owner_ready = String::new();
        BufReader::new(owner_stderr)
            .read_line(&mut owner_ready)
            .expect("ordered owner readiness");
        assert_eq!(owner_ready.trim_end(), ORDERED_CONTENDER_OWNER_READY);

        fs::write(&start_file, []).expect("release typed contender");
        let contender_status = subprocess_command(&config_dir, "contend")
            .env("FASTIPLAYER_TEST_START_FILE", &start_file)
            .env("FASTIPLAYER_TEST_CONTENDER_DONE", &contender_done_file)
            .status()
            .expect("typed contender status");
        let owner_status = owner.wait().expect("ordered owner status");

        assert_eq!(contender_status.code(), Some(23));
        assert!(owner_status.success());
        assert!(!config_dir.join("config.toml").exists());
        assert!(!config_dir.join("playlist-state.json").exists());
    }

    #[test]
    fn forced_timeout_owner_blocks_competitor_until_process_exit() {
        let _lease_test_guard = serialize_lease_test();
        let root = TempDir::new().expect("temp root");
        let config_dir = root.path().join("config");
        let ready_file = root.path().join("ready");
        let mut owner = subprocess_command(&config_dir, "hold")
            .env("FASTIPLAYER_TEST_READY_FILE", &ready_file)
            .spawn()
            .expect("holding owner");
        for _ in 0..100 {
            if ready_file.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready_file.exists(), "owner did not publish ready marker");

        let paths = ConfigPaths::from_config_dir(config_dir.clone());
        assert!(matches!(
            acquire_linux_guard(&paths),
            Err(AppInstanceLeaseError::AlreadyRunning)
        ));

        owner.kill().expect("hard-kill owner");
        owner.wait().expect("reap killed owner");

        assert!(paths.app_instance_lock_file().exists());
        assert!(acquire_linux_guard(&paths).is_ok());
    }

    #[test]
    fn exec_child_does_not_keep_parent_lease_alive() {
        let _lease_test_guard = serialize_lease_test();
        let root = TempDir::new().expect("temp root");
        let config_dir = root.path().join("config");
        let child_pid_file = root.path().join("child.pid");
        let status = subprocess_command(&config_dir, "spawn-child")
            .env("FASTIPLAYER_TEST_CHILD_PID_FILE", &child_pid_file)
            .status()
            .expect("parent helper");

        assert!(status.success());
        let child_pid = fs::read_to_string(&child_pid_file).expect("child pid");
        assert!(
            std::path::Path::new("/proc")
                .join(child_pid.trim())
                .exists(),
            "proof child should still be alive"
        );
        let paths = ConfigPaths::from_config_dir(config_dir);
        assert!(acquire_linux_guard(&paths).is_ok());
    }

    #[test]
    #[allow(
        clippy::zombie_processes,
        reason = "helper intentionally exits while its proof child remains alive to verify CLOEXEC"
    )]
    fn subprocess_lease_helper() {
        let Ok(config_dir) = std::env::var("FASTIPLAYER_TEST_LEASE_ROOT") else {
            return;
        };
        let action = std::env::var("FASTIPLAYER_TEST_LEASE_ACTION").expect("helper action");
        let paths = ConfigPaths::from_config_dir(config_dir);

        if action == "contend" {
            let start_file = std::env::var("FASTIPLAYER_TEST_START_FILE").expect("start file");
            while !std::path::Path::new(&start_file).exists() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        let lease = match acquire_linux_guard(&paths) {
            Ok(lease) => lease,
            Err(AppInstanceLeaseError::AlreadyRunning) if action == "contend" => {
                let contender_done =
                    std::env::var("FASTIPLAYER_TEST_CONTENDER_DONE").expect("contender done file");
                fs::write(contender_done, []).expect("publish contention result");
                std::process::exit(23);
            }
            Err(error) => panic!("helper acquire failed: {error}"),
        };

        match action.as_str() {
            "abort" => std::process::abort(),
            "contend" | "ordered-contender-owner" => {
                let contender_done =
                    std::env::var("FASTIPLAYER_TEST_CONTENDER_DONE").expect("contender done file");
                for attempt in 0..200 {
                    if std::path::Path::new(&contender_done).exists() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                    if action == "ordered-contender-owner" && attempt == 0 {
                        eprintln!("{ORDERED_CONTENDER_OWNER_READY}");
                    }
                }
                std::process::exit(24);
            }
            "hold" => {
                let ready_file = std::env::var("FASTIPLAYER_TEST_READY_FILE").expect("ready file");
                fs::write(ready_file, []).expect("write ready marker");
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            "spawn-child" => {
                let child = Command::new("sleep")
                    .arg("2")
                    .spawn()
                    .expect("spawn proof child");
                let pid_path =
                    std::env::var("FASTIPLAYER_TEST_CHILD_PID_FILE").expect("child pid path");
                fs::write(pid_path, child.id().to_string()).expect("write child pid");
                drop(lease);
            }
            other => panic!("unknown helper action: {other}"),
        }
    }

    fn subprocess_command(config_dir: &std::path::Path, action: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("current test binary"));
        command
            .arg("--exact")
            .arg("app_instance::linux::tests::subprocess_lease_helper")
            .arg("--nocapture")
            .env("FASTIPLAYER_TEST_LEASE_ROOT", config_dir)
            .env("FASTIPLAYER_TEST_LEASE_ACTION", action);
        command
    }
}
