//! Второй запуск: пересылка аргументов уже запущенному экземпляру (сессия 13).
//!
//! Когда process lease занят, этот процесс не становится вторым плеером. Он
//! превращает свои media-аргументы в URI, передаёт их первому экземпляру вместе с
//! билетом активации окна и завершается с кодом 0. Если передать не удалось,
//! вызывающий код показывает понятную фатальную ошибку запуска.
//!
//! Владение:
//! - здесь решается только **что** переслать: аргумент → URI (ссылка как есть,
//!   путь → абсолютный `file://` без потерь) и билет активации из окружения;
//! - **как** переслать (D-Bus, проверки, подтверждение) — `desktop_integration`;
//! - что открыть у получателя — конвейер внешнего открытия сессии 12.
//!
//! Config второго процесса не читается вовсе: lease не получен, а поведение
//! открытия определяет config первого экземпляра.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use desktop_integration::{
    ForwardedInstanceRequest, ForwardedRequestRejection, InstanceForwardingClientConfig,
    InstanceForwardingError, WindowActivationToken,
};
use thiserror::Error;

use crate::url_service_adapter::{StartupUrlClassification, classify_startup_url};

/// Сколько второй запуск ждёт, пока стартующий первый экземпляр начнёт принимать
/// запросы. Lease берётся раньше config и окна, поэтому окно «уже запущен, но
/// ещё не слушает» реально; 10 с с запасом покрывают холодный старт.
const LISTENER_WAIT: Duration = Duration::from_secs(10);
/// Как часто проверять, появился ли приёмник.
const LISTENER_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Сколько ждать ответа на сам запрос. Больше, чем таймаут подтверждения UI-потока
/// получателя (`app_shell::instance_forwarding`), чтобы получить его осмысленный
/// ответ «не отвечает», а не оборвать вызов раньше.
const CALL_TIMEOUT: Duration = Duration::from_secs(8);

/// Переменная окружения с билетом активации Wayland (xdg-activation).
const WAYLAND_ACTIVATION_TOKEN_VARIABLE: &str = "XDG_ACTIVATION_TOKEN";
/// Переменная окружения с билетом запуска X11 (startup-notification).
const X11_STARTUP_ID_VARIABLE: &str = "DESKTOP_STARTUP_ID";

/// Что именно пересылалось: от этого зависит текст ошибки для пользователя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForwardedPayload {
    /// Второй запуск без файлов: только поднять окно.
    WindowActivationOnly,
    /// Второй запуск с файлами или ссылками.
    MediaArguments,
}

impl ForwardedPayload {
    fn for_arguments(arguments: &[OsString]) -> Self {
        if arguments.is_empty() {
            Self::WindowActivationOnly
        } else {
            Self::MediaArguments
        }
    }
}

/// Почему пересылка не удалась. Технические детали — для лога, текст пользователю
/// выбирает `fatal_startup`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum RunningInstanceForwardingFailure {
    /// Относительный путь нельзя привязать: рабочая папка процесса недоступна.
    #[error("рабочая папка процесса недоступна, относительный путь не переслать")]
    CurrentDirectoryUnavailable,
    /// Запрос нарушает лимиты протокола (например, слишком много файлов).
    #[error("запрос нельзя переслать: {0}")]
    RequestNotForwardable(ForwardedRequestRejection),
    /// Транспорт не доставил запрос или получатель не принял его.
    #[error("запрос не доставлен запущенному экземпляру: {0}")]
    Delivery(InstanceForwardingError),
}

/// Ошибка пересылки вместе с тем, что пересылалось.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{failure}")]
pub(crate) struct RunningInstanceForwardingError {
    pub(crate) failure: RunningInstanceForwardingFailure,
    pub(crate) payload: ForwardedPayload,
}

/// Транспорт до запущенного экземпляра; в тестах подменяется.
pub(crate) trait RunningInstanceForwarder {
    /// Передаёт запрос и ждёт подтверждения UI-потока получателя.
    fn forward(&self, request: &ForwardedInstanceRequest) -> Result<(), InstanceForwardingError>;
}

/// Настоящий транспорт: `desktop_integration` (на Linux — D-Bus).
pub(crate) struct DesktopRunningInstanceForwarder;

impl RunningInstanceForwarder for DesktopRunningInstanceForwarder {
    fn forward(&self, request: &ForwardedInstanceRequest) -> Result<(), InstanceForwardingError> {
        desktop_integration::forward_to_running_instance(
            request,
            InstanceForwardingClientConfig {
                listener_wait: LISTENER_WAIT,
                listener_poll_interval: LISTENER_POLL_INTERVAL,
                call_timeout: CALL_TIMEOUT,
            },
        )
    }
}

/// Окружение второго процесса, из которого строится запрос.
pub(crate) struct ForwardingEnvironment {
    /// Рабочая папка для относительных путей; `None` — недоступна.
    pub(crate) current_directory: Option<PathBuf>,
    /// Билет активации окна от рабочего стола, если он выдан.
    pub(crate) activation_token: Option<WindowActivationToken>,
}

