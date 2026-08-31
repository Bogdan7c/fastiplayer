use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rustiplayer_config::YtDlpConfig;
use serde::de::DeserializeOwned;
use serde_json::Value;
use web_media_core::ExtractorInvocationReason;

use crate::dto::YtDlpMetadata;
use crate::embed_recovery::{GenericExtractorImpersonation, candidate_arguments};
use crate::error::YtDlpServiceError;
#[cfg(test)]
use crate::invocation::YtDlpExtractorAdapter;
use crate::invocation::{
    ExtractorProcessInvocation, ExtractorProcessLauncher, ExtractorProcessPhase,
};
use crate::locator::YtDlpMediaLocator;
use crate::process_output::{
    ProcessOutputBudgetSignal, YtDlpProcessOutputBudgets, spawn_stderr_reader, spawn_stdout_reader,
    validate_json_node_budget,
};
use crate::process_tree::{
    OwnedPipeDrainError, OwnedPipeReader, OwnedProcess, OwnedProcessCleanupFailure,
    OwnedProcessRootState, OwnedProcessSpawnError, spawn_owned_process_with_launcher,
};

mod recovery;

pub(crate) use recovery::recover_playable_document_after_platform_hijack;

/// Имя production binary, через который service получает direct stream metadata.
const YT_DLP_EXECUTABLE: &str = "yt-dlp";

/// Интервал polling-а child process: маленький, но без busy-loop.
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Верхняя граница счётчика stderr в safe diagnostic context.
const MAX_REPORTED_STDERR_BYTES: usize = 1_048_576;

/// Runtime policy запуска `yt-dlp`, отделённая от parsing/selection логики.
#[derive(Clone)]
pub(crate) struct YtDlpProcessConfig {
    /// Имя или путь к executable.
    executable: String,

    /// Верхняя граница ожидания metadata command.
    timeout: Duration,

    /// Независимые byte/structure budgets single-item process path-а.
    output_budgets: YtDlpProcessOutputBudgets,

    /// Injected launcher текущей adapter instance, общий для primary и recovery.
    process_launcher: Arc<dyn ExtractorProcessLauncher>,

    /// Неизменяемая пользовательская причина всей extraction operation.
    invocation_reason: ExtractorInvocationReason,
}

impl YtDlpProcessConfig {
    /// Строит process policy из пользовательского YtDlp config.
    #[cfg(test)]
    pub(crate) fn from_yt_dlp_config(
        yt_dlp_config: &YtDlpConfig,
    ) -> Result<Self, YtDlpServiceError> {
        Self::from_yt_dlp_config_with_invocation(
            yt_dlp_config,
            YtDlpExtractorAdapter::default().process_launcher(),
            ExtractorInvocationReason::PageMediaResolution,
        )
    }

    /// Строит process policy с explicit injected launcher и product reason.
    pub(crate) fn from_yt_dlp_config_with_invocation(
        yt_dlp_config: &YtDlpConfig,
        process_launcher: Arc<dyn ExtractorProcessLauncher>,
        invocation_reason: ExtractorInvocationReason,
    ) -> Result<Self, YtDlpServiceError> {
        let output_budgets = YtDlpProcessOutputBudgets::from_config(yt_dlp_config)?;

        Ok(Self {
            executable: YT_DLP_EXECUTABLE.to_string(),
            timeout: Duration::from_millis(yt_dlp_config.resolve_timeout_ms),
            output_budgets,
            process_launcher,
            invocation_reason,
        })
    }

    /// Строит topology policy с тем же launcher/reason contract-ом.
    pub(crate) fn from_yt_dlp_config_for_topology_with_invocation(
        yt_dlp_config: &YtDlpConfig,
        process_launcher: Arc<dyn ExtractorProcessLauncher>,
        invocation_reason: ExtractorInvocationReason,
    ) -> Result<Self, crate::topology::YtDlpTopologyError> {
        let output_budgets = YtDlpProcessOutputBudgets::from_config(yt_dlp_config)
            .map_err(crate::topology::YtDlpTopologyError::process)?;

        Ok(Self {
            executable: YT_DLP_EXECUTABLE.to_string(),
            timeout: Duration::from_millis(yt_dlp_config.resolve_timeout_ms),
            output_budgets,
            process_launcher,
            invocation_reason,
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

    /// Возвращает validated single-item output budget profile.
    const fn output_budgets(&self) -> YtDlpProcessOutputBudgets {
        self.output_budgets
    }

    /// Возвращает injected launcher, не раскрывая его concrete type.
    pub(crate) fn process_launcher(&self) -> &dyn ExtractorProcessLauncher {
        self.process_launcher.as_ref()
    }

    /// Создаёт secret-free event для конкретной subprocess phase.
    pub(crate) const fn invocation(
        &self,
        phase: ExtractorProcessPhase,
    ) -> ExtractorProcessInvocation {
        ExtractorProcessInvocation::new(self.invocation_reason, phase)
    }

    /// Связывает instance launcher с phase-specific secret-free event.
    fn launch_context(&self, phase: ExtractorProcessPhase) -> ProcessLaunchContext<'_> {
        ProcessLaunchContext {
            process_launcher: self.process_launcher(),
            invocation: self.invocation(phase),
        }
    }
}

impl std::fmt::Debug for YtDlpProcessConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("YtDlpProcessConfig")
            .field("executable", &self.executable)
            .field("timeout", &self.timeout)
            .field("output_budgets", &self.output_budgets)
            .field("process_launcher", &"<injected>")
            .field("invocation_reason", &self.invocation_reason)
            .finish()
    }
}

