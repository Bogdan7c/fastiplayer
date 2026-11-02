//! Chromium под управлением DevTools (CDP).
//!
//! Профиль — НЕ отдельный `--user-data-dir`: по умолчанию Chromium берёт
//! `$XDG_CONFIG_HOME/chromium`, а это временный каталог стенда, куда установщик моста
//! записал `NativeMessagingHosts/<имя>.json`. Порт отладки — `--remote-debugging-port=0`,
//! выбранный браузером порт читаем из файла `DevToolsActivePort` в профиле.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use browser_bridge_protocol::extension_identity::CHROMIUM_DEVELOPMENT_EXTENSION_ID;
use serde_json::Value;

use super::devtools::{CdpConnection, http_json, http_request};
use super::hand_off::{
    HAND_OFF_EXTENSION_PAGE, HandOffError, HandOffOutcome, ROBOT_FUNCTION, parse_robot_output,
};
use super::support::{
    EXTENSION_PAGE_TIMEOUT, GRACEFUL_EXIT_WAIT, GROUP_GONE_WAIT, POLL_STEP, TERMINATE_GRACE,
    settle_page_text, spawn_browser, wait_until,
};
use super::{
    BrowserError, BrowserExit, BrowserKind, BrowserUnderTest, ExtensionPage, ExtensionSource,
    LaunchOptions, TabHandle,
};
use crate::environment::BenchEnvironment;
use crate::environment::process::ManagedProcess;

/// Сколько ждать, пока Chromium запишет порт отладки.
const DEVTOOLS_PORT_WAIT: Duration = Duration::from_secs(40);
/// Сколько ждать ответ фона на `hand_off_tab`: он сначала поднимает связь с плеером
/// (будит его через посредника), и только потом отвечает.
const HAND_OFF_REPLY_WAIT: Duration = Duration::from_secs(60);
/// Имя файла с портом отладки в каталоге профиля.
const ACTIVE_PORT_FILE: &str = "DevToolsActivePort";

/// Запущенный Chromium.
pub(crate) struct Chromium {
    process: ManagedProcess,
    port: u16,
    stderr_log: PathBuf,
}

