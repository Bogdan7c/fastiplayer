//! Фатальные ошибки запуска: один тип, человеческий текст, показ и код выхода.
//!
//! Раньше часть ошибок запуска (нет Vulkan, окно не создалось, внутренний сбой)
//! уходила только в лог, а процесс завершался с кодом 0; ошибки до окна
//! (аргументы, папка настроек, «уже запущен») печатались только в stderr. При
//! запуске из файлового менеджера пользователь не видел ничего.
//!
//! Теперь любой путь фатального выхода превращается в `FatalStartupError`:
//! - причина (`FatalStartupReason`) определяет человеческий текст;
//! - техническая деталь уходит только в лог;
//! - `conclude_process` показывает ошибку через `FatalStartupPresenter` и
//!   возвращает ненулевой код выхода.
//!
//! Модуль не решает, *когда* процесс должен завершиться: это остаётся за bootstrap
//! (`app_instance`), `AppShell` и `main`. Он владеет только описанием ошибки и показом.

mod bootstrap_mapping;
mod config_location;
mod desktop_notification_channel;
mod dialog_utility;
mod messages;
mod presentation;

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

use std::fmt;
use std::process::ExitCode;

pub(crate) use config_location::ConfigDirectoryLocation;
pub(crate) use presentation::{FatalStartupPresenter, SystemFatalStartupPresenter};

use crate::app_instance::ProcessArgsError;

/// Код выхода при фатальной ошибке запуска: «общая ошибка» по конвенции Unix.
///
/// Отличается от кода 70 (`TERMINAL_SHUTDOWN_TIMEOUT_EXIT_CODE`), которым процесс
/// аварийно выходит, если не уложился в срок штатного завершения.
const FATAL_STARTUP_EXIT_CODE: u8 = 1;

/// Почему приложение не смогло запуститься. Определяет текст для пользователя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FatalStartupReason {
    /// Неверные аргументы командной строки.
    InvalidArguments(ProcessArgsError),
    /// Система не сообщила, где у пользователя папка настроек.
    ConfigLocationUnknown,
    /// Другой экземпляр плеера уже работает, а поднять его окно не удалось.
    AlreadyRunning,
    /// Плеер уже запущен, но не ответил на запрос второго запуска (завис).
    RunningInstanceNotResponding,
    /// Плеер уже запущен, но как раз закрывается.
    RunningInstanceShuttingDown,
    /// Плеер уже запущен, но передать ему файлы не удалось.
    RunningInstanceDidNotTakeFiles,
    /// Папку настроек нельзя безопасно использовать.
    ConfigDirectoryUnusable {
        problem: ConfigDirectoryProblem,
        location: ConfigDirectoryLocation,
    },
    /// Файл настроек не удалось прочитать или создать (битый файл сюда не попадает:
    /// его восстанавливает config-слой).
    ConfigFileUnusable { location: ConfigDirectoryLocation },
    /// Сборка для платформы, где запуск пока не поддерживается.
    UnsupportedPlatform,
    /// Нет подключения к графическому сеансу (Wayland/X11).
    GraphicalSessionUnavailable,
    /// Графический сеанс есть, но окно создать не удалось.
    WindowCreationFailed,
    /// Нет пригодной видеокарты/драйвера Vulkan для вывода изображения.
    GraphicsUnavailable,
    /// Внутренний сбой плеера, который пользователь сам исправить не может.
    InternalFailure,
}

/// Что именно не так с папкой настроек.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigDirectoryProblem {
    /// Папка или файл блокировки принадлежат другому пользователю (обычно root после `sudo`).
    OwnedByAnotherUser,
    /// На месте папки лежит файл.
    NotADirectory,
    /// Файл блокировки экземпляра — не обычный файл (например, ссылка или папка).
    LockFileNotRegularFile { lock_file_name: String },
    /// Файл блокировки подменили, пока плеер его проверял.
    LockFileReplacedDuringStartup,
    /// Система запретила доступ к папке.
    AccessDenied,
    /// Диск с папкой смонтирован только для чтения.
    ReadOnlyStorage,
    /// На диске с папкой нет места.
    StorageFull,
    /// Прочая системная ошибка ввода-вывода.
    SystemError,
}

/// Фатальная ошибка запуска: причина для пользователя и деталь для лога.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FatalStartupError {
    reason: FatalStartupReason,
    technical_detail: String,
}

impl FatalStartupError {
    /// Собирает ошибку из причины и технической детали, которая пойдёт только в лог.
    pub(crate) fn new(reason: FatalStartupReason, technical_detail: impl Into<String>) -> Self {
        Self {
            reason,
            technical_detail: technical_detail.into(),
        }
    }

