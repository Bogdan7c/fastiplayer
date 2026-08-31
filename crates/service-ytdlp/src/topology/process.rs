//! Bounded child-process owner для line-delimited topology extraction.

use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::embed_recovery::{GENERIC_IMPERSONATE_EXTRACTOR_ARGS, GenericExtractorImpersonation};
use crate::invocation::{ExtractorProcessInvocation, ExtractorProcessLauncher};
use crate::process_tree::{
    OwnedPipeDrainError, OwnedPipeReader, OwnedProcess, OwnedProcessCleanupFailure,
    OwnedProcessRootState, OwnedProcessSpawnError, spawn_owned_pipe_reader,
    spawn_owned_process_with_launcher,
};

use super::limits::{YtDlpTopologyBudgets, YtDlpTopologyError};

/// Poll interval сохраняет responsive cancellation без busy-spin.
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Fixed read chunk исключает allocation по длине недоверенной line.
const PIPE_READ_CHUNK_BYTES: usize = 8 * 1024;

/// `--dump-json` печатает lazy entries, а `--dump-single-json` — final root.
///
/// Порядок safety-critical и закреплён exact-argv focused test-ом.
const TOPOLOGY_ARGUMENTS_BEFORE_POLICY: [&str; 7] = [
    "--quiet",
    "--no-warnings",
    "--simulate",
    "--dump-json",
    "--dump-single-json",
    "--flat-playlist",
    "--lazy-playlist",
];

/// Успешный bounded process result.
pub(crate) struct TopologyProcessOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout_lines: Vec<Vec<u8>>,
    pub(crate) stderr_bytes: usize,
}

impl std::fmt::Debug for TopologyProcessOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TopologyProcessOutput")
            .field("status", &self.status)
            .field("stdout_line_count", &self.stdout_lines.len())
            .field("stderr_bytes", &self.stderr_bytes)
            .finish()
    }
}

