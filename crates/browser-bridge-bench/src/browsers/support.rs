//! Общее для обоих браузеров: запуск процесса с журналом stderr, ожидание условий,
//! опрос текста страницы расширения.

use std::ffi::OsString;
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::BrowserError;
use crate::environment::process::ManagedProcess;

/// Шаг опроса условий.
pub(crate) const POLL_STEP: Duration = Duration::from_millis(200);
/// Сколько ждём штатного выхода браузера после просьбы закрыться.
pub(crate) const GRACEFUL_EXIT_WAIT: Duration = Duration::from_secs(15);
/// Сколько даём группе после SIGTERM, прежде чем убить.
pub(crate) const TERMINATE_GRACE: Duration = Duration::from_secs(5);
/// Сколько ждём, пока вся группа (в том числе посредник) исчезнет после выхода браузера.
pub(crate) const GROUP_GONE_WAIT: Duration = Duration::from_secs(5);
/// Потолок ожидания текста страницы расширения.
pub(crate) const EXTENSION_PAGE_TIMEOUT: Duration = Duration::from_secs(20);

/// Запускает браузер лидером группы; stderr (а с ним stderr посредника) пишется в файл.
pub(crate) fn spawn_browser(
    program: &Path,
    arguments: &[OsString],
    env: &[(OsString, OsString)],
    env_removed: &[OsString],
    stderr_log: &Path,
) -> Result<ManagedProcess, BrowserError> {
    let stderr = File::create(stderr_log)
        .map_err(|source| BrowserError::io(format!("create {}", stderr_log.display()), source))?;
    // Firefox дублирует консоль браузера (а с ней stderr посредника) в STDOUT, поэтому
    // оба потока пишем в один файл, как делал стенд 05 (`>log 2>&1`).
    let stdout = stderr
        .try_clone()
        .map_err(|source| BrowserError::io(format!("clone {}", stderr_log.display()), source))?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .envs(env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for name in env_removed {
        command.env_remove(name);
    }
    ManagedProcess::spawn(command).map_err(|source| BrowserError::Spawn {
        program: program.display().to_string(),
        source,
    })
}

/// Повторяет `probe`, пока она не вернёт `Some`, не больше `timeout`.
pub(crate) fn wait_until<T>(
    what: &str,
    timeout: Duration,
    mut probe: impl FnMut() -> Option<T>,
) -> Result<T, BrowserError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = probe() {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(BrowserError::Timeout {
                what: what.to_owned(),
                waited: timeout,
            });
        }
        std::thread::sleep(POLL_STEP);
    }
}

/// Ждёт «текст страницы готов»: с `expected` — пока он не появится в тексте; без него —
/// пока текст не станет непустым и не перестанет меняться между двумя опросами.
///
/// Нужна потому, что попап рисует заглушку сразу, а настоящий статус приходит от фона
/// позже. На исходе времени отдаёт последний непустой текст (вызывающий сам решит, что
/// с ним делать), а при пустом — ошибку.
pub(crate) fn settle_page_text(
    url: &str,
    expected: Option<&str>,
    mut read_text: impl FnMut() -> Result<String, BrowserError>,
) -> Result<String, BrowserError> {
    let deadline = Instant::now() + EXTENSION_PAGE_TIMEOUT;
    let mut previous = String::new();
    let mut last_error = None;
    while Instant::now() < deadline {
        match read_text() {
            Ok(text) => {
                last_error = None;
                let ready = match expected {
                    Some(needle) => text.contains(needle),
                    None => !text.is_empty() && text == previous,
                };
                if ready {
                    return Ok(text);
                }
                previous = text;
            }
            // Страница ещё грузится: ошибки скрипта в это время ожидаемы.
            Err(error) => last_error = Some(error),
        }
        std::thread::sleep(POLL_STEP);
    }
    if !previous.is_empty() {
        return Ok(previous);
    }
    Err(last_error.unwrap_or(BrowserError::ExtensionPageEmpty {
        url: url.to_owned(),
        waited: EXTENSION_PAGE_TIMEOUT,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settle_returns_text_once_it_stops_changing() {
        let mut texts = [
            "Checking",
            "Connected to Fastiplayer 0.1",
            "Connected to Fastiplayer 0.1",
        ]
        .into_iter();
        let text = settle_page_text("test://page", None, || {
            Ok(texts.next().unwrap_or("late").to_owned())
        })
        .expect("settles");
        assert_eq!(text, "Connected to Fastiplayer 0.1");
    }

    #[test]
    fn settle_waits_for_the_expected_text() {
        let mut texts = ["Checking", "Not connected", "Connected to Fastiplayer"].into_iter();
        let text = settle_page_text("test://page", Some("Connected"), || {
            Ok(texts.next().unwrap_or("late").to_owned())
        })
        .expect("finds the expected text");
        assert_eq!(text, "Connected to Fastiplayer");
    }

    #[test]
    fn wait_until_times_out_with_what_was_awaited() {
        let error =
            wait_until::<()>("never", Duration::from_millis(300), || None).expect_err("never true");
        assert!(matches!(error, BrowserError::Timeout { ref what, .. } if what == "never"));
    }
}
