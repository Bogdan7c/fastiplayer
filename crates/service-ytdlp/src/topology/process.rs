//! Bounded child-process owner для line-delimited topology extraction.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::limits::{YtDlpTopologyBudgets, YtDlpTopologyError};

/// Poll interval сохраняет responsive cancellation без busy-spin.
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Fixed read chunk исключает allocation по длине недоверенной line.
const PIPE_READ_CHUNK_BYTES: usize = 8 * 1024;

/// `--dump-json` печатает lazy entries, а `--dump-single-json` — final root.
///
/// Порядок safety-critical и закреплён exact-argv focused test-ом.
const TOPOLOGY_ARGUMENTS_BEFORE_URL: [&str; 7] = [
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
pub(crate) fn run_topology_process(
    executable: &str,
    exact_locator: &str,
    timeout: Duration,
    budgets: YtDlpTopologyBudgets,
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

    let mut command = Command::new(executable);
    command
        .args(TOPOLOGY_ARGUMENTS_BEFORE_URL)
        .arg(exact_locator)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(YtDlpTopologyError::process)?;
    let stdout = match take_child_stdout(&mut child) {
        Ok(stdout) => stdout,
        Err(error) => {
            let _ = terminate_and_wait(&mut child);
            return Err(error);
        }
    };
    let stderr = match take_child_stderr(&mut child) {
        Ok(stderr) => stderr,
        Err(error) => {
            let _ = terminate_and_wait(&mut child);
            return Err(error);
        }
    };
    let budget_signal = Arc::new(AtomicU8::new(BudgetSignal::None as u8));

    let stdout_reader = match spawn_stdout_reader(stdout, budgets, Arc::clone(&budget_signal)) {
        Ok(reader) => reader,
        Err(error) => {
            terminate_and_wait(&mut child)?;
            return Err(error);
        }
    };
    let stderr_reader = match spawn_stderr_reader(stderr, budgets, Arc::clone(&budget_signal)) {
        Ok(reader) => reader,
        Err(error) => {
            terminate_and_wait(&mut child)?;
            let _ = join_stdout_reader(stdout_reader);
            return Err(error);
        }
    };

    let wait_outcome = wait_for_child(&mut child, timeout, is_cancelled, budget_signal.as_ref())?;
    let stdout_result = join_stdout_reader(stdout_reader)?;
    let stderr_bytes = join_stderr_reader(stderr_reader)?;

    match wait_outcome {
        ProcessWaitOutcome::Exited(status) => {
            if let Some(error) = BudgetSignal::load(budget_signal.as_ref()).into_error() {
                return Err(error);
            }
            Ok(TopologyProcessOutput {
                status,
                stdout_lines: stdout_result.lines,
                stderr_bytes,
            })
        }
        ProcessWaitOutcome::TimedOut => Err(YtDlpTopologyError::Timeout),
        ProcessWaitOutcome::Cancelled => Err(YtDlpTopologyError::Cancellation),
        ProcessWaitOutcome::BudgetExceeded(signal) => Err(signal
            .into_error()
            .unwrap_or(YtDlpTopologyError::StdoutBudgetExceeded)),
    }
}

fn take_child_stdout(child: &mut Child) -> Result<std::process::ChildStdout, YtDlpTopologyError> {
    child
        .stdout
        .take()
        .ok_or_else(|| YtDlpTopologyError::process(anyhow::anyhow!("stdout pipe недоступен")))
}

fn take_child_stderr(child: &mut Child) -> Result<std::process::ChildStderr, YtDlpTopologyError> {
    child
        .stderr
        .take()
        .ok_or_else(|| YtDlpTopologyError::process(anyhow::anyhow!("stderr pipe недоступен")))
}

#[derive(Debug)]
struct StdoutReadResult {
    lines: Vec<Vec<u8>>,
}

fn spawn_stdout_reader(
    stdout: std::process::ChildStdout,
    budgets: YtDlpTopologyBudgets,
    budget_signal: Arc<AtomicU8>,
) -> Result<thread::JoinHandle<io::Result<StdoutReadResult>>, YtDlpTopologyError> {
    thread::Builder::new()
        .name("yt-dlp-topology-stdout".to_owned())
        .spawn(move || read_stdout(stdout, budgets, budget_signal.as_ref()))
        .map_err(YtDlpTopologyError::process)
}

fn spawn_stderr_reader(
    stderr: std::process::ChildStderr,
    budgets: YtDlpTopologyBudgets,
    budget_signal: Arc<AtomicU8>,
) -> Result<thread::JoinHandle<io::Result<usize>>, YtDlpTopologyError> {
    thread::Builder::new()
        .name("yt-dlp-topology-stderr".to_owned())
        .spawn(move || count_stderr(stderr, budgets.stderr_bytes, budget_signal.as_ref()))
        .map_err(YtDlpTopologyError::process)
}

fn read_stdout(
    mut stdout: std::process::ChildStdout,
    budgets: YtDlpTopologyBudgets,
    budget_signal: &AtomicU8,
) -> io::Result<StdoutReadResult> {
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

fn count_stderr(
    mut stderr: std::process::ChildStderr,
    stderr_budget: usize,
    budget_signal: &AtomicU8,
) -> io::Result<usize> {
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

fn join_stdout_reader(
    reader: thread::JoinHandle<io::Result<StdoutReadResult>>,
) -> Result<StdoutReadResult, YtDlpTopologyError> {
    reader
        .join()
        .map_err(|_| {
            YtDlpTopologyError::process(anyhow::anyhow!("stdout reader thread завершился panic"))
        })?
        .map_err(YtDlpTopologyError::process)
}

fn join_stderr_reader(
    reader: thread::JoinHandle<io::Result<usize>>,
) -> Result<usize, YtDlpTopologyError> {
    reader
        .join()
        .map_err(|_| {
            YtDlpTopologyError::process(anyhow::anyhow!("stderr reader thread завершился panic"))
        })?
        .map_err(YtDlpTopologyError::process)
}

fn wait_for_child(
    child: &mut Child,
    timeout: Duration,
    is_cancelled: &dyn Fn() -> bool,
    budget_signal: &AtomicU8,
) -> Result<ProcessWaitOutcome, YtDlpTopologyError> {
    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(ProcessWaitOutcome::Exited(status)),
            Ok(None) => {}
            Err(error) => {
                let _ = terminate_and_wait(child);
                return Err(YtDlpTopologyError::process(error));
            }
        }

        let active_budget_signal = BudgetSignal::load(budget_signal);
        if active_budget_signal != BudgetSignal::None {
            terminate_and_wait(child)?;
            return Ok(ProcessWaitOutcome::BudgetExceeded(active_budget_signal));
        }
        if is_cancelled() {
            terminate_and_wait(child)?;
            return Ok(ProcessWaitOutcome::Cancelled);
        }
        if started_at.elapsed() >= timeout {
            terminate_and_wait(child)?;
            return Ok(ProcessWaitOutcome::TimedOut);
        }

        let remaining_timeout = timeout.saturating_sub(started_at.elapsed());
        thread::sleep(remaining_timeout.min(PROCESS_POLL_INTERVAL));
    }
}

fn terminate_and_wait(child: &mut Child) -> Result<(), YtDlpTopologyError> {
    match child.kill() {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
        Err(error) => return Err(YtDlpTopologyError::process(error)),
    }
    child.wait().map_err(YtDlpTopologyError::process)?;
    Ok(())
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
test "$8" = "https://input.invalid/root?token=secret" || exit 98
printf '%s\n' \
  '{"_type":"url","url":"https://delegate.invalid/1"}' \
  '{"_type":"playlist","id":"root","title":"Root","entries":[{"_type":"url","url":"https://delegate.invalid/1"}]}'
"#,
        );
        let output = run_topology_process(
            script.to_str().expect("UTF-8 test path"),
            "https://input.invalid/root?token=secret",
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
    fn timeout_and_cancellation_kill_and_wait_child() {
        let timeout_script = create_script("#!/bin/sh\nwhile :; do :; done\n");
        let timeout_error = run_topology_process(
            timeout_script.to_str().expect("UTF-8 test path"),
            "https://input.invalid/root",
            Duration::from_millis(30),
            YtDlpTopologyBudgets::default(),
            &|| false,
        )
        .expect_err("slow child должен получить timeout");
        assert!(matches!(timeout_error, YtDlpTopologyError::Timeout));
        remove_script(timeout_script);

        let cancel_script = create_script("#!/bin/sh\nwhile :; do :; done\n");
        let cancellation_checks = AtomicU64::new(0);
        let cancellation_error = run_topology_process(
            cancel_script.to_str().expect("UTF-8 test path"),
            "https://input.invalid/root",
            Duration::from_secs(2),
            YtDlpTopologyBudgets::default(),
            &|| cancellation_checks.fetch_add(1, Ordering::Relaxed) > 1,
        )
        .expect_err("cancelled child должен быть остановлен");
        assert!(matches!(
            cancellation_error,
            YtDlpTopologyError::Cancellation
        ));
        remove_script(cancel_script);
    }

    #[test]
    fn nonzero_and_pipe_budgets_never_expose_payload() {
        let nonzero_script =
            create_script("#!/bin/sh\nprintf 'password=very-secret' >&2\nexit 7\n");
        let output = run_topology_process(
            nonzero_script.to_str().expect("UTF-8 test path"),
            "https://input.invalid/root?token=secret",
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