/// Запускает exact app-owned topology argv.
#[cfg(test)]
pub(crate) fn run_topology_process(
    executable: &str,
    exact_locator: &str,
    impersonation: GenericExtractorImpersonation,
    timeout: Duration,
    budgets: YtDlpTopologyBudgets,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<TopologyProcessOutput, YtDlpTopologyError> {
    let adapter = crate::YtDlpExtractorAdapter::default();
    let launcher = adapter.process_launcher();
    run_topology_process_with_invocation(
        executable,
        exact_locator,
        impersonation,
        timeout,
        budgets,
        launcher.as_ref(),
        ExtractorProcessInvocation::new(
            web_media_core::ExtractorInvocationReason::CollectionTopologyResolution,
            crate::ExtractorProcessPhase::TopologyPrimary,
        ),
        is_cancelled,
    )
}

/// Production topology path с explicit injected launcher и invocation event.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_topology_process_with_invocation(
    executable: &str,
    exact_locator: &str,
    impersonation: GenericExtractorImpersonation,
    timeout: Duration,
    budgets: YtDlpTopologyBudgets,
    process_launcher: &dyn ExtractorProcessLauncher,
    invocation: ExtractorProcessInvocation,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<TopologyProcessOutput, YtDlpTopologyError> {
    if timeout.is_zero() {
        return Err(YtDlpTopologyError::process(anyhow::anyhow!(
            "process timeout должен быть положительным"
        )));
    }
    if is_cancelled() {
        return Err(YtDlpTopologyError::Cancellation);
    }
    let operation_started_at = Instant::now();

    let mut command = Command::new(executable);
    command.args(TOPOLOGY_ARGUMENTS_BEFORE_POLICY);
    if impersonation == GenericExtractorImpersonation::RequiredForHttp {
        command.args(GENERIC_IMPERSONATE_EXTRACTOR_ARGS);
    }
    command
        .arg(exact_locator)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = match spawn_owned_process_with_launcher(
        &mut command,
        operation_started_at,
        timeout,
        is_cancelled,
        process_launcher,
        invocation,
    ) {
        Ok(process) => process,
        Err(OwnedProcessSpawnError::Cancellation) => {
            return Err(YtDlpTopologyError::Cancellation);
        }
        Err(OwnedProcessSpawnError::Process(error)) => {
            return Err(YtDlpTopologyError::process(error));
        }
    };
    let stdout = match process.take_stdout() {
        Some(stdout) => stdout,
        None => {
            let primary = YtDlpTopologyError::process(anyhow::anyhow!("stdout pipe недоступен"));
            return Err(finish_topology_process_after_error(&mut process, primary));
        }
    };
    let stderr = match process.take_stderr() {
        Some(stderr) => stderr,
        None => {
            let primary = YtDlpTopologyError::process(anyhow::anyhow!("stderr pipe недоступен"));
            return Err(finish_topology_process_after_error(&mut process, primary));
        }
    };
    let budget_signal = Arc::new(AtomicU8::new(BudgetSignal::None as u8));

    let stdout_reader = match spawn_stdout_reader(stdout, budgets, Arc::clone(&budget_signal)) {
        Ok(reader) => reader,
        Err(primary) => return Err(finish_topology_process_after_error(&mut process, primary)),
    };
    let stderr_reader = match spawn_stderr_reader(stderr, budgets, Arc::clone(&budget_signal)) {
        Ok(reader) => reader,
        Err(primary) => {
            if let Err(cleanup) = process.finish() {
                return Err(combine_topology_process_failures(primary, cleanup.into()));
            }
            return match stdout_reader.abort() {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(combine_topology_process_failures(
                    primary,
                    anyhow::Error::new(cleanup),
                )),
            };
        }
    };

    let remaining_timeout = timeout.saturating_sub(operation_started_at.elapsed());
    let wait_result = wait_for_child(
        &mut process,
        remaining_timeout,
        is_cancelled,
        budget_signal.as_ref(),
    );
    let wait_outcome = match wait_result {
        Ok(outcome) => outcome,
        Err(primary) => {
            return match abort_topology_readers(stdout_reader, stderr_reader) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(combine_topology_process_failures(
                    primary,
                    anyhow::Error::new(cleanup),
                )),
            };
        }
    };

    match wait_outcome {
        ProcessWaitOutcome::Exited(status) => {
            if let Some(error) = BudgetSignal::load(budget_signal.as_ref()).into_error() {
                return match abort_topology_readers(stdout_reader, stderr_reader) {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(combine_topology_process_failures(
                        error,
                        anyhow::Error::new(cleanup),
                    )),
                };
            }
            let (stdout_result, stderr_bytes) = drain_topology_readers(
                stdout_reader,
                stderr_reader,
                operation_started_at,
                timeout,
                is_cancelled,
            )?;
            Ok(TopologyProcessOutput {
                status,
                stdout_lines: stdout_result.lines,
                stderr_bytes,
            })
        }
        ProcessWaitOutcome::TimedOut => finish_topology_outcome_after_abort(
            YtDlpTopologyError::Timeout,
            stdout_reader,
            stderr_reader,
        ),
        ProcessWaitOutcome::Cancelled => finish_topology_outcome_after_abort(
            YtDlpTopologyError::Cancellation,
            stdout_reader,
            stderr_reader,
        ),
        ProcessWaitOutcome::BudgetExceeded(signal) => finish_topology_outcome_after_abort(
            signal
                .into_error()
                .unwrap_or(YtDlpTopologyError::StdoutBudgetExceeded),
            stdout_reader,
            stderr_reader,
        ),
    }
}

/// Завершает owner после primary failure, сохраняя cleanup failure в одной причине.
fn finish_topology_process_after_error(
    process: &mut OwnedProcess,
    primary: YtDlpTopologyError,
) -> YtDlpTopologyError {
    match process.finish() {
        Ok(_) => primary,
        Err(cleanup) => combine_topology_process_failures(primary, cleanup.into()),
    }
}

/// Упаковывает primary и дополнительную cleanup/join ошибку без потери причин.
fn combine_topology_process_failures(
    primary: YtDlpTopologyError,
    cleanup: anyhow::Error,
) -> YtDlpTopologyError {
    YtDlpTopologyError::process(OwnedProcessCleanupFailure::new(
        anyhow::Error::new(primary),
        cleanup,
    ))
}

