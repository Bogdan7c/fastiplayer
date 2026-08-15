use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustiplayer_config::YtDlpConfig;
use serde::de::DeserializeOwned;
use serde_json::Value;
use url::Url;

use crate::dto::YtDlpMetadata;
use crate::embed_recovery::{
    GenericExtractorImpersonation, candidate_arguments, discover_non_platform_embed_urls,
    discover_page_title, should_attempt_platform_embed_recovery, write_pages_arguments,
};
use crate::error::YtDlpServiceError;
use crate::locator::YtDlpMediaLocator;
use crate::process_tree::{
    OwnedPipe, OwnedPipeDrainError, OwnedPipeReader, OwnedProcess, OwnedProcessCleanupFailure,
    OwnedProcessRootState, OwnedProcessSpawnError, spawn_owned_pipe_reader, spawn_owned_process,
};

/// Имя production binary, через который service получает direct stream metadata.
const YT_DLP_EXECUTABLE: &str = "yt-dlp";

/// Интервал polling-а child process: маленький, но без busy-loop.
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Верхняя граница счётчика stderr в safe diagnostic context.
const MAX_REPORTED_STDERR_BYTES: usize = 1_048_576;

const MAX_RECOVERY_DUMP_FILES: usize = 8;
const MAX_RECOVERY_DUMP_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RECOVERY_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
static RECOVERY_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Process-owned каталог с безусловной best-effort очисткой при выходе из scope.
struct RecoveryTempDirectory {
    path: PathBuf,
}

impl RecoveryTempDirectory {
    fn create() -> Result<Self, YtDlpServiceError> {
        let base = std::env::temp_dir();
        for _ in 0..16 {
            let sequence = RECOVERY_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = base.join(format!(
                "rustiplayer-ytdlp-recovery-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            let mut directory_builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                directory_builder.mode(0o700);
            }
            match directory_builder.create(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(YtDlpServiceError::process(error)),
            }
        }

        Err(YtDlpServiceError::process(anyhow::anyhow!(
            "не удалось выделить уникальный recovery-каталог"
        )))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RecoveryTempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Runtime policy запуска `yt-dlp`, отделённая от parsing/selection логики.
#[derive(Debug, Clone)]
pub(crate) struct YtDlpProcessConfig {
    /// Имя или путь к executable.
    executable: String,

    /// Верхняя граница ожидания metadata command.
    timeout: Duration,
}

impl YtDlpProcessConfig {
    /// Строит process policy из пользовательского YtDlp config.
    pub(crate) fn from_yt_dlp_config(
        yt_dlp_config: &YtDlpConfig,
    ) -> Result<Self, YtDlpServiceError> {
        if yt_dlp_config.resolve_timeout_ms == 0 {
            return Err(YtDlpServiceError::process(anyhow::anyhow!(
                "yt_dlp.resolve_timeout_ms должен быть положительным"
            )));
        }

        Ok(Self {
            executable: YT_DLP_EXECUTABLE.to_string(),
            timeout: Duration::from_millis(yt_dlp_config.resolve_timeout_ms),
        })
    }

    /// Строит ту же runtime policy, переводя только config validation в topology error.
    pub(crate) fn from_yt_dlp_config_for_topology(
        yt_dlp_config: &YtDlpConfig,
    ) -> Result<Self, crate::topology::YtDlpTopologyError> {
        if yt_dlp_config.resolve_timeout_ms == 0 {
            return Err(crate::topology::YtDlpTopologyError::process(
                anyhow::anyhow!("yt_dlp.resolve_timeout_ms должен быть положительным"),
            ));
        }

        Ok(Self {
            executable: YT_DLP_EXECUTABLE.to_string(),
            timeout: Duration::from_millis(yt_dlp_config.resolve_timeout_ms),
        })
    }

    /// Возвращает executable только process owner-у для spawn.
    pub(crate) fn executable_for_spawn(&self) -> &str {
        self.executable.as_str()
    }

    /// Возвращает validated extraction timeout.
    pub(crate) const fn extraction_timeout(&self) -> Duration {
        self.timeout
    }
}

/// Собранный stdout/stderr внешнего процесса.
struct ProcessOutput {
    /// Exit status, полученный от OS.
    status: ExitStatus,

    /// Полный stdout процесса.
    stdout: Vec<u8>,

    /// Полный stderr процесса.
    stderr: Vec<u8>,
}

#[derive(Debug)]
struct RecoveryDumpEvidence {
    candidates: Vec<String>,
    page_title: Option<String>,
}

impl std::fmt::Debug for ProcessOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessOutput")
            .field("status", &self.status)
            .field(
                "stdout",
                &format_args!("<redacted:{} bytes>", self.stdout.len()),
            )
            .field(
                "stderr",
                &format_args!("<redacted:{} bytes>", self.stderr.len()),
            )
            .finish()
    }
}

/// Результат ожидания child process.
enum ProcessWaitOutcome {
    /// Процесс завершился сам.
    Exited(ExitStatus),

