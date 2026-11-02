//! Открыть страницу магазина В КОНКРЕТНОМ браузере пользователя.
//!
//! Для Google Chrome и семейства Firefox тихой установки нет: плеер открывает
//! страницу нашего экстеншена в нужном браузере, а «Add» жмёт человек. Шов
//! [`BrowserLauncher`] отделяет установщик от процессов: в тестах стоит
//! записывающий фейк, в приложении — [`RealBrowserLauncher`].

use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use thiserror::Error;

/// Как запустить конкретный браузер.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserLaunchCommand {
    /// Исполняемый файл, найденный в `PATH` (`<exe> <url>`).
    Executable(PathBuf),
    /// Flatpak-приложение (`flatpak run <app id> <url>`).
    Flatpak {
        /// Идентификатор приложения (`com.google.Chrome`).
        app_id: String,
    },
}

/// Почему браузер не запустился.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LaunchError {
    /// Программа для запуска не найдена (для Flatpak — нет утилиты `flatpak`).
    #[error("The program to open the browser was not found")]
    ProgramNotFound,
    /// Процесс не удалось создать.
    #[error("Could not start the browser: {0:?}")]
    Spawn(io::ErrorKind),
}

/// Открывает адрес в браузере.
pub trait BrowserLauncher: Send + Sync {
    /// Запускает браузер с адресом и сразу возвращается (не ждёт закрытия браузера).
    fn open_url(&self, command: &BrowserLaunchCommand, url: &str) -> Result<(), LaunchError>;
}

/// Настоящий запуск процессов.
#[derive(Debug, Clone)]
pub struct RealBrowserLauncher {
    flatpak_program: PathBuf,
}

impl RealBrowserLauncher {
    /// `flatpak` из `PATH`.
    pub fn new() -> Self {
        Self::with_flatpak_program("flatpak")
    }

    /// Своя программа вместо `flatpak` (тесты подставляют скрипт-заглушку).
    pub fn with_flatpak_program(program: impl Into<PathBuf>) -> Self {
        Self {
            flatpak_program: program.into(),
        }
    }
}

impl Default for RealBrowserLauncher {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserLauncher for RealBrowserLauncher {
    fn open_url(&self, command: &BrowserLaunchCommand, url: &str) -> Result<(), LaunchError> {
        let mut process = match command {
            BrowserLaunchCommand::Executable(executable) => {
                let mut process = Command::new(executable);
                process.arg(url);
                process
            }
            BrowserLaunchCommand::Flatpak { app_id } => {
                let mut process = Command::new(&self.flatpak_program);
                process.args(["run", app_id, url]);
                process
            }
        };
        process
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach_from_our_process_group(&mut process);
        let child = process.spawn().map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => LaunchError::ProgramNotFound,
            kind => LaunchError::Spawn(kind),
        })?;
        reap_in_background(child);
        Ok(())
    }
}

/// Своя группа процессов: закрытие плеера (или Ctrl+C в его терминале) не должно
/// убивать браузер, который мы запустили.
#[cfg(unix)]
fn detach_from_our_process_group(process: &mut Command) {
    use std::os::unix::process::CommandExt;
    process.process_group(0);
}

/// Вне Unix группы процессов нет.
#[cfg(not(unix))]
fn detach_from_our_process_group(_process: &mut Command) {}

/// Дочерний процесс надо дождаться, иначе после выхода останется зомби. Ждёт
/// именованный поток: браузер может жить часами, а плеер не должен блокироваться.
fn reap_in_background(mut child: std::process::Child) {
    let spawned = std::thread::Builder::new()
        .name("browser-launch-reaper".to_owned())
        .spawn(move || {
            if let Err(error) = child.wait() {
                tracing::debug!(kind = ?error.kind(), "waiting for the launched browser failed");
            }
        });
    if let Err(error) = spawned {
        // Браузер уже запущен — это главное; без потока зомби уйдёт вместе с плеером.
        tracing::warn!(kind = ?error.kind(), "could not start the browser reaper thread");
    }
}
