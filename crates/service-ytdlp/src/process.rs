use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustiplayer_config::YtDlpConfig;
use serde::de::DeserializeOwned;

use crate::dto::YtDlpMetadata;
use crate::error::YtDlpServiceError;

/// Имя production binary, через который service получает direct stream metadata.
const YT_DLP_EXECUTABLE: &str = "yt-dlp";

/// Интервал polling-а child process: маленький, но без busy-loop.
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Верхняя граница счётчика stderr в safe diagnostic context.
const MAX_REPORTED_STDERR_BYTES: usize = 1_048_576;

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
    video_url: &str,
    process_config: &YtDlpProcessConfig,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<YtDlpMetadata, YtDlpServiceError> {
    resolve_yt_dlp_candidate_document_with_cancellation(video_url, process_config, is_cancelled)
}

/// Единственный process path для typed candidate JSON consumers.
pub(crate) fn resolve_yt_dlp_candidate_document_with_cancellation<T: DeserializeOwned>(
    video_url: &str,
    process_config: &YtDlpProcessConfig,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<T, YtDlpServiceError> {
    let command_arguments = [
        "--quiet",
        "--no-warnings",
        "--simulate",
        "--dump-single-json",
        "--no-playlist",
        video_url,
    ];

    let command_output = run_process_with_timeout_and_cancellation(
        process_config.executable.as_str(),
        &command_arguments,
        process_config.timeout,
        is_cancelled,
    )?;

    ensure_yt_dlp_candidate_success(command_output.status, &command_output.stderr)?;

    let stdout_text =
        String::from_utf8(command_output.stdout).map_err(YtDlpServiceError::invalid_response)?;

    serde_json::from_str(&stdout_text).map_err(YtDlpServiceError::invalid_response)
}

/// Запускает внешний процесс, читает stdout/stderr параллельно и ограничивает ожидание.
#[cfg(test)]
fn run_process_with_timeout(
    executable: &str,
    arguments: &[&str],
    timeout: Duration,
) -> Result<ProcessOutput, YtDlpServiceError> {
    run_process_with_timeout_and_cancellation(executable, arguments, timeout, &|| false)
}

/// Запускает внешний процесс с timeout-ом и cooperative cancellation.
fn run_process_with_timeout_and_cancellation(
    executable: &str,
    arguments: &[&str],
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

    let mut child = Command::new(executable)
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(YtDlpServiceError::process)?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| YtDlpServiceError::process(anyhow::anyhow!("stdout pipe недоступен")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| YtDlpServiceError::process(anyhow::anyhow!("stderr pipe недоступен")))?;
    let stdout_reader = spawn_pipe_reader("stdout", stdout)?;
    let stderr_reader = spawn_pipe_reader("stderr", stderr)?;

    let wait_outcome = wait_for_process_with_timeout(&mut child, timeout, is_cancelled)?;
    let stdout = join_pipe_reader("stdout", stdout_reader)?;
    let stderr = join_pipe_reader("stderr", stderr_reader)?;

    match wait_outcome {
        ProcessWaitOutcome::Exited(status) => Ok(ProcessOutput {
            status,
            stdout,
            stderr,
        }),
        ProcessWaitOutcome::TimedOut => Err(YtDlpServiceError::Timeout),
        ProcessWaitOutcome::Cancelled => Err(YtDlpServiceError::Cancellation),
    }
}

/// Запускает thread, который вычитывает pipe до EOF и предотвращает заполнение OS buffer-а.
fn spawn_pipe_reader<R>(
    pipe_name: &'static str,
    mut pipe: R,
) -> Result<thread::JoinHandle<io::Result<Vec<u8>>>, YtDlpServiceError>
where
    R: Read + Send + 'static,
{
    thread::Builder::new()
        .name(format!("yt-dlp-{pipe_name}"))
        .spawn(move || {
            let mut captured_bytes = Vec::new();
            pipe.read_to_end(&mut captured_bytes)?;
            Ok(captured_bytes)
        })
        .map_err(YtDlpServiceError::process)
}

/// Забирает bytes из pipe reader thread и превращает panic/thread IO в понятную ошибку.
fn join_pipe_reader(
    pipe_name: &'static str,
    reader: thread::JoinHandle<io::Result<Vec<u8>>>,
) -> Result<Vec<u8>, YtDlpServiceError> {
    reader
        .join()
        .map_err(|_| {
            YtDlpServiceError::process(anyhow::anyhow!(
                "reader thread для {pipe_name} завершился panic"
            ))
        })?
        .map_err(YtDlpServiceError::process)
}

/// Ждёт child process с bounded polling и убивает его при превышении timeout-а.
fn wait_for_process_with_timeout(
    child: &mut Child,
    timeout: Duration,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ProcessWaitOutcome, YtDlpServiceError> {
    let start = Instant::now();

    loop {
        if let Some(status) = child.try_wait().map_err(YtDlpServiceError::process)? {
            return Ok(ProcessWaitOutcome::Exited(status));
        }

        if is_cancelled() {
            terminate_process(child)?;
            return Ok(ProcessWaitOutcome::Cancelled);
        }

        if start.elapsed() >= timeout {
            terminate_process(child)?;
            return Ok(ProcessWaitOutcome::TimedOut);
        }

        let remaining_timeout = timeout.saturating_sub(start.elapsed());
        thread::sleep(remaining_timeout.min(PROCESS_POLL_INTERVAL));
    }
}

/// Останавливает зависший child process и reaps его, чтобы не оставлять zombie process.
fn terminate_process(child: &mut Child) -> Result<(), YtDlpServiceError> {
    match child.kill() {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
        Err(error) => return Err(YtDlpServiceError::process(error)),
    }

    child.wait().map_err(YtDlpServiceError::process)?;
    Ok(())
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
