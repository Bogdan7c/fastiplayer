use super::*;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::recovery::{
    MAX_RECOVERY_DUMP_BYTES, MAX_RECOVERY_DUMP_FILES, enrich_recovered_document_title,
    read_recovery_embed_candidates,
};

static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
fn create_executable_test_script(
    directory: &Path,
    script: &str,
) -> Result<PathBuf, YtDlpServiceError> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let path = directory.join("fake-yt-dlp");
    let mut executable_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(YtDlpServiceError::process)?;
    executable_file
        .write_all(script.as_bytes())
        .map_err(YtDlpServiceError::process)?;
    let mut permissions = executable_file
        .metadata()
        .map_err(YtDlpServiceError::process)?
        .permissions();
    permissions.set_mode(0o700);
    executable_file
        .set_permissions(permissions)
        .map_err(YtDlpServiceError::process)?;
    drop(executable_file);
    Ok(path)
}

fn test_directory(label: &str) -> PathBuf {
    for _ in 0..16 {
        let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rustiplayer-ytdlp-test-{label}-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return path,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("test directory creation failed: {error}"),
        }
    }

    panic!("test directory collision budget exhausted")
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn create(label: &str) -> Self {
        Self(test_directory(label))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Запускает внешний процесс, читает stdout/stderr параллельно и ограничивает ожидание.
fn run_process_with_timeout(
    executable: &str,
    arguments: &[&str],
    timeout: Duration,
) -> Result<ProcessOutput, YtDlpServiceError> {
    let default_config = YtDlpConfig::default();
    let output_budgets = YtDlpProcessOutputBudgets::new(
        default_config.single_item_stdout_limit_bytes,
        default_config.single_item_stderr_limit_bytes,
        default_config.single_item_json_node_limit,
    )?;
    run_process_with_timeout_and_cancellation(
        executable,
        arguments,
        None,
        timeout,
        output_budgets,
        &|| false,
    )
}

/// Возвращает production defaults для focused process lifecycle tests.
fn test_output_budgets() -> YtDlpProcessOutputBudgets {
    let config = YtDlpConfig::default();
    YtDlpProcessOutputBudgets::new(
        config.single_item_stdout_limit_bytes,
        config.single_item_stderr_limit_bytes,
        config.single_item_json_node_limit,
    )
    .expect("default output budgets валидны")
}

/// Создаёт маленький explicit budget для exact-boundary regressions.
fn explicit_output_budgets(
    stdout_bytes: u64,
    stderr_bytes: u64,
    json_nodes: u64,
) -> YtDlpProcessOutputBudgets {
    YtDlpProcessOutputBudgets::new(stdout_bytes, stderr_bytes, json_nodes)
        .expect("explicit test output budgets валидны")
}

/// Прямой caller service API не может обойти верхние resource limits AppConfig-а.
#[test]
fn process_config_rejects_unvalidated_resource_budget_above_config_maximum() {
    let config = YtDlpConfig {
        single_item_stdout_limit_bytes: u64::MAX,
        ..YtDlpConfig::default()
    };

    let error = YtDlpProcessConfig::from_yt_dlp_config(&config)
        .expect_err("direct YtDlpConfig caller не должен обходить validation");

    assert!(matches!(error, YtDlpServiceError::ProcessFailure { .. }));
}

/// Проверяет, что fixture process уже не выполняется; zombie означает завершённый descendant.
#[cfg(unix)]
fn process_id_is_running(process_id: libc::pid_t) -> bool {
    let process_stat_path = format!("/proc/{process_id}/stat");
    let Ok(process_stat) = fs::read_to_string(process_stat_path) else {
        return false;
    };
    let Some((_, state_and_fields)) = process_stat.rsplit_once(") ") else {
        return true;
    };
    !matches!(state_and_fields.as_bytes().first(), Some(b'Z' | b'X'))
}

#[cfg(unix)]
struct EscapedProcessGuard {
    process_id_record: PathBuf,
}

#[cfg(unix)]
impl EscapedProcessGuard {
    fn new(process_id_record: PathBuf) -> Self {
        Self { process_id_record }
    }

    fn wait_for_process_id(&self) -> Option<libc::pid_t> {
        let started_at = Instant::now();
        loop {
            if let Ok(process_id_text) = fs::read_to_string(&self.process_id_record)
                && let Ok(process_id) = process_id_text.trim().parse::<libc::pid_t>()
                && process_id > 0
            {
                return Some(process_id);
            }
            if started_at.elapsed() >= Duration::from_secs(1) {
                return None;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(unix)]
impl Drop for EscapedProcessGuard {
    fn drop(&mut self) {
        let Some(process_id) = self.wait_for_process_id() else {
            eprintln!("escaped fixture не записал PID для cleanup");
            return;
        };
        // SAFETY: положительный PID прочитан из app-owned fixture marker-а.
        let kill_result = unsafe { libc::kill(process_id, libc::SIGKILL) };
        if kill_result == -1 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            eprintln!("escaped fixture PID {process_id} не получил SIGKILL");
            return;
        }

        let cleanup_started_at = Instant::now();
        loop {
            // SAFETY: signal 0 только проверяет существование известного fixture PID.
            let probe_result = unsafe { libc::kill(process_id, 0) };
            if probe_result == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                return;
            }
            if cleanup_started_at.elapsed() >= Duration::from_secs(2) {
                eprintln!("escaped fixture PID {process_id} не был reap-нут вовремя");
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(unix)]
#[test]
fn recovery_discovers_player_across_dumps_and_cleans_working_directory() {
    let fixture_directory = TestDirectory::create("recovery");
    let recovery_path_record = fixture_directory.path().join("recovery-path");
    let script = format!(
        r#"#!/bin/sh
for argument do
if [ "$argument" = "--write-pages" ]; then
    pwd > "{}"
    printf '%s' '<title>Wrong fallback</title><iframe src="https://ordinary.example/assets/preview"></iframe>' > a.dump
    printf '%s' '<link rel="canonical" href="https://cinema.example/watch/42"><meta property="og:title" content="Catalog film"><iframe src="https://www.youtube.com/embed/hijack"></iframe><iframe src="https://broken.example/player/first"></iframe><iframe src="https://anonymous.example/player/42"></iframe>' > b.dump
    exit 0
fi
last_argument="$argument"
done
if [ "$last_argument" = "https://cinema.example/watch/42" ]; then
printf '%s\n' '{{"extractor_key":"Youtube","webpage_url":"https://www.youtube.com/watch?v=hijack"}}'
elif [ "$last_argument" = "https://broken.example/player/first" ]; then
exit 92
else
test "$last_argument" = "https://anonymous.example/player/42" || exit 91
printf '%s\n' '{{"extractor_key":"Generic","webpage_url":"https://anonymous.example/player/42"}}'
fi
"#,
        recovery_path_record.display()
    );
    let executable = create_executable_test_script(fixture_directory.path(), &script)
        .expect("fake yt-dlp executable");
    let process_config = YtDlpProcessConfig {
        executable: executable.to_string_lossy().into_owned(),
        timeout: Duration::from_secs(2),
        output_budgets: test_output_budgets(),
    };

    let locator = crate::parse_yt_dlp_media_locator("https://cinema.example/watch/42")
        .expect("parse recovery test locator");
    let document: Value =
        resolve_yt_dlp_candidate_document_with_cancellation(&locator, &process_config, &|| false)
            .expect("recovery should select anonymous player");

    assert_eq!(
        document.get("webpage_url").and_then(Value::as_str),
        Some("https://anonymous.example/player/42")
    );
    assert_eq!(
        document.get("title").and_then(Value::as_str),
        Some("Catalog film")
    );
    let recovery_path =
        fs::read_to_string(recovery_path_record).expect("script records recovery cwd");
    assert!(
        !Path::new(recovery_path.trim()).exists(),
        "process-owned recovery directory must be cleaned"
    );
}

#[cfg(unix)]
#[test]
fn cancelled_recovery_cleans_working_directory() {
    let fixture_directory = TestDirectory::create("cancelled-recovery");
    let recovery_path_record = fixture_directory.path().join("recovery-path");
    let script = format!(
        r#"#!/bin/sh
for argument do
if [ "$argument" = "--write-pages" ]; then
    pwd > "{}"
    sleep 30
    exit 0
fi
done
printf '%s\n' '{{"extractor_key":"Youtube","webpage_url":"https://www.youtube.com/watch?v=hijack"}}'
"#,
        recovery_path_record.display()
    );
    let executable = create_executable_test_script(fixture_directory.path(), &script)
        .expect("fake yt-dlp executable");
    let process_config = YtDlpProcessConfig {
        executable: executable.to_string_lossy().into_owned(),
        timeout: Duration::from_secs(10),
        output_budgets: test_output_budgets(),
    };

    let cancellation_started_at = Instant::now();
    let locator = crate::parse_yt_dlp_media_locator("https://cinema.example/watch/42")
        .expect("parse cancelled recovery test locator");
    let error = resolve_yt_dlp_candidate_document_with_cancellation::<Value>(
        &locator,
        &process_config,
        &|| recovery_path_record.exists(),
    )
    .expect_err("recovery cancellation must remain typed");

    assert!(
        matches!(error, YtDlpServiceError::Cancellation),
        "recovery cancellation returned {error:?}"
    );
    assert!(
        cancellation_started_at.elapsed() < Duration::from_secs(2),
        "owned process-group cancellation must not wait for the descendant sleep"
    );
    let recovery_path =
        fs::read_to_string(recovery_path_record).expect("script records recovery cwd");
    assert!(
        !Path::new(recovery_path.trim()).exists(),
        "cancelled recovery directory must be cleaned"
    );
}

#[test]
fn oversized_recovery_dump_fails_closed() {
    let directory = TestDirectory::create("oversized-dump");
    let dump_path = directory.path().join("oversized.dump");
    let dump = fs::File::create(&dump_path).expect("create dump");
    dump.set_len(MAX_RECOVERY_DUMP_BYTES + 1)
        .expect("extend dump");

    assert!(
        read_recovery_embed_candidates(
            directory.path(),
            "https://cinema.example/watch/42",
            &|| false
        )
        .expect("oversize is a closed recovery result")
        .candidates
        .is_empty()
    );
}

#[test]
fn too_many_recovery_dumps_fail_closed() {
    let directory = TestDirectory::create("too-many-dumps");
    for index in 0..=MAX_RECOVERY_DUMP_FILES {
        fs::write(
            directory.path().join(format!("{index}.dump")),
            r#"<iframe src="https://anonymous.example/player/42"></iframe>"#,
        )
        .expect("write dump");
    }

    assert!(
        read_recovery_embed_candidates(
            directory.path(),
            "https://cinema.example/watch/42",
            &|| false
        )
        .expect("file-count overflow is a closed recovery result")
        .candidates
        .is_empty()
    );
}

#[test]
fn recovery_dump_scan_preserves_cancellation() {
    let directory = TestDirectory::create("cancelled-scan");
    fs::write(
        directory.path().join("page.dump"),
        r#"<iframe src="https://anonymous.example/player/42"></iframe>"#,
    )
    .expect("write dump");

    let error = read_recovery_embed_candidates(
        directory.path(),
        "https://cinema.example/watch/42",
        &|| true,
    )
    .expect_err("cancelled scan must not become an empty recovery");
    assert!(matches!(error, YtDlpServiceError::Cancellation));
}

#[test]
fn title_enrichment_replaces_only_missing_blank_or_generic_title() {
    for original_title in [None, Some(""), Some(" VIDEO ")] {
        let mut document = serde_json::json!({"title": original_title});
        enrich_recovered_document_title(&mut document, Some("Catalog title"));
        assert_eq!(
            document.get("title").and_then(Value::as_str),
            Some("Catalog title")
        );
    }

    let mut document = serde_json::json!({"title": "Extractor title"});
    enrich_recovered_document_title(&mut document, Some("Catalog title"));
    assert_eq!(
        document.get("title").and_then(Value::as_str),
        Some("Extractor title")
    );
}

/// Проверяет, что stdout сохраняется, а stderr только считается до завершения процесса.
#[test]
fn process_output_collects_stdout_and_stderr() {
    let output = run_process_with_timeout(
        "sh",
        &["-c", "printf stdout-text; printf stderr-text >&2"],
        Duration::from_secs(1),
    )
    .expect("shell output captured");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"stdout-text");
    assert_eq!(output.stderr_bytes, b"stderr-text".len());
}

/// Ровно разрешённое число stdout bytes остаётся успешным process result.
#[test]
fn process_stdout_exact_boundary_succeeds() {
    let output = run_process_with_timeout_and_cancellation(
        "sh",
        &["-c", "printf 1234"],
        None,
        Duration::from_secs(1),
        explicit_output_budgets(4, 16, 16),
        &|| false,
    )
    .expect("stdout exact boundary должен быть допустим");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"1234");
}

/// Первый byte сверх stdout budget немедленно завершает всю process group.
#[cfg(unix)]
#[test]
fn process_stdout_limit_plus_one_is_typed_and_stops_descendant() {
    let fixture_directory = TestDirectory::create("stdout-overflow-descendant");
    let descendant_pid_record = fixture_directory.path().join("descendant-pid");
    let script = format!(
        "#!/bin/sh\nsleep 30 &\ndescendant_pid=$!\nprintf '%s' \"$descendant_pid\" > '{}'\nprintf 12345\nwait\n",
        descendant_pid_record.display()
    );
    let executable = create_executable_test_script(fixture_directory.path(), &script)
        .expect("create stdout boundary fixture");
    let started_at = Instant::now();

    let error = run_process_with_timeout_and_cancellation(
        executable.to_str().expect("UTF-8 executable path"),
        &[],
        None,
        Duration::from_secs(5),
        explicit_output_budgets(4, 16, 16),
        &|| false,
    )
    .expect_err("stdout limit + 1 должен остановить process");

    assert!(matches!(
        error,
        YtDlpServiceError::StdoutLimitExceeded { limit_bytes: 4 }
    ));
    assert!(started_at.elapsed() < Duration::from_secs(2));
    let descendant_pid = fs::read_to_string(descendant_pid_record)
        .expect("fixture должен записать descendant PID")
        .trim()
        .parse::<libc::pid_t>()
        .expect("fixture descendant PID должен быть числом");
    assert!(
        !process_id_is_running(descendant_pid),
        "stdout overflow должен остановить descendant той же process group"
    );
}

/// Stderr exact boundary считается без сохранения payload.
#[test]
fn process_stderr_exact_boundary_succeeds_without_payload_capture() {
    let output = run_process_with_timeout_and_cancellation(
        "sh",
        &["-c", "printf 1234 >&2"],
        None,
        Duration::from_secs(1),
        explicit_output_budgets(16, 4, 16),
        &|| false,
    )
    .expect("stderr exact boundary должен быть допустим");

    assert!(output.status.success());
    assert_eq!(output.stderr_bytes, 4);
    assert!(format!("{output:?}").contains("<redacted:4 bytes>"));
}

/// Первый byte сверх stderr budget сохраняет отдельную typed identity.
#[test]
fn process_stderr_limit_plus_one_is_typed() {
    let error = run_process_with_timeout_and_cancellation(
        "sh",
        &["-c", "printf 12345 >&2; sleep 30"],
        None,
        Duration::from_secs(5),
        explicit_output_budgets(16, 4, 16),
        &|| false,
    )
    .expect_err("stderr limit + 1 должен остановить process");

    assert!(matches!(
        error,
        YtDlpServiceError::StderrLimitExceeded { limit_bytes: 4 }
    ));
}

/// Большой допустимый JSON проходит process, DTO и normalization до рабочего candidate-а.
#[cfg(unix)]
#[test]
fn large_valid_single_item_reaches_normalized_candidate_snapshot() {
    use crate::candidate::{YtDlpCandidateDocument, normalize_candidate_document};
    use web_media_core::{ExtractionGeneration, SourceIdentity};

    let fixture_directory = TestDirectory::create("large-valid-candidate");
    let script = r#"#!/bin/sh
printf '%s' '{"title":"Large valid profile","duration":1,"formats":[{"format_id":"18","url":"https://media.invalid/18","protocol":"https","ext":"mp4","container":"mp4","vcodec":"avc1.42001E","acodec":"mp4a.40.2","dynamic_range":"SDR"}],"unused_padding":"'
head -c 1048576 /dev/zero | tr '\0' x
printf '%s' '"}'
"#;
    let executable = create_executable_test_script(fixture_directory.path(), script)
        .expect("create large candidate fixture");
    let process_config = YtDlpProcessConfig {
        executable: executable.to_string_lossy().into_owned(),
        timeout: Duration::from_secs(5),
        output_budgets: explicit_output_budgets(2 * 1024 * 1024, 1024, 128),
    };
    let locator = crate::parse_yt_dlp_media_locator("https://media.invalid/item")
        .expect("valid candidate fixture locator");

    let document: YtDlpCandidateDocument =
        resolve_yt_dlp_candidate_document_with_cancellation(&locator, &process_config, &|| false)
            .expect("large valid JSON должен пройти process и DTO boundaries");
    let snapshot = normalize_candidate_document(
        document,
        SourceIdentity::new(7007),
        ExtractionGeneration::new(1),
    );

    assert_eq!(snapshot.inventory().len(), 1);
    assert_eq!(snapshot.accepted_candidates().count(), 1);
}

/// Нормальный root exit очищает lingering descendant до join унаследованных pipe-ов.
#[cfg(unix)]
#[test]
fn process_normal_root_exit_does_not_wait_for_lingering_descendant() {
    let started_at = Instant::now();
    let output = run_process_with_timeout(
        "sh",
        &["-c", "sleep 30 & printf root-exited"],
        Duration::from_secs(2),
    )
    .expect("normal root exit must retain successful output");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"root-exited");
    assert!(
        started_at.elapsed() < Duration::from_secs(1),
        "lingering descendant must be killed before pipe-reader join"
    );
}

/// Descendant вне owned PGID не превращается в success и не блокирует pipe drain.
#[cfg(unix)]
#[test]
fn process_escaped_process_group_pipe_holder_fails_bounded() {
    let fixture_directory = TestDirectory::create("escaped-process-group");
    let process_id_record = fixture_directory.path().join("escaped-pid");
    let escaped_process_guard = EscapedProcessGuard::new(process_id_record.clone());
    let shell_command = format!(
        "setsid sh -c 'echo $$ > \"{}\"; exec sleep 30' & \
         while [ ! -s \"{}\" ]; do sleep 0.01; done; \
         printf root-exited",
        process_id_record.display(),
        process_id_record.display()
    );

    let started_at = Instant::now();
    let process_result = run_process_with_timeout(
        "sh",
        &["-c", shell_command.as_str()],
        Duration::from_secs(2),
    );
    assert!(
        escaped_process_guard.wait_for_process_id().is_some(),
        "escaped fixture must publish its PID before root exit"
    );
    let error = process_result.expect_err("escaped pipe holder must not look successful");

    assert!(matches!(error, YtDlpServiceError::ProcessFailure { .. }));
    assert!(
        started_at.elapsed() < Duration::from_secs(2),
        "escaped pipe holder must hit bounded drain instead of sleep duration"
    );
}

/// Transient Unix `ETXTBSY` повторяется и всё равно доходит до выполнения executable.
#[cfg(unix)]
#[test]
fn process_spawn_retries_temporary_text_file_busy_and_executes() {
    let fixture_directory = TestDirectory::create("spawn-text-file-busy");
    let executable = create_executable_test_script(
        fixture_directory.path(),
        "#!/bin/sh\nprintf 'spawn-retry-ok'\n",
    )
    .expect("create retry executable");
    let executable_writer = fs::OpenOptions::new()
        .write(true)
        .open(&executable)
        .expect("hold executable open for writing");

    let initial_error = Command::new(&executable)
        .spawn()
        .expect_err("writer-open executable must initially fail to spawn");
    assert_eq!(initial_error.raw_os_error(), Some(libc::ETXTBSY));

    let writer_release = thread::spawn(move || {
        thread::sleep(Duration::from_millis(35));
        drop(executable_writer);
    });
    let process_result = run_process_with_timeout(
        executable.to_str().expect("UTF-8 executable path"),
        &[],
        Duration::from_secs(1),
    );
    writer_release
        .join()
        .expect("writer release thread must not panic");

    let output = process_result.expect("ETXTBSY retry must reach executable output");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"spawn-retry-ok");
}

/// Исчерпанный retry сохраняет исходный typed process failure и остаётся bounded.
#[cfg(unix)]
#[test]
fn process_spawn_text_file_busy_exhaustion_preserves_process_failure() {
    let fixture_directory = TestDirectory::create("spawn-text-file-busy-exhausted");
    let executable = create_executable_test_script(
        fixture_directory.path(),
        "#!/bin/sh\nprintf 'must-not-run'\n",
    )
    .expect("create exhausted-retry executable");
    let _executable_writer = fs::OpenOptions::new()
        .write(true)
        .open(&executable)
        .expect("hold executable open for all attempts");

    let started_at = Instant::now();
    let error = run_process_with_timeout(
        executable.to_str().expect("UTF-8 executable path"),
        &[],
        Duration::from_secs(1),
    )
    .expect_err("exhausted ETXTBSY retry must fail");

    let YtDlpServiceError::ProcessFailure { source } = error else {
        panic!("exhausted ETXTBSY retry returned {error:?}");
    };
    assert_eq!(
        source
            .downcast_ref::<io::Error>()
            .and_then(io::Error::raw_os_error),
        Some(libc::ETXTBSY)
    );
    assert!(
        started_at.elapsed() < Duration::from_millis(500),
        "fixed spawn retry budget must not consume the one-second process timeout"
    );
}

/// Cooperative cancellation проверяется между transient spawn-попытками.
#[cfg(unix)]
#[test]
fn process_spawn_text_file_busy_retry_preserves_cancellation() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let fixture_directory = TestDirectory::create("spawn-text-file-busy-cancelled");
    let executable = create_executable_test_script(
        fixture_directory.path(),
        "#!/bin/sh\nprintf 'must-not-run'\n",
    )
    .expect("create cancelled-retry executable");
    let _executable_writer = fs::OpenOptions::new()
        .write(true)
        .open(&executable)
        .expect("hold executable open until cancellation");
    let cancellation_checks = AtomicUsize::new(0);

    let error = run_process_with_timeout_and_cancellation(
        executable.to_str().expect("UTF-8 executable path"),
        &[],
        None,
        Duration::from_secs(1),
        test_output_budgets(),
        &|| cancellation_checks.fetch_add(1, Ordering::Relaxed) >= 2,
    )
    .expect_err("cancellation between ETXTBSY attempts must remain typed");

    assert!(matches!(error, YtDlpServiceError::Cancellation));
    assert!(cancellation_checks.load(Ordering::Relaxed) >= 3);
}

