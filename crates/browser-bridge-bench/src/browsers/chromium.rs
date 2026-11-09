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
    HAND_OFF_EXTENSION_PAGE, HandOffError, HandOffOptions, HandOffOutcome, ROBOT_FUNCTION,
    parse_robot_output,
};
use super::popup_probe::PROBE_FUNCTION;
use super::support::{
    EXTENSION_PAGE_TIMEOUT, GRACEFUL_EXIT_WAIT, GROUP_GONE_WAIT, POLL_STEP, TERMINATE_GRACE,
    settle_page_text, spawn_browser, wait_until,
};
use super::toolbar_pin;
use super::{
    BrowserError, BrowserExit, BrowserKind, BrowserUnderTest, ExtensionPage, ExtensionSource,
    LaunchOptions, PopupSnapshot, TabHandle, WindowPrivacy,
};
use crate::environment::BenchEnvironment;
use crate::environment::process::ManagedProcess;

/// Сколько ждать, пока Chromium запишет порт отладки.
const DEVTOOLS_PORT_WAIT: Duration = Duration::from_secs(40);
/// Сколько ждать ответ фона на `hand_off_tab`: он сначала поднимает связь с плеером
/// (будит его через посредника), и только потом отвечает.
const HAND_OFF_REPLY_WAIT: Duration = Duration::from_secs(60);
/// Страница попапа экстеншена.
const POPUP_PAGE: &str = "popup.html";
/// Сколько ждать ответ зонда в странице попапа.
const POPUP_PROBE_WAIT: Duration = Duration::from_secs(10);
/// Имя файла с портом отладки в каталоге профиля.
/// Сколько ждать цель приватной вкладки в списке целей.
const PRIVATE_TARGET_WAIT: Duration = Duration::from_secs(10);
const ACTIVE_PORT_FILE: &str = "DevToolsActivePort";

/// Запущенный Chromium.
pub(crate) struct Chromium {
    process: ManagedProcess,
    port: u16,
    stderr_log: PathBuf,
    /// Цель DevTools служебной страницы экстеншена (фоновая вкладка), если она открыта.
    service_target: Option<Value>,
    /// Окно прогона: в приватном вкладки стенда и страницы экстеншена открываются в окне инкогнито.
    window_privacy: WindowPrivacy,
}