/// Сохраняет typed wait outcome, если оба pipe worker-а bounded остановлены.
fn finish_topology_outcome_after_abort(
    primary: YtDlpTopologyError,
    stdout_reader: OwnedPipeReader<StdoutReadResult>,
    stderr_reader: OwnedPipeReader<usize>,
) -> Result<TopologyProcessOutput, YtDlpTopologyError> {
    match abort_topology_readers(stdout_reader, stderr_reader) {
        Ok(()) => Err(primary),
        Err(cleanup) => Err(combine_topology_process_failures(
            primary,
            anyhow::Error::new(cleanup),
        )),
    }
}

#[derive(Debug)]
struct StdoutReadResult {
    lines: Vec<Vec<u8>>,
}

fn spawn_stdout_reader(
    stdout: std::process::ChildStdout,
    budgets: YtDlpTopologyBudgets,
    budget_signal: Arc<AtomicU8>,
) -> Result<OwnedPipeReader<StdoutReadResult>, YtDlpTopologyError> {
    spawn_owned_pipe_reader("yt-dlp-topology-stdout", stdout, move |reader| {
        read_stdout(reader, budgets, budget_signal.as_ref())
    })
    .map_err(YtDlpTopologyError::process)
}

fn spawn_stderr_reader(
    stderr: std::process::ChildStderr,
    budgets: YtDlpTopologyBudgets,
    budget_signal: Arc<AtomicU8>,
) -> Result<OwnedPipeReader<usize>, YtDlpTopologyError> {
    spawn_owned_pipe_reader("yt-dlp-topology-stderr", stderr, move |reader| {
        count_stderr(reader, budgets.stderr_bytes, budget_signal.as_ref())
    })
    .map_err(YtDlpTopologyError::process)
}

fn read_stdout<R>(
    stdout: &mut R,
    budgets: YtDlpTopologyBudgets,
    budget_signal: &AtomicU8,
) -> io::Result<StdoutReadResult>
where
    R: Read + ?Sized,
{
    let mut read_buffer = [0_u8; PIPE_READ_CHUNK_BYTES];
    let mut current_line = Vec::new();
    let mut completed_lines = Vec::new();
    let mut total_bytes = 0usize;
    let mut current_line_overflowed = false;

    loop {
        let bytes_read = stdout.read(&mut read_buffer)?;
        if bytes_read == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(bytes_read);
        if total_bytes > budgets.stdout_bytes {
            BudgetSignal::StdoutBytes.publish(budget_signal);
        }

        for byte in &read_buffer[..bytes_read] {
            if *byte == b'\n' {
                finish_stdout_line(
                    &mut current_line,
                    &mut completed_lines,
                    &mut current_line_overflowed,
                    budgets,
                    budget_signal,
                );
                continue;
            }
            if *byte == b'\r' {
                continue;
            }
            if !current_line_overflowed {
                if current_line.len() < budgets.json_line_bytes {
                    current_line.push(*byte);
                } else {
                    current_line_overflowed = true;
                    BudgetSignal::JsonLineBytes.publish(budget_signal);
                }
            }
        }
    }

    if !current_line.is_empty() || current_line_overflowed {
        finish_stdout_line(
            &mut current_line,
            &mut completed_lines,
            &mut current_line_overflowed,
            budgets,
            budget_signal,
        );
    }

    Ok(StdoutReadResult {
        lines: completed_lines,
    })
}

fn finish_stdout_line(
    current_line: &mut Vec<u8>,
    completed_lines: &mut Vec<Vec<u8>>,
    current_line_overflowed: &mut bool,
    budgets: YtDlpTopologyBudgets,
    budget_signal: &AtomicU8,
) {
    if !*current_line_overflowed && !current_line.is_empty() {
        let maximum_process_lines = budgets.entry_count.saturating_add(1);
        if completed_lines.len() < maximum_process_lines {
            completed_lines.push(std::mem::take(current_line));
        } else {
            BudgetSignal::EntryCount.publish(budget_signal);
            current_line.clear();
        }
    } else {
        current_line.clear();
    }
    *current_line_overflowed = false;
}