    /// Процесс был остановлен из-за timeout-а.
    TimedOut,

    /// Процесс был остановлен по cooperative cancellation от owner-а.
    Cancelled,
}

/// Получает общий manifest JSON без playback selector-а и поддерживает отмену.
pub(crate) fn resolve_yt_dlp_candidate_metadata_with_cancellation(
    locator: &YtDlpMediaLocator,
    process_config: &YtDlpProcessConfig,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<YtDlpMetadata, YtDlpServiceError> {
    resolve_yt_dlp_candidate_document_with_cancellation(locator, process_config, is_cancelled)
}

/// Единственный process path для typed candidate JSON consumers.
pub(crate) fn resolve_yt_dlp_candidate_document_with_cancellation<T: DeserializeOwned>(
    locator: &YtDlpMediaLocator,
    process_config: &YtDlpProcessConfig,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<T, YtDlpServiceError> {
    let video_url = locator.expose_secret_for_open();
    let impersonation = GenericExtractorImpersonation::for_input_scheme(locator.input_scheme());
    let primary_document =
        run_dump_single_json(video_url, impersonation, process_config, is_cancelled)?;
    let document = match recover_playable_document_after_platform_hijack(
        video_url,
        &primary_document,
        process_config,
        is_cancelled,
    ) {
        Ok(Some(recovery_document)) => recovery_document,
        // Cancellation остаётся обязательным lifecycle signal, а не
        // extractor failure, который можно безопасно заменить primary.
        Err(YtDlpServiceError::Cancellation) => {
            return Err(YtDlpServiceError::Cancellation);
        }
        Ok(None) | Err(_) => primary_document,
    };

    serde_json::from_value(document).map_err(YtDlpServiceError::invalid_response)
}

/// Общая candidate/topology граница восстановления после подтверждённого platform hijack.
pub(crate) fn recover_playable_document_after_platform_hijack(
    input_url: &str,
    primary_document: &Value,
    process_config: &YtDlpProcessConfig,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<Value>, YtDlpServiceError> {
    if !should_attempt_platform_embed_recovery(input_url, primary_document) {
        return Ok(None);
    }

    recover_non_platform_embed(input_url, process_config, is_cancelled)
}

fn run_dump_single_json(
    video_url: &str,
    impersonation: GenericExtractorImpersonation,
    process_config: &YtDlpProcessConfig,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Value, YtDlpServiceError> {
    let command_arguments = candidate_arguments(video_url, impersonation);
    let command_output = run_process_with_timeout_and_cancellation(
        process_config.executable.as_str(),
        &command_arguments,
        None,
        process_config.timeout,
        is_cancelled,
    )?;

    ensure_yt_dlp_candidate_success(command_output.status, &command_output.stderr)?;

    let stdout_text =
        String::from_utf8(command_output.stdout).map_err(YtDlpServiceError::invalid_response)?;
    serde_json::from_str(&stdout_text).map_err(YtDlpServiceError::invalid_response)
}

fn recover_non_platform_embed(
    input_url: &str,
    process_config: &YtDlpProcessConfig,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<Value>, YtDlpServiceError> {
    if is_cancelled() {
        return Err(YtDlpServiceError::Cancellation);
    }

    let recovery_directory = RecoveryTempDirectory::create()?;
    let write_pages_arguments = write_pages_arguments(input_url);
    let write_pages_output = run_process_with_timeout_and_cancellation(
        process_config.executable.as_str(),
        &write_pages_arguments,
        Some(recovery_directory.path()),
        process_config.timeout,
        is_cancelled,
    )?;
    ensure_yt_dlp_candidate_success(write_pages_output.status, &write_pages_output.stderr)?;

    let evidence =
        read_recovery_embed_candidates(recovery_directory.path(), input_url, is_cancelled)?;
    for embed_url in &evidence.candidates {
        let mut recovered_document = match run_dump_single_json(
            embed_url,
            GenericExtractorImpersonation::RequiredForHttp,
            process_config,
            is_cancelled,
        ) {
            Ok(document) => document,
            Err(YtDlpServiceError::Cancellation) => {
                return Err(YtDlpServiceError::Cancellation);
            }
            Err(_) => continue,
        };
        if should_attempt_platform_embed_recovery(input_url, &recovered_document) {
            continue;
        }
        enrich_recovered_document_title(&mut recovered_document, evidence.page_title.as_deref());
        return Ok(Some(recovered_document));
    }

    Ok(None)
}

fn read_recovery_embed_candidates(
    directory: &Path,
    input_url: &str,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<RecoveryDumpEvidence, YtDlpServiceError> {
    let mut dump_paths = Vec::new();
    for entry in fs::read_dir(directory).map_err(YtDlpServiceError::process)? {
        if is_cancelled() {
            return Err(YtDlpServiceError::Cancellation);
        }
        let entry = entry.map_err(YtDlpServiceError::process)?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "dump")
        {
            dump_paths.push(path);
            if dump_paths.len() > MAX_RECOVERY_DUMP_FILES {
                return Ok(RecoveryDumpEvidence {
                    candidates: Vec::new(),
                    page_title: None,
                });
            }
        }
    }
    dump_paths.sort();

    let mut total_bytes = 0_u64;
    let mut dumped_html = String::new();
    let input_host = Url::parse(input_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
    let mut matching_page_title = None;
    let mut fallback_page_title = None;
    for path in dump_paths {
        if is_cancelled() {
            return Err(YtDlpServiceError::Cancellation);
        }
        let metadata = fs::metadata(&path).map_err(YtDlpServiceError::process)?;
        if !metadata.is_file() || metadata.len() > MAX_RECOVERY_DUMP_BYTES {
            return Ok(RecoveryDumpEvidence {
                candidates: Vec::new(),
                page_title: None,
            });
        }
        total_bytes = total_bytes.saturating_add(metadata.len());
        if total_bytes > MAX_RECOVERY_TOTAL_BYTES {
            return Ok(RecoveryDumpEvidence {
                candidates: Vec::new(),
                page_title: None,
            });
        }

        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        fs::File::open(&path)
            .map_err(YtDlpServiceError::process)?
            .take(MAX_RECOVERY_DUMP_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(YtDlpServiceError::process)?;
        if bytes.len() as u64 > MAX_RECOVERY_DUMP_BYTES {
            return Ok(RecoveryDumpEvidence {
                candidates: Vec::new(),
                page_title: None,
            });
        }
        let html = String::from_utf8_lossy(&bytes);
        if let Some(title) = discover_page_title(&html) {
            fallback_page_title.get_or_insert_with(|| title.clone());
            if matching_page_title.is_none()
                && input_host
                    .as_deref()
                    .is_some_and(|host| html.to_ascii_lowercase().contains(host))
            {
                matching_page_title = Some(title);
            }
        }
        dumped_html.push_str(&html);
        dumped_html.push('\n');
    }
    if is_cancelled() {
        return Err(YtDlpServiceError::Cancellation);
    }
    let candidates = discover_non_platform_embed_urls(&dumped_html);
    if is_cancelled() {
        return Err(YtDlpServiceError::Cancellation);
    }

    Ok(RecoveryDumpEvidence {
        candidates,
        page_title: matching_page_title.or(fallback_page_title),
    })
}

fn enrich_recovered_document_title(document: &mut Value, page_title: Option<&str>) {
    let Some(page_title) = page_title.filter(|title| !title.trim().is_empty()) else {
        return;
    };
    let Some(object) = document.as_object_mut() else {
        return;
    };
    let needs_page_title = object
        .get("title")
        .and_then(Value::as_str)
        .is_none_or(|title| title.trim().is_empty() || title.trim().eq_ignore_ascii_case("video"));
    if needs_page_title {
        object.insert("title".to_owned(), Value::String(page_title.to_owned()));
    }
}

#[cfg(all(test, unix))]
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

#[cfg(test)]
fn test_directory(label: &str) -> PathBuf {
    for _ in 0..16 {
        let sequence = RECOVERY_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
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

#[cfg(test)]
struct TestDirectory(PathBuf);

#[cfg(test)]
impl TestDirectory {
    fn create(label: &str) -> Self {
        Self(test_directory(label))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(test)]
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Запускает внешний процесс, читает stdout/stderr параллельно и ограничивает ожидание.
#[cfg(test)]
fn run_process_with_timeout(
    executable: &str,
    arguments: &[&str],
    timeout: Duration,
) -> Result<ProcessOutput, YtDlpServiceError> {
    run_process_with_timeout_and_cancellation(executable, arguments, None, timeout, &|| false)
}

/// Запускает внешний процесс с timeout-ом и cooperative cancellation.
fn run_process_with_timeout_and_cancellation(
    executable: &str,
    arguments: &[&str],
    current_directory: Option<&Path>,
    timeout: Duration,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ProcessOutput, YtDlpServiceError> {
    if timeout.is_zero() {
        return Err(YtDlpServiceError::process(anyhow::anyhow!(
            "process timeout должен быть положительным"
        )));
    }
    if is_cancelled() {
        return Err(YtDlpServiceError::Cancellation);
    }
    let operation_started_at = Instant::now();

    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(directory) = current_directory {
        command.current_dir(directory);
    }
    let mut process =
        match spawn_owned_process(&mut command, operation_started_at, timeout, is_cancelled) {
            Ok(process) => process,
            Err(OwnedProcessSpawnError::Cancellation) => {
                return Err(YtDlpServiceError::Cancellation);
            }
            Err(OwnedProcessSpawnError::Process(error)) => {
                return Err(YtDlpServiceError::process(error));
            }
        };

    let stdout = match process.take_stdout() {
        Some(stdout) => stdout,
        None => {
            let primary = YtDlpServiceError::process(anyhow::anyhow!("stdout pipe недоступен"));
            return Err(finish_process_after_error(&mut process, primary));
        }
    };
    let stderr = match process.take_stderr() {
        Some(stderr) => stderr,
        None => {
            let primary = YtDlpServiceError::process(anyhow::anyhow!("stderr pipe недоступен"));
            return Err(finish_process_after_error(&mut process, primary));
        }
    };
    let stdout_reader = match spawn_pipe_reader("stdout", stdout) {
        Ok(reader) => reader,
        Err(primary) => return Err(finish_process_after_error(&mut process, primary)),
    };
    let stderr_reader = match spawn_pipe_reader("stderr", stderr) {
        Ok(reader) => reader,
        Err(primary) => {
            if let Err(cleanup) = process.finish() {
                return Err(combine_process_failures(primary, cleanup.into()));
            }
            return match stdout_reader.abort() {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(combine_process_failures(
                    primary,
                    anyhow::Error::new(cleanup),
                )),
            };
        }
    };

    let remaining_timeout = timeout.saturating_sub(operation_started_at.elapsed());
    let wait_result = wait_for_process_with_timeout(&mut process, remaining_timeout, is_cancelled);
    let wait_outcome = match wait_result {
        Ok(outcome) => outcome,
        Err(primary) => {
            return match abort_pipe_readers(stdout_reader, stderr_reader) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(combine_process_failures(
                    primary,
                    anyhow::Error::new(cleanup),
                )),
            };
        }
    };

    match wait_outcome {
        ProcessWaitOutcome::Exited(status) => {
            let pipe_output = drain_pipe_readers(
                stdout_reader,
                stderr_reader,
                operation_started_at,
                timeout,
                is_cancelled,
            )?;
            let (stdout, stderr) = pipe_output;
            Ok(ProcessOutput {
                status,
                stdout,
                stderr,
            })
        }
        ProcessWaitOutcome::TimedOut => match abort_pipe_readers(stdout_reader, stderr_reader) {
            Ok(()) => Err(YtDlpServiceError::Timeout),
            Err(cleanup) => Err(combine_process_failures(
                YtDlpServiceError::Timeout,
                anyhow::Error::new(cleanup),
            )),
        },
        ProcessWaitOutcome::Cancelled => match abort_pipe_readers(stdout_reader, stderr_reader) {
            Ok(()) => Err(YtDlpServiceError::Cancellation),
            Err(cleanup) => Err(combine_process_failures(
                YtDlpServiceError::Cancellation,
                anyhow::Error::new(cleanup),
            )),
        },
    }
}

/// Завершает owner после primary failure, сохраняя обе ошибки при cleanup failure.
fn finish_process_after_error(
    process: &mut OwnedProcess,
    primary: YtDlpServiceError,
) -> YtDlpServiceError {
    match process.finish() {
        Ok(_) => primary,
        Err(cleanup) => combine_process_failures(primary, cleanup.into()),
    }
}

/// Упаковывает primary и дополнительную cleanup/join ошибку без потери причин.
fn combine_process_failures(
    primary: YtDlpServiceError,
    cleanup: anyhow::Error,
) -> YtDlpServiceError {
    YtDlpServiceError::process(OwnedProcessCleanupFailure::new(
        anyhow::Error::new(primary),
        cleanup,
    ))
}

/// Запускает thread, который вычитывает pipe до EOF и предотвращает заполнение OS buffer-а.
fn spawn_pipe_reader<R>(
    pipe_name: &'static str,
    pipe: R,
) -> Result<OwnedPipeReader<Vec<u8>>, YtDlpServiceError>
where
    R: OwnedPipe,
{
    let thread_name = match pipe_name {
        "stdout" => "yt-dlp-stdout",
        "stderr" => "yt-dlp-stderr",
        _ => "yt-dlp-pipe",
    };
    spawn_owned_pipe_reader(thread_name, pipe, |reader| {
        let mut captured_bytes = Vec::new();
        reader.read_to_end(&mut captured_bytes)?;
        Ok(captured_bytes)
    })
    .map_err(YtDlpServiceError::process)
}

fn map_pipe_drain_error(error: OwnedPipeDrainError) -> YtDlpServiceError {
    match error {
        OwnedPipeDrainError::Cancellation => YtDlpServiceError::Cancellation,
        OwnedPipeDrainError::OperationTimedOut => YtDlpServiceError::Timeout,
        OwnedPipeDrainError::CancellationCleanup { source } => {
            combine_process_failures(YtDlpServiceError::Cancellation, anyhow::Error::new(source))
        }
        OwnedPipeDrainError::OperationTimeoutCleanup { source } => {
            combine_process_failures(YtDlpServiceError::Timeout, anyhow::Error::new(source))
        }
        other => YtDlpServiceError::process(other),
    }
}

/// Bounded drain обоих pipe-reader-ов с одним operation deadline и grace budget.
fn drain_pipe_readers(
    stdout_reader: OwnedPipeReader<Vec<u8>>,
    stderr_reader: OwnedPipeReader<Vec<u8>>,
    operation_started_at: Instant,
    operation_timeout: Duration,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(Vec<u8>, Vec<u8>), YtDlpServiceError> {
    let drain_started_at = Instant::now();
    let stdout_result = stdout_reader
        .drain(
            operation_started_at,
            operation_timeout,
            drain_started_at,
            is_cancelled,
        )
        .map_err(map_pipe_drain_error);
    let stderr_result = stderr_reader
        .drain(
            operation_started_at,
            operation_timeout,
            drain_started_at,
            is_cancelled,
        )
        .map_err(map_pipe_drain_error);

    match (stdout_result, stderr_result) {
        (Ok(stdout), Ok(stderr)) => Ok((stdout, stderr)),
        (Err(primary), Ok(_)) | (Ok(_), Err(primary)) => Err(primary),
        (Err(primary), Err(cleanup)) => Err(combine_process_failures(
            primary,
            anyhow::Error::new(cleanup),
        )),
    }
}

/// Bounded останавливает оба reader worker-а после non-success process outcome.
fn abort_pipe_readers(
    stdout_reader: OwnedPipeReader<Vec<u8>>,
    stderr_reader: OwnedPipeReader<Vec<u8>>,
) -> Result<(), YtDlpServiceError> {
    let stdout_result = stdout_reader.abort().map_err(YtDlpServiceError::process);
    let stderr_result = stderr_reader.abort().map_err(YtDlpServiceError::process);

    match (stdout_result, stderr_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) | (Ok(()), Err(primary)) => Err(primary),
        (Err(primary), Err(cleanup)) => Err(combine_process_failures(
            primary,
            anyhow::Error::new(cleanup),
        )),
    }
}

/// Ждёт child process с bounded polling и убивает его при превышении timeout-а.
fn wait_for_process_with_timeout(
    process: &mut OwnedProcess,
    timeout: Duration,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ProcessWaitOutcome, YtDlpServiceError> {
    let start = Instant::now();

    loop {
        match process.poll_root_exit() {
            Ok(OwnedProcessRootState::Exited) => {
                let status = process.finish().map_err(YtDlpServiceError::process)?;
                return Ok(ProcessWaitOutcome::Exited(status));
            }
            Ok(OwnedProcessRootState::Running) => {}
            Err(error) => {
                let primary = YtDlpServiceError::process(error);
                return Err(finish_process_after_error(process, primary));
            }
        }

        if is_cancelled() {
            process.finish().map_err(|cleanup| {
                combine_process_failures(YtDlpServiceError::Cancellation, cleanup.into())
            })?;
            return Ok(ProcessWaitOutcome::Cancelled);
        }

        if start.elapsed() >= timeout {
            process.finish().map_err(|cleanup| {
                combine_process_failures(YtDlpServiceError::Timeout, cleanup.into())
            })?;
            return Ok(ProcessWaitOutcome::TimedOut);
        }

        let remaining_timeout = timeout.saturating_sub(start.elapsed());
        thread::sleep(remaining_timeout.min(PROCESS_POLL_INTERVAL));
    }
}

/// Преобразует ошибку metadata-only candidates command в читаемую ошибку.
fn ensure_yt_dlp_candidate_success(
    status: ExitStatus,
    stderr_bytes: &[u8],
) -> Result<(), YtDlpServiceError> {
    if status.success() {
        return Ok(());
    }

    Err(YtDlpServiceError::ExtractorRejection {
        stderr_bytes: stderr_bytes.len().min(MAX_REPORTED_STDERR_BYTES),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
                if probe_result == -1
                    && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
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
        };

        let locator = crate::parse_yt_dlp_media_locator("https://cinema.example/watch/42")
            .expect("parse recovery test locator");
        let document: Value =
            resolve_yt_dlp_candidate_document_with_cancellation(&locator, &process_config, &|| {
                false
            })
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

    /// Проверяет, что stdout/stderr читаются до завершения процесса.
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
        assert_eq!(output.stderr, b"stderr-text");
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

        let error = ensure_yt_dlp_candidate_success(output.status, &output.stderr)
            .expect_err("non-zero status должен стать typed extractor error");
        let formatted = format!("{error:?} {error}");
        assert!(!formatted.contains("password"));
        assert!(!formatted.contains("secret"));
        assert!(!formatted.contains("example.test"));
        assert!(formatted.contains("stderr скрыт"));
        assert!(formatted.len() < 512);
    }
}