impl ForwardingEnvironment {
    /// Читает рабочую папку и билет активации текущего процесса.
    pub(crate) fn of_current_process() -> Self {
        Self {
            current_directory: std::env::current_dir().ok(),
            activation_token: activation_token_from(|variable| std::env::var(variable).ok()),
        }
    }
}

/// Строит запрос из аргументов и отдаёт его транспорту.
pub(crate) fn forward_arguments_to_running_instance(
    arguments: &[OsString],
    environment: ForwardingEnvironment,
    forwarder: &impl RunningInstanceForwarder,
) -> Result<(), RunningInstanceForwardingError> {
    let payload = ForwardedPayload::for_arguments(arguments);
    let into_error = |failure| RunningInstanceForwardingError { failure, payload };

    let request = forwarded_request_from_arguments(arguments, environment).map_err(into_error)?;
    forwarder
        .forward(&request)
        .map_err(RunningInstanceForwardingFailure::Delivery)
        .map_err(into_error)?;
    tracing::info!(
        forwarded_arguments = arguments.len(),
        "Аргументы переданы уже запущенному экземпляру"
    );
    Ok(())
}

/// Аргументы → запрос: пустой список — «поднять окно», иначе «открыть URI».
fn forwarded_request_from_arguments(
    arguments: &[OsString],
    environment: ForwardingEnvironment,
) -> Result<ForwardedInstanceRequest, RunningInstanceForwardingFailure> {
    let ForwardingEnvironment {
        current_directory,
        activation_token,
    } = environment;
    if arguments.is_empty() {
        return Ok(ForwardedInstanceRequest::activate(activation_token));
    }
    let uris = arguments
        .iter()
        .map(|argument| forwarded_uri_for_argument(argument, current_directory.as_deref()))
        .collect::<Result<Vec<String>, _>>()?;
    ForwardedInstanceRequest::open(uris, activation_token)
        .map_err(RunningInstanceForwardingFailure::RequestNotForwardable)
}

/// Один аргумент → URI.
///
/// Ссылкой считается то же, что считает ссылкой первый запуск
/// (`classify_startup_url`): иначе один и тот же аргумент открывался бы по-разному
/// в зависимости от того, запущен ли уже плеер. Ссылка уходит как есть, ничего
/// не обрезается и не нормализуется.
fn forwarded_uri_for_argument(
    argument: &OsStr,
    current_directory: Option<&Path>,
) -> Result<String, RunningInstanceForwardingFailure> {
    if let Some(utf8_argument) = argument.to_str()
        && !matches!(
            classify_startup_url(utf8_argument),
            StartupUrlClassification::NotUrl
        )
    {
        return Ok(utf8_argument.to_owned());
    }
    let absolute_path = absolute_path_for(Path::new(argument), current_directory)?;
    file_uri_for(&absolute_path)
}

/// Относительный путь привязывается к рабочей папке второго процесса: у первого
/// экземпляра она другая. Путь не канонизируется (символические ссылки и `..`
/// сохраняются) — получатель открывает его так же, как открыл бы первый запуск.
fn absolute_path_for(
    path: &Path,
    current_directory: Option<&Path>,
) -> Result<PathBuf, RunningInstanceForwardingFailure> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    current_directory
        .map(|directory| directory.join(path))
        .ok_or(RunningInstanceForwardingFailure::CurrentDirectoryUnavailable)
}

#[cfg(unix)]
fn file_uri_for(absolute_path: &Path) -> Result<String, RunningInstanceForwardingFailure> {
    // `absolute_path_for` гарантирует абсолютный путь; `None` здесь был бы ошибкой
    // программы, поэтому он честно сообщается как «рабочая папка недоступна».
    crate::external_open::uri::file_uri_from_absolute_path(absolute_path)
        .ok_or(RunningInstanceForwardingFailure::CurrentDirectoryUnavailable)
}

#[cfg(not(unix))]
fn file_uri_for(_absolute_path: &Path) -> Result<String, RunningInstanceForwardingFailure> {
    // Транспорта пересылки на этих платформах нет (`desktop_integration` вернёт
    // UnsupportedPlatform); кодирование путей появится вместе с ним.
    Err(RunningInstanceForwardingFailure::Delivery(
        InstanceForwardingError::UnsupportedPlatform,
    ))
}

/// Билет активации: сначала Wayland, затем X11. Непригодное значение пропускается.
fn activation_token_from(
    read_variable: impl Fn(&str) -> Option<String>,
) -> Option<WindowActivationToken> {
    [WAYLAND_ACTIVATION_TOKEN_VARIABLE, X11_STARTUP_ID_VARIABLE]
        .into_iter()
        .filter_map(read_variable)
        .find_map(WindowActivationToken::from_raw)
}

#[cfg(test)]
mod tests;
