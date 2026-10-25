//! Тесты перезапуска: решение, порядок «замок отпущен -> запуск», отказы запуска и
//! настоящий дочерний процесс.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};

use super::*;

/// Fake-источник пути бинаря.
struct FixedExecutable(io::Result<PathBuf>);

impl ExecutablePathSource for FixedExecutable {
    fn current_executable(&self) -> io::Result<PathBuf> {
        match &self.0 {
            Ok(path) => Ok(path.clone()),
            Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
        }
    }
}

/// Fake-spawner: записывает запуски и подставляет заданный исход.
struct RecordingSpawner {
    spawned: RefCell<Vec<PathBuf>>,
    fail_with: Option<io::ErrorKind>,
}

impl RecordingSpawner {
    fn succeeding() -> Self {
        Self {
            spawned: RefCell::new(Vec::new()),
            fail_with: None,
        }
    }

    fn spawned(&self) -> Vec<PathBuf> {
        self.spawned.borrow().clone()
    }
}

impl ProgramSpawner for RecordingSpawner {
    fn spawn_detached(&self, program: &Path) -> io::Result<()> {
        if let Some(kind) = self.fail_with {
            return Err(io::Error::new(kind, "fake spawn failure"));
        }
        self.spawned.borrow_mut().push(program.to_path_buf());
        Ok(())
    }
}

/// Токен для тестов, где порядок с реальным замком не важен.
fn released_token() -> InstanceLeaseReleased {
    InstanceLeaseReleased(())
}

/// Существующий файл, который сойдёт за «свой бинарь» (сам тестовый исполняемый файл).
fn existing_executable() -> PathBuf {
    std::env::current_exe().expect("test executable path")
}

#[test]
fn decision_requires_request_and_clean_shutdown() {
    let decide = decide_relaunch;
    for shutdown in [ShutdownQuality::Clean, ShutdownQuality::Degraded] {
        assert_eq!(
            decide(RelaunchRequest::NotRequested, shutdown),
            RelaunchDecision::NotRequested
        );
    }
    assert_eq!(
        decide(RelaunchRequest::AfterExit, ShutdownQuality::Clean),
        RelaunchDecision::Relaunch
    );
    assert_eq!(
        decide(RelaunchRequest::AfterExit, ShutdownQuality::Degraded),
        RelaunchDecision::SkippedAfterDegradedShutdown
    );
}

#[test]
fn clean_requested_relaunch_spawns_current_executable_without_arguments() {
    let spawner = RecordingSpawner::succeeding();
    let program = existing_executable();

    let outcome = relaunch_after_release(
        released_token(),
        RelaunchRequest::AfterExit,
        ShutdownQuality::Clean,
        &FixedExecutable(Ok(program.clone())),
        &spawner,
    );

    assert!(
        matches!(outcome, RelaunchOutcome::Spawned { program: ref spawned } if *spawned == program)
    );
    assert_eq!(spawner.spawned(), vec![program]);
}

#[test]
fn not_requested_never_spawns() {
    let spawner = RecordingSpawner::succeeding();

    let outcome = relaunch_after_release(
        released_token(),
        RelaunchRequest::NotRequested,
        ShutdownQuality::Clean,
        &FixedExecutable(Ok(existing_executable())),
        &spawner,
    );

    assert!(matches!(outcome, RelaunchOutcome::NotRequested));
    assert!(spawner.spawned().is_empty());
}

#[test]
fn degraded_shutdown_skips_relaunch_instead_of_restarting_over_partial_state() {
    let spawner = RecordingSpawner::succeeding();

    let outcome = relaunch_after_release(
        released_token(),
        RelaunchRequest::AfterExit,
        ShutdownQuality::Degraded,
        &FixedExecutable(Ok(existing_executable())),
        &spawner,
    );

    assert!(matches!(
        outcome,
        RelaunchOutcome::SkippedAfterDegradedShutdown
    ));
    assert!(spawner.spawned().is_empty());
}

#[test]
fn replaced_or_missing_binary_is_a_reported_failure_not_a_silent_skip() {
    let directory = tempfile::tempdir().expect("temp dir");
    let missing = directory.path().join("fastiplayer");
    let deleted_suffix = PathBuf::from(format!("{} (deleted)", existing_executable().display()));
    for program in [missing, deleted_suffix] {
        let spawner = RecordingSpawner::succeeding();

        let outcome = relaunch_after_release(
            released_token(),
            RelaunchRequest::AfterExit,
            ShutdownQuality::Clean,
            &FixedExecutable(Ok(program.clone())),
            &spawner,
        );

        assert!(
            matches!(
                outcome,
                RelaunchOutcome::Failed(RelaunchError::ExecutableReplaced { .. })
            ),
            "{program:?}: {outcome:?}"
        );
        assert!(spawner.spawned().is_empty(), "{program:?}");
    }
}

