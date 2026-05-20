use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rustiplayer_config::YoutubeConfig;

use crate::dto::YtDlpMetadata;

/// Имя env-переменной для полного переопределения selector-а `yt-dlp`.
const FORMAT_SELECTOR_ENV: &str = "VIDEO_PLAYER_YOUTUBE_FORMAT_SELECTOR";

/// Имя production binary, через который service получает direct stream metadata.
const YT_DLP_EXECUTABLE: &str = "yt-dlp";

/// Интервал polling-а child process: маленький, но без busy-loop.
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Selector `yt-dlp` по умолчанию для поддерживаемых тестовых потоков.
///
/// Приоритет намеренно идёт от самого тяжёлого SDR-теста к более лёгким:
/// 4K60 -> 4K30 -> 1080p60 -> 1080p.
///
/// Важно: `vcodec=vp9` выбран строго, без `vp9.2`.
/// `vp9.2` обычно означает HDR/10-bit, а текущий production renderer Phase 9
/// рассчитан на NV12/8-bit для обычного playback path.
const DEFAULT_FORMAT_SELECTOR: &str = concat!(
    "bestvideo[ext=webm][vcodec=vp9][height=2160][fps>=60]+",
    "bestaudio[ext=webm][acodec=opus]/",
    "bestvideo[ext=webm][vcodec=vp9][height=2160]+",
    "bestaudio[ext=webm][acodec=opus]/",
    "bestvideo[ext=webm][vcodec=vp9][height=1080][fps>=60]+",
    "bestaudio[ext=webm][acodec=opus]/",
    "bestvideo[ext=webm][vcodec=vp9][height=1080]+",
    "bestaudio[ext=webm][acodec=opus]"
);

/// Runtime policy запуска `yt-dlp`, отделённая от parsing/selection логики.
#[derive(Debug, Clone)]
pub(crate) struct YtDlpProcessConfig {
    /// Имя или путь к executable.
    executable: String,

    /// Верхняя граница ожидания metadata command.
    timeout: Duration,
}

impl YtDlpProcessConfig {
    /// Строит process policy из пользовательского YouTube config.
    pub(crate) fn from_youtube_config(youtube_config: &YoutubeConfig) -> Result<Self> {
        if youtube_config.resolve_timeout_ms == 0 {
            bail!("youtube.resolve_timeout_ms должен быть положительным");
        }

        Ok(Self {
            executable: YT_DLP_EXECUTABLE.to_string(),
            timeout: Duration::from_millis(youtube_config.resolve_timeout_ms),
        })
    }
}

/// Собранный stdout/stderr внешнего процесса.
#[derive(Debug)]
struct ProcessOutput {
    /// Exit status, полученный от OS.
    status: ExitStatus,

    /// Полный stdout процесса.
    stdout: Vec<u8>,

    /// Полный stderr процесса.
    stderr: Vec<u8>,
}

/// Результат ожидания child process.
enum ProcessWaitOutcome {
    /// Процесс завершился сам.
    Exited(ExitStatus),

    /// Процесс был остановлен из-за timeout-а.
    TimedOut,
}

/// Получает metadata и выбранные direct URLs через `yt-dlp`.
pub(crate) fn resolve_youtube_metadata(
    video_url: &str,
    process_config: &YtDlpProcessConfig,
) -> Result<YtDlpMetadata> {
    // Выбираем selector из env или используем тестовую policy по умолчанию.
    let format_selector = youtube_format_selector();
    let command_arguments = [
        "--quiet",
        "--no-warnings",
        "--simulate",
        "--dump-single-json",
        "--no-playlist",
        "--format",
        format_selector.as_str(),
        video_url,
    ];

    let command_output = run_process_with_timeout(
        process_config.executable.as_str(),
        &command_arguments,
        process_config.timeout,
    )
    .context("Не удалось выполнить yt-dlp для получения streaming metadata")?;

    // Любая ошибка selection/access должна стать понятной ошибкой UI.
    ensure_yt_dlp_success(command_output.status, &command_output.stderr)?;

    // JSON должен быть валидным UTF-8.
    let stdout_text = String::from_utf8(command_output.stdout)
        .context("yt-dlp вернул metadata stdout не в UTF-8")?;

    // Парсим typed JSON, чтобы не ходить по magic string paths руками.
    serde_json::from_str(&stdout_text).context("Не удалось разобрать JSON metadata от yt-dlp")
}

/// Получает manifest formats без применения текущего service default selector-а.
pub(crate) fn resolve_youtube_candidate_metadata(
    video_url: &str,
    process_config: &YtDlpProcessConfig,
) -> Result<YtDlpMetadata> {
    let command_arguments = [
        "--quiet",
        "--no-warnings",
        "--simulate",
        "--dump-single-json",
        "--no-playlist",
        video_url,
    ];

    let command_output = run_process_with_timeout(
        process_config.executable.as_str(),
        &command_arguments,
        process_config.timeout,
    )
    .context("Не удалось выполнить yt-dlp для получения YouTube stream candidates")?;

    ensure_yt_dlp_candidate_success(command_output.status, &command_output.stderr)?;

    let stdout_text = String::from_utf8(command_output.stdout)
        .context("yt-dlp вернул candidates stdout не в UTF-8")?;

    serde_json::from_str(&stdout_text).context("Не удалось разобрать JSON candidates от yt-dlp")
}