/// Аргументы командной строки (отдельная чистая функция: её проверяют тесты).
pub(crate) fn command_line(
    options: &LaunchOptions,
    virtual_display: bool,
) -> Result<Vec<OsString>, BrowserError> {
    let mut unpacked = Vec::new();
    for source in &options.extensions {
        match source {
            ExtensionSource::UnpackedDir(directory) => unpacked.push(directory.clone()),
            ExtensionSource::PackedFile(file) => {
                return Err(BrowserError::UnsupportedExtension(format!(
                    "Chromium loads only unpacked extension directories, got file {}",
                    file.display()
                )));
            }
        }
    }
    let mut arguments: Vec<OsString> = [
        "--no-first-run",
        "--no-default-browser-check",
        "--remote-debugging-port=0",
        // Без связки ключей: во временном HOME её нет, а запрос пароля повис бы.
        "--password-store=basic",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    if options.autoplay_without_gesture {
        arguments.push("--autoplay-policy=no-user-gesture-required".into());
    }
    if virtual_display {
        arguments.push("--ozone-platform=wayland".into());
    }
    if !unpacked.is_empty() {
        // `--load-extension` принимает список через запятую.
        let list = unpacked
            .iter()
            .map(|path| path.to_string_lossy())
            .collect::<Vec<_>>()
            .join(",");
        arguments.push(format!("--load-extension={list}").into());
    }
    arguments.push("about:blank".into());
    Ok(arguments)
}

/// Запускает Chromium и ждёт порт отладки.
pub(crate) fn launch(
    environment: &BenchEnvironment,
    options: &LaunchOptions,
    executable: &Path,
) -> Result<Chromium, BrowserError> {
    let virtual_display =
        environment.display_target() == crate::environment::DisplayTarget::Virtual;
    let arguments = command_line(options, virtual_display)?;
    let profile = environment.xdg_config_home().join("chromium");
    // Файл от прошлого запуска в этом же профиле обманул бы ожидание порта.
    let port_file = profile.join(ACTIVE_PORT_FILE);
    let _stale_port_file = std::fs::remove_file(&port_file);
    let stderr_log = BrowserKind::Chromium.stderr_log_path(environment.work_dir());
    let process = spawn_browser(
        executable,
        &arguments,
        &environment.browser_command_env(),
        &environment.browser_command_env_removed(),
        &stderr_log,
    )?;
    let port = wait_until("Chromium DevToolsActivePort", DEVTOOLS_PORT_WAIT, || {
        let content = std::fs::read_to_string(&port_file).ok()?;
        content.lines().next()?.trim().parse::<u16>().ok()
    })?;
    Ok(Chromium {
        process,
        port,
        stderr_log,
    })
}

impl Chromium {
    /// Открывает адрес в новой вкладке и возвращает описание цели DevTools.
    fn new_target(&self, url: &str) -> Result<Value, BrowserError> {
        http_json(
            self.port,
            "PUT",
            &format!("/json/new?{}", percent_encode_query(url)),
        )
    }

    /// Выполняет скрипт робота в странице экстеншена; пока страница не получила API
    /// экстеншена (грузится), повторяет попытку.
    fn run_robot_in(
        &self,
        target: &Value,
        page_url_prefix: &str,
    ) -> Result<HandOffOutcome, BrowserError> {
        let socket_url = target
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserError::protocol("DevTools", format!("no socket url: {target}"))
            })?;
        let mut connection = CdpConnection::connect(socket_url)?;
        let prefix_literal = Value::String(page_url_prefix.to_owned()).to_string();
        let expression = format!("({ROBOT_FUNCTION})(globalThis.chrome, {prefix_literal})");
        let deadline = Instant::now() + EXTENSION_PAGE_TIMEOUT;
        loop {
            let attempt = connection
                .evaluate_string_promise(&expression, HAND_OFF_REPLY_WAIT)
                .and_then(|raw| Ok(parse_robot_output(&raw, page_url_prefix)?));
            match attempt {
                // Страница ещё грузится: выражение падает или API нет — пробуем снова.
                Err(BrowserError::HandOff(HandOffError::PageNotReady))
                | Err(BrowserError::CommandFailed { .. })
                    if Instant::now() < deadline =>
                {
                    std::thread::sleep(POLL_STEP);
                }
                other => return other,
            }
        }
    }

    fn close_target(&self, target_id: &str) -> Result<(), BrowserError> {
        http_request(self.port, "GET", &format!("/json/close/{target_id}")).map(|_| ())
    }
}

/// Кодирует адрес для `PUT /json/new?<адрес>`.
///
/// Chromium разбирает хвост как набор параметров запроса: незакодированный `&` обрезает
/// адрес (`?run=x&mode=handoff` терял `mode=handoff`, и страница молча оставалась в обычном режиме).
/// Кодируем всё, кроме безопасных знаков; Chromium раскодирует адрес обратно.
fn percent_encode_query(url: &str) -> String {
    let mut encoded = String::with_capacity(url.len());
    for byte in url.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b':' | b'/') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn target_id(target: &Value) -> Result<String, BrowserError> {
    target
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| BrowserError::protocol("DevTools", format!("target without id: {target}")))
}

impl BrowserUnderTest for Chromium {
    fn kind(&self) -> BrowserKind {
        BrowserKind::Chromium
    }

    fn open_tab(&mut self, url: &str) -> Result<TabHandle, BrowserError> {
        Ok(TabHandle::new(target_id(&self.new_target(url)?)?))
    }

    fn close_tab(&mut self, tab: TabHandle) -> Result<(), BrowserError> {
        self.close_target(tab.as_str())
    }