fn count_stderr<R>(
    stderr: &mut R,
    stderr_budget: usize,
    budget_signal: &AtomicU8,
) -> io::Result<usize>
where
    R: Read + ?Sized,
{
    let mut read_buffer = [0_u8; PIPE_READ_CHUNK_BYTES];
    let mut observed_bytes = 0usize;
    loop {
        let bytes_read = stderr.read(&mut read_buffer)?;
        if bytes_read == 0 {
            break;
        }
        observed_bytes = observed_bytes.saturating_add(bytes_read);
        if observed_bytes > stderr_budget {
            BudgetSignal::StderrBytes.publish(budget_signal);
        }
    }

    Ok(observed_bytes.min(stderr_budget))
}

fn map_topology_pipe_drain_error(error: OwnedPipeDrainError) -> YtDlpTopologyError {
    match error {
        OwnedPipeDrainError::Cancellation => YtDlpTopologyError::Cancellation,
        OwnedPipeDrainError::OperationTimedOut => YtDlpTopologyError::Timeout,
        OwnedPipeDrainError::CancellationCleanup { source } => combine_topology_process_failures(
            YtDlpTopologyError::Cancellation,
            anyhow::Error::new(source),
        ),
        OwnedPipeDrainError::OperationTimeoutCleanup { source } => {
            combine_topology_process_failures(
                YtDlpTopologyError::Timeout,
                anyhow::Error::new(source),
            )
        }
        other => YtDlpTopologyError::process(other),
    }
}

/// Bounded drain обоих topology readers после нормального root exit.
fn drain_topology_readers(
    stdout_reader: OwnedPipeReader<StdoutReadResult>,
    stderr_reader: OwnedPipeReader<usize>,
    operation_started_at: Instant,
    operation_timeout: Duration,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(StdoutReadResult, usize), YtDlpTopologyError> {
    let drain_started_at = Instant::now();
    let stdout_result = stdout_reader
        .drain(
            operation_started_at,
            operation_timeout,
            drain_started_at,
            is_cancelled,
        )
        .map_err(map_topology_pipe_drain_error);
    let stderr_result = stderr_reader
        .drain(
            operation_started_at,
            operation_timeout,
            drain_started_at,
            is_cancelled,
        )
        .map_err(map_topology_pipe_drain_error);

    match (stdout_result, stderr_result) {
        (Ok(stdout), Ok(stderr)) => Ok((stdout, stderr)),
        (Err(primary), Ok(_)) | (Ok(_), Err(primary)) => Err(primary),
        (Err(primary), Err(cleanup)) => Err(combine_topology_process_failures(
            primary,
            anyhow::Error::new(cleanup),
        )),
    }
}

/// Bounded останавливает оба topology reader worker-а после non-success outcome.
fn abort_topology_readers(
    stdout_reader: OwnedPipeReader<StdoutReadResult>,
    stderr_reader: OwnedPipeReader<usize>,
) -> Result<(), YtDlpTopologyError> {
    let stdout_result = stdout_reader.abort().map_err(YtDlpTopologyError::process);
    let stderr_result = stderr_reader.abort().map_err(YtDlpTopologyError::process);

    match (stdout_result, stderr_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) | (Ok(()), Err(primary)) => Err(primary),
        (Err(primary), Err(cleanup)) => Err(combine_topology_process_failures(
            primary,
            anyhow::Error::new(cleanup),
        )),
    }
}

fn wait_for_child(
    process: &mut OwnedProcess,
    timeout: Duration,
    is_cancelled: &dyn Fn() -> bool,
    budget_signal: &AtomicU8,
) -> Result<ProcessWaitOutcome, YtDlpTopologyError> {
    let started_at = Instant::now();
    loop {
        match process.poll_root_exit() {
            Ok(OwnedProcessRootState::Exited) => {
                let status = process.finish().map_err(YtDlpTopologyError::process)?;
                return Ok(ProcessWaitOutcome::Exited(status));
            }
            Ok(OwnedProcessRootState::Running) => {}
            Err(error) => {
                let primary = YtDlpTopologyError::process(error);
                return Err(finish_topology_process_after_error(process, primary));
            }
        }

        let active_budget_signal = BudgetSignal::load(budget_signal);
        if active_budget_signal != BudgetSignal::None {
            let primary = active_budget_signal
                .into_error()
                .unwrap_or(YtDlpTopologyError::StdoutBudgetExceeded);
            process
                .finish()
                .map_err(|cleanup| combine_topology_process_failures(primary, cleanup.into()))?;
            return Ok(ProcessWaitOutcome::BudgetExceeded(active_budget_signal));
        }
        if is_cancelled() {
            process.finish().map_err(|cleanup| {
                combine_topology_process_failures(YtDlpTopologyError::Cancellation, cleanup.into())
            })?;
            return Ok(ProcessWaitOutcome::Cancelled);
        }
        if started_at.elapsed() >= timeout {
            process.finish().map_err(|cleanup| {
                combine_topology_process_failures(YtDlpTopologyError::Timeout, cleanup.into())
            })?;
            return Ok(ProcessWaitOutcome::TimedOut);
        }

        let remaining_timeout = timeout.saturating_sub(started_at.elapsed());
        thread::sleep(remaining_timeout.min(PROCESS_POLL_INTERVAL));
    }
}