#[test]
fn unknown_executable_path_and_spawn_failure_stay_distinct() {
    let spawner = RecordingSpawner::succeeding();
    let unknown = relaunch_after_release(
        released_token(),
        RelaunchRequest::AfterExit,
        ShutdownQuality::Clean,
        &FixedExecutable(Err(io::Error::from(io::ErrorKind::NotFound))),
        &spawner,
    );
    assert!(matches!(
        unknown,
        RelaunchOutcome::Failed(RelaunchError::ExecutablePathUnknown(_))
    ));

    let failing = RecordingSpawner {
        spawned: RefCell::new(Vec::new()),
        fail_with: Some(io::ErrorKind::PermissionDenied),
    };
    let denied = relaunch_after_release(
        released_token(),
        RelaunchRequest::AfterExit,
        ShutdownQuality::Clean,
        &FixedExecutable(Ok(existing_executable())),
        &failing,
    );
    let RelaunchOutcome::Failed(RelaunchError::SpawnFailed { source, .. }) = denied else {
        panic!("expected SpawnFailed, got {denied:?}");
    };
    assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
}

/// Spawner, который в момент запуска пробует взять тот же замок single-instance.
/// Именно так поведёт себя новый процесс: если замок ещё удержан, он «проиграет гонку».
#[cfg(target_os = "linux")]
struct LeaseProbingSpawner {
    paths: fastiplayer_config::ConfigPaths,
    lease_was_free_at_spawn: RefCell<Option<bool>>,
}

#[cfg(target_os = "linux")]
impl ProgramSpawner for LeaseProbingSpawner {
    fn spawn_detached(&self, _program: &Path) -> io::Result<()> {
        use super::super::AppInstanceLeasePlatform;
        // Повтор нужен только тестам: параллельный тест может на миг унаследовать копию
        // дескриптора через fork (до exec он CLOEXEC-закрывается). Если замок так и не
        // освободился, `release()` его не отпустил, и тест честно падает.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let acquired = loop {
            let attempt = super::super::linux::LinuxAppInstanceLeasePlatform.acquire(&self.paths);
            if attempt.is_ok() || std::time::Instant::now() >= deadline {
                break attempt;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        *self.lease_was_free_at_spawn.borrow_mut() = Some(acquired.is_ok());
        Ok(())
    }
}

/// Порядок: пока замок держит старый процесс, новый его не получит; после `release` и
/// перезапуска новый процесс замок получает.
#[cfg(target_os = "linux")]
#[test]
fn replacement_is_spawned_only_after_the_instance_lease_is_free() {
    use super::super::AppInstanceLeasePlatform;
    let root = tempfile::tempdir().expect("temp root");
    let paths = fastiplayer_config::ConfigPaths::from_config_dir(root.path().join("config"));
    let lease = super::super::linux::LinuxAppInstanceLeasePlatform
        .acquire(&paths)
        .expect("first instance lease");
    // Контроль теста: пока замок у «старого процесса», претендент получает отказ.
    assert!(matches!(
        super::super::linux::LinuxAppInstanceLeasePlatform.acquire(&paths),
        Err(super::super::AppInstanceLeaseError::AlreadyRunning)
    ));
    let spawner = LeaseProbingSpawner {
        paths,
        lease_was_free_at_spawn: RefCell::new(None),
    };

    let released = lease.release();
    let outcome = relaunch_after_release(
        released,
        RelaunchRequest::AfterExit,
        ShutdownQuality::Clean,
        &FixedExecutable(Ok(existing_executable())),
        &spawner,
    );

    assert!(matches!(outcome, RelaunchOutcome::Spawned { .. }));
    assert_eq!(
        *spawner.lease_was_free_at_spawn.borrow(),
        Some(true),
        "the new process must find the instance lock free"
    );
}

/// Настоящий дочерний процесс: скрипт-«бинарь» стартует без аргументов и оставляет маркер.
#[cfg(unix)]
#[test]
fn system_spawner_starts_a_real_child_process_without_arguments() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let directory = tempfile::tempdir().expect("temp dir");
    let script = directory.path().join("fake-player.sh");
    let marker = directory.path().join("started.marker");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf 'argc=%s' \"$#\" > '{}'\n",
            marker.display()
        ),
    )
    .expect("script written");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("script is executable");

    // «Text file busy» возможен, если другой поток теста форкает, пока скрипт был открыт
    // на запись; повтор безопасен: до успешного exec ничего не запущено.
    let mut outcome = RelaunchOutcome::NotRequested;
    for _ in 0..20 {
        outcome = relaunch_after_release(
            released_token(),
            RelaunchRequest::AfterExit,
            ShutdownQuality::Clean,
            &FixedExecutable(Ok(script.clone())),
            &SystemProgramSpawner,
        );
        if matches!(outcome, RelaunchOutcome::Spawned { .. }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        matches!(outcome, RelaunchOutcome::Spawned { .. }),
        "{outcome:?}"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read_to_string(&marker).expect("child wrote the marker"),
        "argc=0"
    );
}
