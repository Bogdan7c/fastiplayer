//! Перезапуск приложения после штатного выхода («Restart now» в настройках).
//!
//! Главная ловушка: пока старый процесс держит замок single-instance
//! (`fastiplayer.instance.lock`), новый процесс видит «плеер уже запущен», пересылает
//! активацию по D-Bus и выходит с кодом 0, то есть наивное «запустить себя и выйти»
//! проигрывает гонку. Поэтому порядок жёстко закреплён ТИПАМИ:
//!
//! 1. замок отпускается через [`AppInstanceLease::release`], что даёт токен
//!    [`InstanceLeaseReleased`];
//! 2. запуск нового процесса ([`relaunch_after_release`]) принимает этот токен и без него
//!    не вызывается.
//!
//! Решения владельца и консервативные выборы:
//! - Новый процесс стартует БЕЗ аргументов: очередь и позицию восстанавливает обычный старт
//!   (`playlist-state.json` + resume-файл); пауза не сохраняется (прежнее поведение старта).
//! - Если закрытие прошло с ошибкой (`ShutdownQuality::Degraded`), перезапуск НЕ делается:
//!   состояние могло сохраниться не полностью, а автоматически открывать новое окно поверх
//!   неполного состояния небезопасно. Причина пишется в лог. Таймаут закрытия вообще
//!   завершает процесс кодом 70 внутри `finish_process_shutdown`, до этого модуля не доходя.
//! - Отказ запуска (бинарь удалён или заменён, ошибка spawn) никогда не молчит:
//!   [`RelaunchOutcome::report`] пишет `error!` в лог и строку в stderr.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use thiserror::Error;
use tracing::{error, info, warn};

use super::AppInstanceLease;

/// Суффикс, который Linux добавляет к пути `/proc/self/exe`, если файл запущенного
/// бинаря удалён или заменён (например, при обновлении пакета).
const DELETED_EXECUTABLE_SUFFIX: &str = " (deleted)";

/// Просили ли перезапустить приложение после выхода.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RelaunchRequest {
    /// Обычный выход.
    #[default]
    NotRequested,

    /// Пользователь нажал «Restart now»: после выхода запустить приложение снова.
    AfterExit,
}

/// Как закончилось штатное закрытие процесса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShutdownQuality {
    /// Все владельцы завершились без ошибок: состояние очереди и настроек сохранено.
    Clean,

    /// Закрытие дошло до конца, но с типизированной ошибкой (или завершилось аварийно).
    Degraded,
}

/// Решение о перезапуске, принятое из запроса и качества закрытия.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelaunchDecision {
    /// Перезапуск не просили.
    NotRequested,

    /// Просили и закрытие чистое: запускать новый процесс.
    Relaunch,

    /// Просили, но закрытие прошло с ошибкой: не перезапускаем (см. заголовок модуля).
    SkippedAfterDegradedShutdown,
}

/// Принимает решение о перезапуске (чистая функция, без побочных эффектов).
pub(crate) const fn decide_relaunch(
    request: RelaunchRequest,
    shutdown: ShutdownQuality,
) -> RelaunchDecision {
    match (request, shutdown) {
        (RelaunchRequest::NotRequested, _) => RelaunchDecision::NotRequested,
        (RelaunchRequest::AfterExit, ShutdownQuality::Clean) => RelaunchDecision::Relaunch,
        (RelaunchRequest::AfterExit, ShutdownQuality::Degraded) => {
            RelaunchDecision::SkippedAfterDegradedShutdown
        }
    }
}

/// Доказательство, что замок single-instance уже отпущен. Создаётся только в
/// [`AppInstanceLease::release`], потребляя сам замок.
#[derive(Debug)]
pub(crate) struct InstanceLeaseReleased(());

impl AppInstanceLease {
    /// Отпускает замок и возвращает токен, без которого перезапуск не вызвать.
    pub(crate) fn release(self) -> InstanceLeaseReleased {
        // Освобождение — это Drop guard-а (закрытие дескриптора снимает flock).
        drop(self);
        InstanceLeaseReleased(())
    }
}

/// Итог попытки перезапуска.
#[derive(Debug)]
pub(crate) enum RelaunchOutcome {
    /// Перезапуск не просили.
    NotRequested,

    /// Просили, но не выполнили из-за ошибки закрытия (в лог уже записано).
    SkippedAfterDegradedShutdown,

    /// Новый процесс запущен.
    Spawned {
        /// Запущенный исполняемый файл.
        program: PathBuf,
    },

    /// Запуск не удался; причина записана в лог и в stderr.
    Failed(RelaunchError),
}

/// Почему перезапуск не состоялся.
#[derive(Debug, Error)]
pub(crate) enum RelaunchError {
    /// Не удалось узнать путь собственного бинаря.
    #[error("could not determine the path of the running executable: {0}")]
    ExecutablePathUnknown(io::Error),