/// Аргументы командной строки (отдельная чистая функция: её проверяют тесты).
pub(crate) fn command_line(
    options: &LaunchOptions,
    virtual_display: bool,
    desktop_input: bool,
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
    if desktop_input {
        // Всплывашка перевода («страница на английском») двигает панель и перехватывает клик.
        arguments.push("--disable-features=Translate".into());
    }
    if !options.loopback_host_aliases.is_empty() {
        // «Публичные» имена стенда ведут на этот же ПК: `MAP имя 127.0.0.1`, правила через запятую.
        arguments.push(host_resolver_rules(&options.loopback_host_aliases).into());
    }
    // Приватный прогон: экстеншен по `--load-extension` в приватные окна не пускают, разрешение
    // «работать в инкогнито» даёт только `Extensions.loadUnpacked` с `enableInIncognito` (см. `launch`),
    // а ему нужен тот же флаг отладки расширений, что и поздней установке. `--incognito` даёт окно
    // инкогнито со стартовой вкладкой; как открывать в нём остальные вкладки — см. `new_private_target`.
    let private_window = options.window_privacy == WindowPrivacy::Private;
    if private_window {
        arguments.push("--incognito".into());
    }
    if options.runtime_extension_install || private_window {
        // Без этого флага `Extensions.loadUnpacked` по CDP отказывает (10d: поздняя установка).
        arguments.push("--enable-unsafe-extension-debugging".into());
    }
    if !unpacked.is_empty() && !private_window {
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

/// Аргумент `--host-resolver-rules`: каждое имя разрешается в `127.0.0.1`.
fn host_resolver_rules(aliases: &[String]) -> String {
    let rules: Vec<String> = aliases
        .iter()
        .map(|alias| format!("MAP {alias} 127.0.0.1"))
        .collect();
    format!("--host-resolver-rules={}", rules.join(","))
}

/// Запускает Chromium и ждёт порт отладки.
pub(crate) fn launch(
    environment: &BenchEnvironment,
    options: &LaunchOptions,
    executable: &Path,
) -> Result<Chromium, BrowserError> {
    let virtual_display =
        environment.display_target() == crate::environment::DisplayTarget::Virtual;
    let desktop_input = environment.desktop_endpoint().is_ok();
    let arguments = command_line(options, virtual_display, desktop_input)?;
    let profile = environment.xdg_config_home().join("chromium");
    // Файл от прошлого запуска в этом же профиле обманул бы ожидание порта.
    let port_file = profile.join(ACTIVE_PORT_FILE);
    let _stale_port_file = std::fs::remove_file(&port_file);
    // Мышиным сценариям значок нужен на панели: закрепляем его в профиле стенда до запуска.
    if desktop_input {
        toolbar_pin::pin_extension(&profile, CHROMIUM_DEVELOPMENT_EXTENSION_ID)?;
    }
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
    let chromium = Chromium {
        process,
        port,
        stderr_log,
        service_target: None,
        window_privacy: options.window_privacy,
    };
    if options.window_privacy == WindowPrivacy::Private {
        // Экстеншены приватного прогона ставятся уже в запущенный браузер и сразу с разрешением
        // для окон инкогнито (в командной строке для этого флага нет).
        for source in &options.extensions {
            chromium.load_unpacked(source, IncognitoAccess::Allowed)?;
        }
    }
    Ok(chromium)
}

/// Пускать ли экстеншен в окна инкогнито при загрузке по CDP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IncognitoAccess {
    /// Как после `--load-extension`: только обычные окна.
    NotAllowed,
    /// Разрешение «работать в окне инкогнито» включено (`enableInIncognito`).
    Allowed,
}

impl Chromium {
    /// Открывает адрес в новой вкладке ПРИВАТНОГО окна (инкогнито) и возвращает описание цели DevTools.
    ///
    /// Что проверено на Chromium 156 (спайк 14b2). Окно инкогнито даёт флаг `--incognito` (в нём лежит
    /// первая вкладка запуска), но ни `PUT /json/new`, ни `Target.createTarget` вкладку в него не
    /// кладут: обе открывают ОБЫЧНОЕ окно, и экстеншен видит такую вкладку с `tab.incognito = false`.
    /// `Target.createBrowserContext` приватную среду создаёт, но экстеншен там заблокирован
    /// (`ERR_BLOCKED_BY_CLIENT`): разрешение «работать в инкогнито» (`enableInIncognito` при
    /// `Extensions.loadUnpacked`) относится только к окнам профиля с флагом `--incognito`.
    /// Поэтому вкладку в окно инкогнито открывает сам экстеншен: `tabs.create({ windowId })` из его
    /// страницы, открытой в обычном окне (экстеншен пущен в инкогнито, и API ему это позволяет).
    /// Цель DevTools новой вкладки находим по разнице списка целей до и после.
    fn new_private_target(&self, url: &str) -> Result<Value, BrowserError> {
        let known_ids = self.page_target_ids()?;
        let helper = http_json(
            self.port,
            "PUT",
            &format!(
                "/json/new?{}",
                percent_encode_query(&format!(
                    "chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/{HAND_OFF_EXTENSION_PAGE}"
                ))
            ),
        )?;
        let helper_id = target_id(&helper)?;
        let url_literal = Value::String(url.to_owned()).to_string();
        let create_tab = format!(
            "async (api) => {{ \
               const privateWindow = (await api.windows.getAll()).find((window) => window.incognito); \
               if (!privateWindow) throw new Error('no incognito window'); \
               const tab = await api.tabs.create({{ windowId: privateWindow.id, url: {url_literal}, active: true }}); \
               return String(tab.id); \
             }}"
        );
        let created = self.run_function_in(&helper, &create_tab);
        let closed = self.close_target(&helper_id);
        created?;
        closed?;
        wait_until(
            "private tab target in /json/list",
            PRIVATE_TARGET_WAIT,
            || {
                http_json(self.port, "GET", "/json/list")
                    .ok()?
                    .as_array()?
                    .iter()
                    .find(|target| {
                        target.get("type").and_then(Value::as_str) == Some("page")
                            && target.get("id").and_then(Value::as_str).is_some_and(|id| {
                                id != helper_id && !known_ids.iter().any(|known| known == id)
                            })
                    })
                    .cloned()
            },
        )
    }

    /// Идентификаторы всех открытых страниц-целей.
    fn page_target_ids(&self) -> Result<Vec<String>, BrowserError> {
        let targets = http_json(self.port, "GET", "/json/list")?;
        Ok(targets
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|target| target.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect())
    }

    /// Открывает адрес в новой вкладке и возвращает описание цели DevTools.
    fn new_target(&self, url: &str) -> Result<Value, BrowserError> {
        if self.window_privacy == WindowPrivacy::Private {
            return self.new_private_target(url);
        }
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
        options: HandOffOptions,
    ) -> Result<HandOffOutcome, BrowserError> {
        let socket_url = target
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserError::protocol("DevTools", format!("no socket url: {target}"))
            })?;
        let mut connection = CdpConnection::connect(socket_url)?;
        let prefix_literal = Value::String(page_url_prefix.to_owned()).to_string();
        let extra_fields = options.extra_message_fields();
        let expression =
            format!("({ROBOT_FUNCTION})(globalThis.chrome, {prefix_literal}, {extra_fields})");
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

    /// Выполняет `async (api) => string` в открытой странице экстеншена; пока страница грузится
    /// (выражение падает), повторяет попытку до таймаута.
    fn run_function_in(
        &self,
        target: &Value,
        async_function: &str,
    ) -> Result<String, BrowserError> {
        let socket_url = target
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserError::protocol("DevTools", format!("no socket url: {target}"))
            })?;
        let mut connection = CdpConnection::connect(socket_url)?;
        let expression = format!("({async_function})(globalThis.chrome)");
        let deadline = Instant::now() + EXTENSION_PAGE_TIMEOUT;
        loop {
            match connection.evaluate_string_promise(&expression, HAND_OFF_REPLY_WAIT) {
                Err(BrowserError::CommandFailed { .. }) if Instant::now() < deadline => {
                    std::thread::sleep(POLL_STEP);
                }
                other => return other,
            }
        }
    }

    /// `Extensions.loadUnpacked` по браузерной цели CDP: общая часть поздней установки (10d) и
    /// установки в приватный прогон (14b2).
    fn load_unpacked(
        &self,
        source: &ExtensionSource,
        incognito: IncognitoAccess,
    ) -> Result<(), BrowserError> {
        let ExtensionSource::UnpackedDir(directory) = source else {
            return Err(BrowserError::UnsupportedExtension(
                "Chromium installs at runtime only unpacked extension directories".to_owned(),
            ));
        };
        let absolute = std::fs::canonicalize(directory).map_err(|error| {
            BrowserError::io(
                format!("resolve extension path {}", directory.display()),
                error,
            )
        })?;
        let version = http_json(self.port, "GET", "/json/version")?;
        let socket_url = version
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserError::protocol("DevTools", format!("no browser socket url: {version}"))
            })?;
        let mut connection = CdpConnection::connect(socket_url)?;
        let reply = connection.call(
            "Extensions.loadUnpacked",
            serde_json::json!({
                "path": absolute.to_string_lossy(),
                "enableInIncognito": incognito == IncognitoAccess::Allowed,
            }),
        )?;
        // Идентификатор берётся из `key` манифеста: другой id означал бы, что значок на панели,
        // посредник и всё остальное стенда смотрят на чужой экстеншен.
        let loaded_id = reply.get("id").and_then(Value::as_str);
        if loaded_id != Some(CHROMIUM_DEVELOPMENT_EXTENSION_ID) {
            return Err(BrowserError::protocol(
                "DevTools",
                format!(
                    "Extensions.loadUnpacked gave id {loaded_id:?}, expected {CHROMIUM_DEVELOPMENT_EXTENSION_ID}"
                ),
            ));
        }
        Ok(())
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

    fn activate_tab(&mut self, tab: &TabHandle) -> Result<(), BrowserError> {
        http_request(
            self.port,
            "GET",
            &format!("/json/activate/{}", tab.as_str()),
        )
        .map(|_| ())
    }

    fn tab_title(&mut self, tab: &TabHandle) -> Result<String, BrowserError> {
        let targets = http_json(self.port, "GET", "/json/list")?;
        targets
            .as_array()
            .into_iter()
            .flatten()
            .find(|target| target.get("id").and_then(Value::as_str) == Some(tab.as_str()))
            .and_then(|target| target.get("title").and_then(Value::as_str))
            .map(str::to_owned)
            .ok_or_else(|| {
                BrowserError::protocol(
                    "DevTools",
                    format!("no target {} in /json/list", tab.as_str()),
                )
            })
    }

    fn popup_snapshot(&mut self) -> Result<Option<PopupSnapshot>, BrowserError> {
        let popup_prefix =
            format!("chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/{POPUP_PAGE}");
        let targets = http_json(self.port, "GET", "/json/list")?;
        let Some(socket_url) = targets
            .as_array()
            .into_iter()
            .flatten()
            .find(|target| {
                target
                    .get("url")
                    .and_then(Value::as_str)
                    .is_some_and(|url| url.starts_with(&popup_prefix))
            })
            .and_then(|target| target.get("webSocketDebuggerUrl").and_then(Value::as_str))
        else {
            return Ok(None);
        };
        let mut connection = CdpConnection::connect(socket_url)?;
        let raw = connection
            .evaluate_string_promise(&format!("({PROBE_FUNCTION})(document)"), POPUP_PROBE_WAIT)?;
        Ok(Some(PopupSnapshot::parse(&raw).map_err(|error| {
            BrowserError::protocol("DevTools", format!("popup probe answer {raw:?}: {error}"))
        })?))
    }

    fn open_popup_url(&mut self) -> Result<Option<String>, BrowserError> {
        let popup_prefix =
            format!("chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/{POPUP_PAGE}");
        let targets = http_json(self.port, "GET", "/json/list")?;
        Ok(targets
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|target| target.get("url").and_then(Value::as_str))
            .find(|url| url.starts_with(&popup_prefix))
            .map(str::to_owned))
    }

    fn hand_off_tab_with_options(
        &mut self,
        stand_tab: &TabHandle,
        page_url_prefix: &str,
        options: HandOffOptions,
    ) -> Result<HandOffOutcome, BrowserError> {
        let url = format!(
            "chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/{HAND_OFF_EXTENSION_PAGE}"
        );
        let target = self.new_target(&url)?;
        let id = target_id(&target)?;
        let outcome = self.run_robot_in(&target, page_url_prefix, options);
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

    fn open_service_page(&mut self, keep_active: &TabHandle) -> Result<(), BrowserError> {
        let url = format!(
            "chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/{HAND_OFF_EXTENSION_PAGE}"
        );
        let target = self.new_target(&url)?;
        // Новая вкладка стала активной: возвращаем активность вкладке стенда.
        self.activate_tab(keep_active)?;
        self.service_target = Some(target);
        Ok(())
    }

    fn run_in_service_page(&mut self, async_function: &str) -> Result<String, BrowserError> {
        let target = self.service_target.clone().ok_or_else(|| {
            BrowserError::protocol("DevTools", "the service page is not open".to_owned())
        })?;
        self.run_function_in(&target, async_function)
    }

    fn run_in_extension_page(&mut self, async_function: &str) -> Result<String, BrowserError> {
        let url = format!(
            "chrome-extension://{CHROMIUM_DEVELOPMENT_EXTENSION_ID}/{HAND_OFF_EXTENSION_PAGE}"
        );
        let target = self.new_target(&url)?;
        let id = target_id(&target)?;
        let result = self.run_function_in(&target, async_function);
        // Служебную страницу закрываем в любом случае.
        let closed = self.close_target(&id);
        let result = result?;
        closed?;
        Ok(result)
    }

    fn install_extension_at_runtime(
        &mut self,
        source: &ExtensionSource,
    ) -> Result<(), BrowserError> {
        self.load_unpacked(source, IncognitoAccess::NotAllowed)
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
        command_line(options, virtual_display, false)
            .expect("valid options")
            .into_iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn command_line_lists_extensions_with_commas_and_enables_autoplay() {
        let options = LaunchOptions {
            window_privacy: WindowPrivacy::Normal,
            extensions: vec![
                ExtensionSource::UnpackedDir("/ext/a".into()),
                ExtensionSource::UnpackedDir("/ext/b".into()),
            ],
            autoplay_without_gesture: true,
            loopback_host_aliases: Vec::new(),
            runtime_extension_install: false,
        };
        let arguments = arguments_as_text(&options, true);
        assert!(arguments.contains(&"--load-extension=/ext/a,/ext/b".to_owned()));
        assert!(arguments.contains(&"--autoplay-policy=no-user-gesture-required".to_owned()));
        assert!(arguments.contains(&"--ozone-platform=wayland".to_owned()));
        assert!(arguments.contains(&"--remote-debugging-port=0".to_owned()));
        assert!(!arguments.iter().any(|a| a.starts_with("--user-data-dir")));
    }

    #[test]
    fn loopback_aliases_become_one_host_resolver_rules_argument() {
        let options = LaunchOptions {
            loopback_host_aliases: vec!["public.bench.example".into(), "ad.example".into()],
            ..LaunchOptions::default()
        };
        let arguments = arguments_as_text(&options, false);
        assert!(arguments.contains(
            &"--host-resolver-rules=MAP public.bench.example 127.0.0.1,MAP ad.example 127.0.0.1"
                .to_owned()
        ));
        // Без псевдонимов правила не появляются.
        assert!(
            !arguments_as_text(&LaunchOptions::default(), false)
                .iter()
                .any(|a| a.starts_with("--host-resolver-rules"))
        );
    }

    #[test]
    fn the_late_install_mode_adds_the_extension_debugging_flag_only_when_asked() {
        let late = LaunchOptions {
            runtime_extension_install: true,
            ..LaunchOptions::default()
        };
        assert!(
            arguments_as_text(&late, false)
                .contains(&"--enable-unsafe-extension-debugging".to_owned())
        );
        assert!(
            !arguments_as_text(&LaunchOptions::default(), false)
                .contains(&"--enable-unsafe-extension-debugging".to_owned())
        );
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
            window_privacy: WindowPrivacy::Normal,
            extensions: vec![ExtensionSource::PackedFile("/ext/adblock.zip".into())],
            autoplay_without_gesture: false,
            loopback_host_aliases: Vec::new(),
            runtime_extension_install: false,
        };
        let error = command_line(&options, false, false).expect_err("packed files are unsupported");
        assert!(matches!(error, BrowserError::UnsupportedExtension(_)));
    }
}