enum ProcessWaitOutcome {
    Exited(ExitStatus),
    TimedOut,
    Cancelled,
    BudgetExceeded(BudgetSignal),
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BudgetSignal {
    None = 0,
    StdoutBytes = 1,
    StderrBytes = 2,
    JsonLineBytes = 3,
    EntryCount = 4,
}

impl BudgetSignal {
    fn publish(self, shared_signal: &AtomicU8) {
        let _ = shared_signal.compare_exchange(
            Self::None as u8,
            self as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn load(shared_signal: &AtomicU8) -> Self {
        match shared_signal.load(Ordering::Acquire) {
            1 => Self::StdoutBytes,
            2 => Self::StderrBytes,
            3 => Self::JsonLineBytes,
            4 => Self::EntryCount,
            _ => Self::None,
        }
    }

    const fn into_error(self) -> Option<YtDlpTopologyError> {
        match self {
            Self::None => None,
            Self::StdoutBytes => Some(YtDlpTopologyError::StdoutBudgetExceeded),
            Self::StderrBytes => Some(YtDlpTopologyError::StderrBudgetExceeded),
            Self::JsonLineBytes => Some(YtDlpTopologyError::JsonLineBudgetExceeded),
            Self::EntryCount => Some(YtDlpTopologyError::EntryBudgetExceeded),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static SCRIPT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
    // `exec` оставляет один процесс-владелец pipe-ов, а `sleep` не отбирает CPU у coverage-runner.
    const SLOW_CHILD_SCRIPT: &str = "#!/bin/sh\nexec sleep 60\n";

    #[test]
    fn exact_argv_emits_lazy_lines_and_final_root() {
        let script = create_script(
            r#"#!/bin/sh
test "$1" = "--quiet" || exit 91
test "$2" = "--no-warnings" || exit 92
test "$3" = "--simulate" || exit 93
test "$4" = "--dump-json" || exit 94
test "$5" = "--dump-single-json" || exit 95
test "$6" = "--flat-playlist" || exit 96
test "$7" = "--lazy-playlist" || exit 97
test "$8" = "--extractor-args" || exit 98
test "$9" = "generic:impersonate" || exit 99
test "${10}" = "https://input.invalid/root?token=secret" || exit 100
printf '%s\n' \
  '{"_type":"url","url":"https://delegate.invalid/1"}' \
  '{"_type":"playlist","id":"root","title":"Root","entries":[{"_type":"url","url":"https://delegate.invalid/1"}]}'
"#,
        );
        let output = run_topology_process(
            script.to_str().expect("UTF-8 test path"),
            "https://input.invalid/root?token=secret",
            GenericExtractorImpersonation::RequiredForHttp,
            Duration::from_secs(2),
            YtDlpTopologyBudgets::default(),
            &|| false,
        )
        .expect("exact argv fake process должен завершиться");

        assert!(output.status.success());
        assert_eq!(output.stdout_lines.len(), 2);
        assert_eq!(output.stderr_bytes, 0);
        remove_script(script);
    }

    #[test]
    fn ftp_argv_omits_http_impersonation_policy() {
        let script = create_script(
            r#"#!/bin/sh
test "$#" -eq 8 || exit 90
test "$1" = "--quiet" || exit 91
test "$2" = "--no-warnings" || exit 92
test "$3" = "--simulate" || exit 93
test "$4" = "--dump-json" || exit 94
test "$5" = "--dump-single-json" || exit 95
test "$6" = "--flat-playlist" || exit 96
test "$7" = "--lazy-playlist" || exit 97
test "$8" = "ftp://media.invalid/audio.ogg" || exit 98
printf '%s\n' '{"_type":"video","id":"ftp-audio"}'
"#,
        );
        let output = run_topology_process(
            script.to_str().expect("UTF-8 test path"),
            "ftp://media.invalid/audio.ogg",
            GenericExtractorImpersonation::NotApplicableForNativeTransport,
            Duration::from_secs(2),
            YtDlpTopologyBudgets::default(),
            &|| false,
        )
        .expect("FTP topology argv fake process должен завершиться");

        assert!(output.status.success());
        assert_eq!(output.stdout_lines.len(), 1);
        remove_script(script);
    }

    #[test]
    fn timeout_and_cancellation_kill_and_wait_child() {
        let timeout_script = create_script(SLOW_CHILD_SCRIPT);
        let timeout_error = run_topology_process(
            timeout_script.to_str().expect("UTF-8 test path"),
            "https://input.invalid/root",
            GenericExtractorImpersonation::RequiredForHttp,
            Duration::from_millis(30),
            YtDlpTopologyBudgets::default(),
            &|| false,
        )
        .expect_err("slow child должен получить timeout");
        assert!(
            matches!(timeout_error, YtDlpTopologyError::Timeout),
            "ожидался Timeout, получено: {timeout_error:?}"
        );
        remove_script(timeout_script);

        let cancel_script = create_script(SLOW_CHILD_SCRIPT);
        let cancellation_checks = AtomicU64::new(0);
        let cancellation_error = run_topology_process(
            cancel_script.to_str().expect("UTF-8 test path"),
            "https://input.invalid/root",
            GenericExtractorImpersonation::RequiredForHttp,
            Duration::from_secs(2),
            YtDlpTopologyBudgets::default(),
            &|| cancellation_checks.fetch_add(1, Ordering::Relaxed) > 1,
        )
        .expect_err("cancelled child должен быть остановлен");
        assert!(
            matches!(cancellation_error, YtDlpTopologyError::Cancellation),
            "ожидался Cancellation, получено: {cancellation_error:?}"
        );
        remove_script(cancel_script);
    }

    #[test]
    fn nonzero_and_pipe_budgets_never_expose_payload() {
        let nonzero_script =
            create_script("#!/bin/sh\nprintf 'password=very-secret' >&2\nexit 7\n");
        let output = run_topology_process(
            nonzero_script.to_str().expect("UTF-8 test path"),
            "https://input.invalid/root?token=secret",
            GenericExtractorImpersonation::RequiredForHttp,
            Duration::from_secs(2),
            YtDlpTopologyBudgets::default(),
            &|| false,
        )
        .expect("process layer возвращает status вызывающему");
        assert!(!output.status.success());
        assert_eq!(output.stderr_bytes, 20);
        assert!(!format!("{output:?}").contains("very-secret"));
        remove_script(nonzero_script);

        let huge_stdout_script =
            create_script("#!/bin/sh\nprintf '12345678901234567890\\n'\nsleep 1\n");
        let budget_error = run_topology_process(
            huge_stdout_script.to_str().expect("UTF-8 test path"),
            "https://input.invalid/root",
            GenericExtractorImpersonation::RequiredForHttp,
            Duration::from_secs(2),
            YtDlpTopologyBudgets {
                stdout_bytes: 8,
                json_line_bytes: 8,
                ..YtDlpTopologyBudgets::default()
            },
            &|| false,
        )
        .expect_err("huge stdout должен быть bounded");
        assert!(matches!(
            budget_error,
            YtDlpTopologyError::StdoutBudgetExceeded | YtDlpTopologyError::JsonLineBudgetExceeded
        ));
        remove_script(huge_stdout_script);
    }

    fn create_script(body: &str) -> PathBuf {
        let sequence = SCRIPT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rustiplayer-ytdlp-topology-{}-{sequence}.sh",
            std::process::id()
        ));
        fs::write(&path, body).expect("test script должен записаться");
        let mut permissions = fs::metadata(&path)
            .expect("test script metadata должна читаться")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).expect("test script должен стать executable");
        path
    }

    fn remove_script(path: PathBuf) {
        fs::remove_file(path).expect("test script должен удалиться");
    }
}