    /// Файл бинаря удалён или заменён после запуска (типично при обновлении пакета).
    #[error(
        "the executable was removed or replaced while running ({program}); start the player manually"
    )]
    ExecutableReplaced {
        /// Путь с суффиксом ` (deleted)` или несуществующий путь.
        program: String,
    },

    /// Операционная система отказала в запуске процесса.
    #[error("could not start {program}: {source}")]
    SpawnFailed {
        /// Путь, который пытались запустить.
        program: String,
        /// Исходная ошибка ОС.
        source: io::Error,
    },
}

/// Порт запуска процесса: реальный в production, fake в тестах.
pub(crate) trait ProgramSpawner {
    /// Запускает `program` без аргументов, не дожидаясь завершения.
    fn spawn_detached(&self, program: &Path) -> io::Result<()>;
}

/// Реальный запуск через `std::process::Command`.
pub(crate) struct SystemProgramSpawner;

impl ProgramSpawner for SystemProgramSpawner {
    fn spawn_detached(&self, program: &Path) -> io::Result<()> {
        // Без аргументов: очередь и позицию восстановит обычный старт. stdin закрыт,
        // stdout/stderr наследуются (логи нового процесса идут туда же).
        let child = Command::new(program).stdin(Stdio::null()).spawn()?;
        // Ждать потомка не нужно: после выхода родителя его усыновит init. Drop `Child`
        // не убивает процесс.
        drop(child);
        Ok(())
    }
}

/// Источник пути собственного бинаря (fake-able).
pub(crate) trait ExecutablePathSource {
    /// Путь исполняемого файла текущего процесса.
    fn current_executable(&self) -> io::Result<PathBuf>;
}

/// Реальный источник: `std::env::current_exe()`.
pub(crate) struct SystemExecutablePath;

impl ExecutablePathSource for SystemExecutablePath {
    fn current_executable(&self) -> io::Result<PathBuf> {
        std::env::current_exe()
    }
}

/// Принимает решение и выполняет перезапуск (если нужен). Исход сообщается через
/// [`RelaunchOutcome::report`].
///
/// `_released` не используется как данные: это квитанция порядка «замок отпущен -> запуск».
pub(crate) fn relaunch_after_release(
    _released: InstanceLeaseReleased,
    request: RelaunchRequest,
    shutdown: ShutdownQuality,
    executable: &impl ExecutablePathSource,
    spawner: &impl ProgramSpawner,
) -> RelaunchOutcome {
    match decide_relaunch(request, shutdown) {
        RelaunchDecision::NotRequested => RelaunchOutcome::NotRequested,
        RelaunchDecision::SkippedAfterDegradedShutdown => {
            RelaunchOutcome::SkippedAfterDegradedShutdown
        }
        RelaunchDecision::Relaunch => match spawn_replacement(executable, spawner) {
            Ok(program) => RelaunchOutcome::Spawned { program },
            Err(relaunch_error) => RelaunchOutcome::Failed(relaunch_error),
        },
    }
}

impl RelaunchOutcome {
    /// Записывает исход: успех и пропуск в лог, отказ запуска ещё и в stderr (окно уже
    /// закрыто, пользователь мог запустить плеер из терминала). Ничего не глотает молча.
    pub(crate) fn report(&self) {
        match self {
            Self::NotRequested => {}
            Self::SkippedAfterDegradedShutdown => {
                warn!("Restart was requested, but shutdown finished with errors; not restarting");
            }
            Self::Spawned { program } => {
                info!(program = %program.display(), "Restarted the application");
            }
            Self::Failed(relaunch_error) => {
                error!(%relaunch_error, "Could not restart the application");
                eprintln!("fastiplayer: could not restart the application: {relaunch_error}");
            }
        }
    }
}

/// Находит собственный бинарь и запускает его.
fn spawn_replacement(
    executable: &impl ExecutablePathSource,
    spawner: &impl ProgramSpawner,
) -> Result<PathBuf, RelaunchError> {
    let program = executable
        .current_executable()
        .map_err(RelaunchError::ExecutablePathUnknown)?;
    // Удалённый или заменённый бинарь: Linux дописывает суффикс к пути, а такого файла нет.
    // Консервативно НЕ подменяем путь «очищенным»: это был бы запуск другого файла.
    if program
        .to_string_lossy()
        .ends_with(DELETED_EXECUTABLE_SUFFIX)
        || !program.exists()
    {
        return Err(RelaunchError::ExecutableReplaced {
            program: program.display().to_string(),
        });
    }
    spawner
        .spawn_detached(&program)
        .map_err(|source| RelaunchError::SpawnFailed {
            program: program.display().to_string(),
            source,
        })?;
    Ok(program)
}

#[cfg(test)]
mod tests;