/// Исчерпанный общий timeout запрещает повторный spawn даже после освобождения writer-а.
#[cfg(unix)]
#[test]
fn process_spawn_text_file_busy_does_not_retry_after_deadline() {
    use std::cell::{Cell, RefCell};

    let fixture_directory = TestDirectory::create("spawn-text-file-busy-deadline");
    let execution_marker = fixture_directory.path().join("executed");
    let executable = create_executable_test_script(
        fixture_directory.path(),
        &format!(
            "#!/bin/sh\nprintf executed > '{}'\n",
            execution_marker.display()
        ),
    )
    .expect("create deadline executable");
    let executable_writer = RefCell::new(Some(
        fs::OpenOptions::new()
            .write(true)
            .open(&executable)
            .expect("hold executable open for first attempt"),
    ));
    let cancellation_checks = Cell::new(0_usize);

    let error = run_process_with_timeout_and_cancellation(
        executable.to_str().expect("UTF-8 executable path"),
        &[],
        None,
        Duration::from_millis(5),
        test_output_budgets(),
        &|| {
            let next_check = cancellation_checks.get() + 1;
            cancellation_checks.set(next_check);
            if next_check == 3 {
                executable_writer.borrow_mut().take();
            }
            false
        },
    )
    .expect_err("deadline must preserve the first ETXTBSY failure");

    assert!(matches!(error, YtDlpServiceError::ProcessFailure { .. }));
    assert!(
        !execution_marker.exists(),
        "executable must not run in a retry started after the deadline"
    );
}

