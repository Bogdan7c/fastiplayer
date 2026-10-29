//! Выбор режима процесса до любого bootstrap: обычный плеер или браузерный режим.
//!
//! Браузерный режим — тот же бинарь с ключом [`BROWSER_SESSION_OPTION`] (решение 60
//! плана `user/browser-bridge/`). Его запускает D-Bus activation по звонку посредника
//! экстеншена, а не человек. Поэтому режим выбирается ДО путей, lease и config:
//! браузерный процесс не берёт замок единственного экземпляра обычного плеера
//! (иначе сработала бы пересылка «второй запуск → первый») и не читает его состояние
//! (инвариант 7: браузерный режим ничего не пишет о просмотре).

use std::ffi::{OsStr, OsString};

use super::ProcessArgsError;

/// Служебный ключ браузерного режима. Единственный источник — `desktop-integration`:
/// тот же ключ он пишет в `.service`-файл D-Bus activation.
const BROWSER_SESSION_OPTION: &str = desktop_integration::BROWSER_SESSION_ARGUMENT;

/// Каким процессом станет этот запуск.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessMode {
    /// Обычный плеер: окно, очередь, пересылка второго запуска.
    Player,
    /// Процесс браузерного режима (мост в браузер): своё имя на шине, без очереди.
    BrowserSession,
}

/// Определяет режим по аргументам без имени исполняемого файла.
///
/// Ключ [`BROWSER_SESSION_OPTION`] учитывается только до `--` (после `--` это имя
/// файла, как и любая другая строка). Он обязан быть единственным аргументом:
/// браузерный процесс не принимает файлы и `--fullscreen`, а смешанный запуск — почти
/// наверняка ошибка в `.service`-файле, о которой лучше сказать сразу.
pub(crate) fn select_process_mode(arguments: &[OsString]) -> Result<ProcessMode, ProcessArgsError> {
    let options = arguments
        .iter()
        .take_while(|argument| argument.as_os_str() != OsStr::new("--"));
    let requests_browser_session = options
        .into_iter()
        .any(|argument| argument.as_os_str() == OsStr::new(BROWSER_SESSION_OPTION));

    if !requests_browser_session {
        return Ok(ProcessMode::Player);
    }
    if arguments.len() == 1 {
        Ok(ProcessMode::BrowserSession)
    } else {
        Err(ProcessArgsError::BrowserSessionWithOtherArguments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn no_arguments_and_media_arguments_select_the_normal_player() {
        assert_eq!(select_process_mode(&[]), Ok(ProcessMode::Player));
        assert_eq!(
            select_process_mode(&arguments(&["--fullscreen", "movie.mkv"])),
            Ok(ProcessMode::Player)
        );
    }

    #[test]
    fn lone_browser_session_option_selects_browser_mode() {
        assert_eq!(
            select_process_mode(&arguments(&["--browser-session"])),
            Ok(ProcessMode::BrowserSession)
        );
    }

    #[test]
    fn browser_session_option_mixed_with_anything_is_rejected() {
        for mixed in [
            &["--browser-session", "movie.mkv"][..],
            &["--fullscreen", "--browser-session"][..],
            &["--browser-session", "--browser-session"][..],
            &["--browser-session", "--"][..],
        ] {
            assert_eq!(
                select_process_mode(&arguments(mixed)),
                Err(ProcessArgsError::BrowserSessionWithOtherArguments),
                "{mixed:?}"
            );
        }
    }

    #[test]
    fn browser_session_after_double_dash_is_a_file_name() {
        // Файл с таким именем открывается обычным плеером, как любой другой.
        assert_eq!(
            select_process_mode(&arguments(&["--", "--browser-session"])),
            Ok(ProcessMode::Player)
        );
    }
}