/// Один named argument объединяет launcher и typed invocation одного spawn path-а.
#[derive(Clone, Copy)]
struct ProcessLaunchContext<'launcher> {
    /// Instance-owned injected launcher.
    process_launcher: &'launcher dyn ExtractorProcessLauncher,
    /// Secret-free reason/phase event.
    invocation: ExtractorProcessInvocation,
}

/// Собранный stdout/stderr внешнего процесса.
struct ProcessOutput {
    /// Exit status, полученный от OS.
    status: ExitStatus,

    /// Полный stdout процесса.
    stdout: Vec<u8>,

    /// Число stderr bytes без сохранения diagnostic payload.
    stderr_bytes: usize,
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
                &format_args!("<redacted:{} bytes>", self.stderr_bytes),
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
    let primary_document = run_dump_single_json(
        video_url,
        impersonation,
        process_config,
        ExtractorProcessPhase::CandidatePrimary,
        is_cancelled,
    )?;
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

fn run_dump_single_json(
    video_url: &str,
    impersonation: GenericExtractorImpersonation,
    process_config: &YtDlpProcessConfig,
    process_phase: ExtractorProcessPhase,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Value, YtDlpServiceError> {
    let command_arguments = candidate_arguments(video_url, impersonation);
    let command_output = run_process_with_extractor_invocation(
        process_config.executable.as_str(),
        &command_arguments,
        None,
        process_config.timeout,
        process_config.output_budgets(),
        process_config.launch_context(process_phase),
        is_cancelled,
    )?;

    ensure_yt_dlp_candidate_success(command_output.status, command_output.stderr_bytes)?;
    validate_json_node_budget(&command_output.stdout, process_config.output_budgets())?;
    serde_json::from_slice(&command_output.stdout).map_err(YtDlpServiceError::invalid_response)
}

/// Запускает внешний процесс с typed invocation, timeout-ом и cancellation.
fn run_process_with_extractor_invocation(
    executable: &str,
    arguments: &[&str],
    current_directory: Option<&Path>,
    timeout: Duration,
    output_budgets: YtDlpProcessOutputBudgets,
    launch_context: ProcessLaunchContext<'_>,
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
    let mut process = match spawn_owned_process_with_launcher(
        &mut command,
        operation_started_at,
        timeout,
        is_cancelled,
        launch_context.process_launcher,
        launch_context.invocation,
    ) {
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
    let output_budget_signal = ProcessOutputBudgetSignal::new();
    let stdout_reader = match spawn_stdout_reader(
        stdout,
        output_budgets.stdout_bytes(),
        output_budget_signal.clone(),
    ) {
        Ok(reader) => reader,
        Err(primary) => return Err(finish_process_after_error(&mut process, primary)),
    };
    let stderr_reader = match spawn_stderr_reader(
        stderr,
        output_budgets.stderr_bytes(),
        output_budget_signal.clone(),
    ) {
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
    let wait_result = wait_for_process_with_timeout(
        &mut process,
        remaining_timeout,
        output_budgets,
        &output_budget_signal,
        is_cancelled,
    );
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
                output_budgets,
                &output_budget_signal,
                is_cancelled,
            )?;
            let (stdout, stderr_bytes) = pipe_output;
            Ok(ProcessOutput {
                status,
                stdout,
                stderr_bytes,
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

/// Test-only wrapper сохраняет focused lifecycle tests на production launcher path-е.
#[cfg(test)]
fn run_process_with_timeout_and_cancellation(
    executable: &str,
    arguments: &[&str],
    current_directory: Option<&Path>,
    timeout: Duration,
    output_budgets: YtDlpProcessOutputBudgets,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ProcessOutput, YtDlpServiceError> {
    let adapter = YtDlpExtractorAdapter::default();
    let launcher = adapter.process_launcher();
    run_process_with_extractor_invocation(
        executable,
        arguments,
        current_directory,
        timeout,
        output_budgets,
        ProcessLaunchContext {
            process_launcher: launcher.as_ref(),
            invocation: ExtractorProcessInvocation::new(
                ExtractorInvocationReason::PageMediaResolution,
                ExtractorProcessPhase::CandidatePrimary,
            ),
        },
        is_cancelled,
    )
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
    stderr_reader: OwnedPipeReader<usize>,
    operation_started_at: Instant,
    operation_timeout: Duration,
    output_budgets: YtDlpProcessOutputBudgets,
    output_budget_signal: &ProcessOutputBudgetSignal,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(Vec<u8>, usize), YtDlpServiceError> {
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
        (Ok(_), Ok(_)) if output_budget_signal.load().is_some() => {
            let stream = output_budget_signal
                .load()
                .expect("guarded output budget signal");
            Err(stream.into_error(output_budgets))
        }
        (Ok(stdout), Ok(stderr_bytes)) => Ok((stdout, stderr_bytes)),
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
    stderr_reader: OwnedPipeReader<usize>,
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
    output_budgets: YtDlpProcessOutputBudgets,
    output_budget_signal: &ProcessOutputBudgetSignal,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ProcessWaitOutcome, YtDlpServiceError> {
    let start = Instant::now();

    loop {
        if let Some(stream) = output_budget_signal.load() {
            let primary = stream.into_error(output_budgets);
            if let Err(cleanup) = process.finish() {
                return Err(combine_process_failures(primary, cleanup.into()));
            }
            return Err(primary);
        }

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
    stderr_bytes: usize,
) -> Result<(), YtDlpServiceError> {
    if status.success() {
        return Ok(());
    }

    Err(YtDlpServiceError::ExtractorRejection {
        stderr_bytes: stderr_bytes.min(MAX_REPORTED_STDERR_BYTES),
    })
}

#[cfg(test)]
mod tests;