    fn open_extension_page(&mut self, page: ExtensionPage) -> Result<String, BrowserError> {
        let url = format!(
            "chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/{}",
            page.file
        );
        let target = self.new_target(&url)?;
        let id = target_id(&target)?;
        let socket_url = target
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserError::protocol("DevTools", format!("no socket url: {target}"))
            })?;
        let mut connection = CdpConnection::connect(socket_url)?;
        let text = settle_page_text(&url, page.expect_text.as_deref(), || connection.body_text());
        // Вкладку закрываем в любом случае: страница расширения держит порт к фону.
        let closed = self.close_target(&id);
        let text = text?;
        closed?;
        Ok(text)
    }

    fn hand_off_tab(
        &mut self,
        stand_tab: &TabHandle,
        page_url_prefix: &str,
    ) -> Result<HandOffOutcome, BrowserError> {
        let url = format!(
            "chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/{HAND_OFF_EXTENSION_PAGE}"
        );
        let target = self.new_target(&url)?;
        let id = target_id(&target)?;
        let outcome = self.run_robot_in(&target, page_url_prefix);
        // Служебную страницу закрываем в любом случае и возвращаем вкладке стенда фокус.
        let closed = self.close_target(&id);
        let activated = http_request(
            self.port,
            "GET",
            &format!("/json/activate/{}", stand_tab.as_str()),
        )
        .map(|_| ());
        let outcome = outcome?;
        closed?;
        activated?;
        Ok(outcome)
    }

    fn shutdown(mut self: Box<Self>) -> Result<BrowserExit, BrowserError> {
        // Просим браузер закрыться сам: `Browser.close` на браузерной цели.
        let asked = http_json(self.port, "GET", "/json/version")
            .ok()
            .and_then(|version| {
                version
                    .get("webSocketDebuggerUrl")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .and_then(|url| CdpConnection::connect(&url).ok())
            .and_then(|mut connection| connection.call("Browser.close", Value::Null).ok());
        let graceful_status = if asked.is_some() {
            self.process.wait_timeout(GRACEFUL_EXIT_WAIT).ok().flatten()
        } else {
            None
        };
        let graceful = graceful_status.is_some();
        // Дождаться ухода посредника (браузер закрыл его stdin) и добить остатки группы.
        let _group_gone = self.process.wait_group_gone(GROUP_GONE_WAIT);
        let status = graceful_status.or_else(|| self.process.terminate(TERMINATE_GRACE));
        Ok(BrowserExit {
            exit_code: status.and_then(|status| status.code()),
            graceful,
            stderr_log: self.stderr_log.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ampersands_and_equals_survive_the_devtools_query() {
        let encoded = percent_encode_query("http://127.0.0.1:1/p.html?run=a&mode=handoff");
        assert_eq!(
            encoded,
            "http://127.0.0.1:1/p.html%3Frun%3Da%26mode%3Dhandoff"
        );
        assert!(!encoded.contains('&'));
    }

    fn arguments_as_text(options: &LaunchOptions, virtual_display: bool) -> Vec<String> {
        command_line(options, virtual_display)
            .expect("valid options")
            .into_iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn command_line_lists_extensions_with_commas_and_enables_autoplay() {
        let options = LaunchOptions {
            extensions: vec![
                ExtensionSource::UnpackedDir("/ext/a".into()),
                ExtensionSource::UnpackedDir("/ext/b".into()),
            ],
            autoplay_without_gesture: true,
        };
        let arguments = arguments_as_text(&options, true);
        assert!(arguments.contains(&"--load-extension=/ext/a,/ext/b".to_owned()));
        assert!(arguments.contains(&"--autoplay-policy=no-user-gesture-required".to_owned()));
        assert!(arguments.contains(&"--ozone-platform=wayland".to_owned()));
        assert!(arguments.contains(&"--remote-debugging-port=0".to_owned()));
        assert!(!arguments.iter().any(|a| a.starts_with("--user-data-dir")));
    }

    #[test]
    fn command_line_without_options_is_minimal() {
        let arguments = arguments_as_text(&LaunchOptions::default(), false);
        assert!(!arguments.iter().any(|a| a.starts_with("--load-extension")));
        assert!(!arguments.iter().any(|a| a.starts_with("--autoplay-policy")));
        assert!(!arguments.contains(&"--ozone-platform=wayland".to_owned()));
    }

    #[test]
    fn packed_extension_file_is_refused_for_chromium() {
        let options = LaunchOptions {
            extensions: vec![ExtensionSource::PackedFile("/ext/adblock.zip".into())],
            autoplay_without_gesture: false,
        };
        let error = command_line(&options, false).expect_err("packed files are unsupported");
        assert!(matches!(error, BrowserError::UnsupportedExtension(_)));
    }
}