    /// Winit не смог подключиться к Wayland/X11 и создать event loop.
    pub(crate) fn graphical_session_unavailable(error: &impl fmt::Display) -> Self {
        Self::new(
            FatalStartupReason::GraphicalSessionUnavailable,
            format!("не удалось создать event loop: {error}"),
        )
    }

    /// Winit не создал окно приложения.
    pub(crate) fn window_creation_failed(error: &impl fmt::Display) -> Self {
        Self::new(
            FatalStartupReason::WindowCreationFailed,
            format!("не удалось создать окно: {error}"),
        )
    }

    /// Рендерер не поднялся: нет адаптера Vulkan, surface или device.
    ///
    /// `{:#}` сохраняет всю цепочку причин anyhow (включая ответ wgpu), которую
    /// прежний лог с `{}` терял, оставляя только верхний контекст.
    pub(crate) fn graphics_unavailable(error: &anyhow::Error) -> Self {
        Self::new(
            FatalStartupReason::GraphicsUnavailable,
            format!("не удалось инициализировать рендерер: {error:#}"),
        )
    }

    /// Внутренний сбой на этапе `stage`; деталь остаётся в логе.
    pub(crate) fn internal_failure(stage: &str, error: &impl fmt::Display) -> Self {
        Self::new(
            FatalStartupReason::InternalFailure,
            format!("{stage}: {error:#}"),
        )
    }

    /// Причина, по которой выбирается текст для пользователя.
    pub(crate) fn reason(&self) -> &FatalStartupReason {
        &self.reason
    }

    /// Техническая деталь для лога; пользователю не показывается.
    pub(crate) fn technical_detail(&self) -> &str {
        &self.technical_detail
    }
}

/// Хранит первую фатальную ошибку, которую event loop зафиксировал до остановки.
///
/// Показ откладывается до конца `main`: сначала штатно завершаются все owners и
/// освобождается lease, иначе повторный запуск, пока висит окно ошибки, ответил бы
/// «плеер уже запущен».
#[derive(Debug, Default)]
pub(crate) struct FatalStartupErrorSlot {
    first_error: Option<FatalStartupError>,
}

impl FatalStartupErrorSlot {
    /// Запоминает ошибку. Если ошибка уже есть, побеждает первая (она и есть
    /// причина остановки), а более поздняя пишется в лог, чтобы не потеряться.
    ///
    /// Первая ошибка тоже сразу пишется в лог: между остановкой event loop и показом
    /// идёт shutdown, который при зависании owners завершает процесс аварийно
    /// (код 70), и тогда до показа дело не дойдёт.
    pub(crate) fn record(&mut self, error: FatalStartupError) {
        match &self.first_error {
            None => {
                tracing::error!(
                    reason = ?error.reason(),
                    detail = %error.technical_detail(),
                    "Фатальная ошибка запуска; event loop останавливается"
                );
                self.first_error = Some(error);
            }
            Some(_) => tracing::warn!(
                reason = ?error.reason(),
                detail = %error.technical_detail(),
                "Повторная фатальная ошибка после уже зафиксированной; показана будет первая"
            ),
        }
    }

    /// Забирает ошибку для показа; повторный вызов вернёт `None`.
    pub(crate) fn take(&mut self) -> Option<FatalStartupError> {
        self.first_error.take()
    }
}

/// Чем закончился процесс с точки зрения пользователя и ОС.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessConclusion {
    /// Плеер штатно отработал и закрыт.
    Completed,
    /// Плеер не смог запуститься; ошибка уже показана.
    FailedToStart,
}

impl ProcessConclusion {
    /// Код выхода процесса: 0 только для штатного завершения.
    pub(crate) fn exit_code(self) -> ExitCode {
        match self {
            Self::Completed => ExitCode::SUCCESS,
            Self::FailedToStart => ExitCode::from(FATAL_STARTUP_EXIT_CODE),
        }
    }
}

/// Завершает процесс: при фатальной ошибке показывает её пользователю.
///
/// Единственная точка, где фатальная ошибка запуска превращается в показ и код
/// выхода; все пути (bootstrap, event loop, окно, GPU) приходят сюда через `main`.
pub(crate) fn conclude_process(
    run_outcome: Result<(), FatalStartupError>,
    presenter: &impl FatalStartupPresenter,
) -> ProcessConclusion {
    match run_outcome {
        Ok(()) => ProcessConclusion::Completed,
        Err(error) => {
            presenter.present(&error);
            ProcessConclusion::FailedToStart
        }
    }
}
