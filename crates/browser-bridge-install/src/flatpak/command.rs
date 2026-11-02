//! Настоящая реализация [`FlatpakTool`]: процесс `flatpak` с ограничением по времени.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::permissions::BRIDGE_TALK_NAME_MASK;
use super::{DEFAULT_FLATPAK_TIMEOUT, FlatpakError, FlatpakTool};

/// Как часто проверять, завершился ли процесс.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Сколько байт stderr сохранять в ошибке.
const STDERR_EXCERPT_BYTES: usize = 512;

/// Запускает `flatpak` из `PATH` (или указанную программу — для тестов).
#[derive(Debug, Clone)]
pub struct RealFlatpakTool {
    program: PathBuf,
    timeout: Duration,
}

impl RealFlatpakTool {
    /// `flatpak` из `PATH` с таймаутом по умолчанию.
    pub fn new() -> Self {
        Self::with_program("flatpak", DEFAULT_FLATPAK_TIMEOUT)
    }

    /// Произвольная программа и таймаут (тесты подставляют скрипт-заглушку).
    pub fn with_program(program: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            program: program.into(),
            timeout,
        }
    }

    /// Запускает команду и возвращает stdout, если код выхода нулевой.
    fn run(&self, arguments: &[&str]) -> Result<String, FlatpakError> {
        let mut child = Command::new(&self.program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => FlatpakError::NotAvailable,
                kind => FlatpakError::Io { kind },
            })?;
        // Потоки читаются в отдельных потоках, иначе заполненный канал заблокирует процесс.
        let stdout_reader = read_in_background(child.stdout.take());
        let stderr_reader = read_in_background(child.stderr.take());

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    kill_and_reap(&mut child);
                    return Err(FlatpakError::TimedOut);
                }
                Ok(None) => std::thread::sleep(POLL_INTERVAL),
                Err(error) => {
                    kill_and_reap(&mut child);
                    return Err(FlatpakError::Io { kind: error.kind() });
                }
            }
        };
        // Вывод ждём не дольше того же срока: фоновый потомок, унаследовавший канал,
        // иначе держал бы его открытым и подвесил бы нас.
        let stdout = collect_before(stdout_reader, deadline)?;
        let stderr = collect_before(stderr_reader, deadline)?;
        if status.success() {
            Ok(String::from_utf8_lossy(&stdout).into_owned())
        } else {
            let excerpt_end = stderr.len().min(STDERR_EXCERPT_BYTES);
            Err(FlatpakError::Failed {
                exit_code: status.code(),
                stderr: String::from_utf8_lossy(&stderr[..excerpt_end])
                    .trim()
                    .to_owned(),
            })
        }
    }
}

impl Default for RealFlatpakTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Процесс убиваем и собираем, чтобы не оставить зомби.
fn kill_and_reap(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Канал с результатом чтения потока.
type ReaderResult = std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>;

/// Вычитывает канал в отдельном потоке; результат приходит по каналу сообщений.
fn read_in_background<Stream: Read + Send + 'static>(
    stream: Option<Stream>,
) -> Option<ReaderResult> {
    stream.map(|mut stream| {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut collected = Vec::new();
            let result = stream.read_to_end(&mut collected).map(|_read| collected);
            // Получатель мог уйти по таймауту: тогда результат никому не нужен.
            let _ = sender.send(result);
        });
        receiver
    })
}

/// Ждёт результат чтения до срока; поток, не успевший к сроку, отпускается.
fn collect_before(
    reader: Option<ReaderResult>,
    deadline: Instant,
) -> Result<Vec<u8>, FlatpakError> {
    let Some(reader) = reader else {
        return Ok(Vec::new());
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    match reader.recv_timeout(remaining) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(error)) => Err(FlatpakError::Io { kind: error.kind() }),
        Err(_timeout_or_closed) => Err(FlatpakError::TimedOut),
    }
}

impl FlatpakTool for RealFlatpakTool {
    fn installed_apps(&self) -> Result<Vec<String>, FlatpakError> {
        match self.run(&["list", "--app", "--columns=application"]) {
            Ok(output) => Ok(output
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()),
            // Нет утилиты — значит Flatpak не используется, это не ошибка.
            Err(FlatpakError::NotAvailable) => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    fn permissions(&self, app_id: &str) -> Result<String, FlatpakError> {
        self.run(&["info", "--show-permissions", app_id])
    }

    fn grant_bridge_bus_access(&self, app_id: &str) -> Result<(), FlatpakError> {
        let talk_name_argument = format!("--talk-name={BRIDGE_TALK_NAME_MASK}");
        self.run(&["override", "--user", &talk_name_argument, app_id])
            .map(|_output| ())
    }
}