/// Проверяет, что зависший process ограничивается timeout-ом.
#[test]
fn process_timeout_stops_slow_child() {
    let error = run_process_with_timeout(
        "sh",
        &[
            "-c",
            "printf 'https://user:password@example.test?v=secret' >&2; sleep 1",
        ],
        Duration::from_millis(25),
    )
    .expect_err("slow process times out");

    assert!(matches!(error, YtDlpServiceError::Timeout));
    assert!(!error.to_string().contains("password"));
    assert!(!error.to_string().contains("secret"));
}

/// Cooperative cancellation должен быстро остановить уже запущенный child process.
#[test]
fn process_cancellation_stops_running_child() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let cancellation_checks = AtomicUsize::new(0);
    let started_at = Instant::now();
    let error = run_process_with_timeout_and_cancellation(
        "sh",
        &["-c", "sleep 5"],
        None,
        Duration::from_secs(5),
        test_output_budgets(),
        &|| cancellation_checks.fetch_add(1, Ordering::Relaxed) > 0,
    )
    .expect_err("cancelled process must not complete successfully");

    assert!(error.to_string().contains("отмен"));
    assert!(started_at.elapsed() < Duration::from_secs(1));
}

#[test]
fn failed_process_error_redacts_and_bounds_stderr() {
    let output = run_process_with_timeout(
        "sh",
        &[
            "-c",
            "printf 'https://user:password@example.test/watch?v=secret' >&2; exit 1",
        ],
        Duration::from_secs(1),
    )
    .expect("test process должен завершиться обычным non-zero status");

    let output_debug = format!("{output:?}");
    assert!(!output_debug.contains("password"));
    assert!(!output_debug.contains("secret"));

    let error = ensure_yt_dlp_candidate_success(output.status, output.stderr_bytes)
        .expect_err("non-zero status должен стать typed extractor error");
    let formatted = format!("{error:?} {error}");
    assert!(!formatted.contains("password"));
    assert!(!formatted.contains("secret"));
    assert!(!formatted.contains("example.test"));
    assert!(formatted.contains("stderr скрыт"));
    assert!(formatted.len() < 512);
}