/// Запускает внешний процесс, читает stdout/stderr параллельно и ограничивает ожидание.
fn run_process_with_timeout(
    executable: &str,
    arguments: &[&str],
    timeout: Duration,
) -> Result<ProcessOutput> {
    if timeout.is_zero() {
        bail!("process timeout должен быть положительным");
    }

    let mut child = Command::new(executable)
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Не удалось запустить process: {executable}"))?;

    let stdout = child
        .stdout
        .take()
        .context("stdout pipe process-а недоступен")?;
    let stderr = child
        .stderr
        .take()
        .context("stderr pipe process-а недоступен")?;
    let stdout_reader = spawn_pipe_reader("stdout", stdout)?;
    let stderr_reader = spawn_pipe_reader("stderr", stderr)?;

    let wait_outcome = wait_for_process_with_timeout(&mut child, timeout)?;
    let stdout = join_pipe_reader("stdout", stdout_reader)?;
    let stderr = join_pipe_reader("stderr", stderr_reader)?;

    match wait_outcome {
        ProcessWaitOutcome::Exited(status) => Ok(ProcessOutput {
            status,
            stdout,
            stderr,
        }),
        ProcessWaitOutcome::TimedOut => {
            let stderr_text = String::from_utf8_lossy(&stderr);
            let trimmed_stderr = stderr_text.trim();
            if trimmed_stderr.is_empty() {
                bail!(
                    "yt-dlp не завершился за {} мс и был остановлен",
                    timeout.as_millis()
                );
            }

            bail!(
                "yt-dlp не завершился за {} мс и был остановлен; stderr: {}",
                timeout.as_millis(),
                trimmed_stderr
            );
        }
    }
}

/// Запускает thread, который вычитывает pipe до EOF и предотвращает заполнение OS buffer-а.
fn spawn_pipe_reader<R>(
    pipe_name: &'static str,
    mut pipe: R,
) -> Result<thread::JoinHandle<io::Result<Vec<u8>>>>
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
        .with_context(|| format!("Не удалось запустить reader thread для {pipe_name}"))
}

/// Забирает bytes из pipe reader thread и превращает panic/thread IO в понятную ошибку.
fn join_pipe_reader(
    pipe_name: &'static str,
    reader: thread::JoinHandle<io::Result<Vec<u8>>>,
) -> Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("reader thread для {pipe_name} завершился panic"))?
        .with_context(|| format!("Не удалось прочитать {pipe_name} process-а"))
}

/// Ждёт child process с bounded polling и убивает его при превышении timeout-а.
fn wait_for_process_with_timeout(
    child: &mut Child,
    timeout: Duration,
) -> Result<ProcessWaitOutcome> {
    let start = Instant::now();

    loop {
        if let Some(status) = child
            .try_wait()
            .context("Не удалось проверить статус process-а")?
        {
            return Ok(ProcessWaitOutcome::Exited(status));
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
fn terminate_process(child: &mut Child) -> Result<()> {
    match child.kill() {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
        Err(error) => return Err(error).context("Не удалось остановить process по timeout-у"),
    }

    child
        .wait()
        .context("Не удалось дождаться остановленного process-а")?;
    Ok(())
}

/// Возвращает selector `yt-dlp` для выбора тестового YouTube формата.
fn youtube_format_selector() -> String {
    // Env override нужен для ручных тестов новых кодеков/разрешений без перекомпиляции.
    if let Ok(configured_selector) = std::env::var(FORMAT_SELECTOR_ENV) {
        // Пустая строка почти всегда ошибка конфигурации, поэтому не используем её.
        if !configured_selector.trim().is_empty() {
            return configured_selector;
        }
    }

    // По умолчанию используем ограниченный VP9/Opus WebM selector для текущего pipeline.
    DEFAULT_FORMAT_SELECTOR.to_string()
}

/// Преобразует неуспешный exit code `yt-dlp` в читаемую ошибку.
fn ensure_yt_dlp_success(status: ExitStatus, stderr_bytes: &[u8]) -> Result<()> {
    // Нулевой exit code означает, что `yt-dlp` считает selection успешным.
    if status.success() {
        return Ok(());
    }

    // stderr сохраняем максимально информативным, но не паникуем на битом UTF-8.
    let stderr_text = String::from_utf8_lossy(stderr_bytes);

    anyhow::bail!(
        "yt-dlp не смог выбрать поддерживаемый SDR VP9/Opus WebM поток (4K60, 4K30, 1080p60 или 1080p): {}",
        stderr_text.trim()
    );
}

/// Преобразует ошибку metadata-only candidates command в читаемую ошибку.
fn ensure_yt_dlp_candidate_success(status: ExitStatus, stderr_bytes: &[u8]) -> Result<()> {
    if status.success() {
        return Ok(());
    }

    let stderr_text = String::from_utf8_lossy(stderr_bytes);

    anyhow::bail!(
        "yt-dlp не смог получить YouTube stream candidates: {}",
        stderr_text.trim()
    );
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
        let error = run_process_with_timeout("sh", &["-c", "sleep 1"], Duration::from_millis(25))
            .expect_err("slow process times out");

        assert!(error.to_string().contains("не завершился"));
    }
}
