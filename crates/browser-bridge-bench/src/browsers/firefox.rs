//! Firefox под управлением Marionette.
//!
//! Профиль создаём сами в каталоге прогона (`--profile`), настройки пишем в `user.js`.
//! Постоянный UUID для нашего gecko-id (`extensions.webextensions.uuids`) даёт известный
//! адрес `moz-extension://<uuid>/popup.html`. Файл подключения посредника Firefox ищет
//! только в `$HOME/.mozilla/native-messaging-hosts/` — а HOME у нас временный.

use std::ffi::OsString;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use browser_bridge_protocol::extension_identity::FIREFOX_EXTENSION_ID;
use serde_json::{Value, json};

use super::hand_off::{
    HAND_OFF_EXTENSION_PAGE, HandOffError, HandOffOutcome, ROBOT_FUNCTION, parse_robot_output,
};
use super::marionette::Marionette;
use super::support::{
    EXTENSION_PAGE_TIMEOUT, GRACEFUL_EXIT_WAIT, GROUP_GONE_WAIT, POLL_STEP, TERMINATE_GRACE,
    settle_page_text, spawn_browser,
};
use super::{
    BrowserError, BrowserExit, BrowserKind, BrowserUnderTest, ExtensionPage, ExtensionSource,
    LaunchOptions, TabHandle,
};
use crate::environment::process::ManagedProcess;
use crate::environment::{BenchEnvironment, DisplayTarget};

/// Постоянный UUID экстеншена: известный `moz-extension://` без чтения профиля.
const EXTENSION_UUID: &str = "7f0c6e10-5a3b-4c1e-9d2f-0b6a8e4d1c33";
/// Сколько ждать Marionette после старта Firefox (первый запуск профиля долгий).
const MARIONETTE_WAIT: Duration = Duration::from_secs(60);

/// Потолок асинхронного скрипта Marionette: фон отвечает на `hand_off_tab` только после
/// того, как поднял связь с плеером.
const HAND_OFF_SCRIPT_TIMEOUT_MS: u64 = 60_000;

/// Запущенный Firefox.
pub(crate) struct Firefox {
    process: ManagedProcess,
    marionette: Marionette,
    stderr_log: PathBuf,
}

/// Звук Firefox в стенде: «звукового устройства нет», явно и одинаково на любой машине.
///
/// Зачем. Стенд прячет звуковой сервер хозяина (частный `XDG_RUNTIME_DIR`, сокета
/// PipeWire/PulseAudio не видно) — звук браузера стенд не проверяет. Без этой настройки
/// cubeb Firefox, не достучавшись до PulseAudio, сам откатывается на ALSA, а «default» у ALSA
/// ведёт в модуль PipeWire, которому подключаться некуда. Обычно открытие потока тут же
/// падает, и страница играет видео по системным часам. Но изредка (≈ раз на 30–50 прогонов
/// `pages`) открытие потока не возвращается, и звуковой сервер AudioIPC родительского
/// процесса — он один на весь браузер — больше не отвечает никому. С этого момента ЛЮБОЙ
/// элемент со звуком в этом Firefox стоит на нуле (`totalVideoFrames 0 -> 0`,
/// `paused=false`), а страницы без звука (`drm-clearkey`, `banner-autoplay`) играют.
///
/// `media.cubeb.force_null_context` — штатный pref Firefox: каждый процесс сразу получает
/// «контекста cubeb нет», ни один звуковой поток не открывается (ни через сервер AudioIPC,
/// ни через ALSA), и видео играет по системным часам — ровно то, чем раньше заканчивался
/// каждый удачный прогон, только без лотереи.
const NO_AUDIO_DEVICE_PREF: &str = "user_pref(\"media.cubeb.force_null_context\", true);";

/// Свободный локальный порт для Marionette (узкое окно гонки между закрытием и запуском).
fn free_local_port() -> Result<u16, BrowserError> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|source| BrowserError::io("find a free port for Marionette", source))?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|source| BrowserError::io("read the free port", source))
}

/// Текст `user.js` профиля (чистая функция: её проверяют тесты).
pub(crate) fn user_js(marionette_port: u16, autoplay_without_gesture: bool) -> String {
    let uuid_map = json!({ FIREFOX_EXTENSION_ID: EXTENSION_UUID }).to_string();
    // Значение pref — строка JS; JSON-строка является корректным литералом JS.
    let uuid_pref = Value::String(uuid_map).to_string();
    let mut lines = vec![
        format!("user_pref(\"marionette.port\", {marionette_port});"),
        format!("user_pref(\"extensions.webextensions.uuids\", {uuid_pref});"),
        // Экстеншены без подписи (временные установки и распакованные папки).
        "user_pref(\"xpinstall.signatures.required\", false);".to_owned(),
        "user_pref(\"extensions.autoDisableScopes\", 0);".to_owned(),
        // Первый запуск, обновления, телеметрия, «браузер по умолчанию».
        "user_pref(\"browser.shell.checkDefaultBrowser\", false);".to_owned(),
        "user_pref(\"browser.aboutwelcome.enabled\", false);".to_owned(),
        "user_pref(\"browser.startup.homepage_override.mstone\", \"ignore\");".to_owned(),
        "user_pref(\"startup.homepage_welcome_url\", \"\");".to_owned(),
        "user_pref(\"browser.newtabpage.enabled\", false);".to_owned(),
        "user_pref(\"app.update.auto\", false);".to_owned(),
        "user_pref(\"app.update.enabled\", false);".to_owned(),
        "user_pref(\"datareporting.policy.dataSubmissionEnabled\", false);".to_owned(),
        "user_pref(\"datareporting.healthreport.uploadEnabled\", false);".to_owned(),
        "user_pref(\"toolkit.telemetry.reportingpolicy.firstRun\", false);".to_owned(),
        "user_pref(\"browser.sessionstore.resume_from_crash\", false);".to_owned(),
        "user_pref(\"browser.tabs.warnOnClose\", false);".to_owned(),
        // Консоль браузера дублируется в stderr процесса: именно туда Firefox пишет stderr
        // посредника native messaging («stderr output from native app …»).
        "user_pref(\"devtools.console.stdout.chrome\", true);".to_owned(),
        // `dump()` в chrome-контексте пишет в stdout процесса (см. `FORWARD_HOST_STDERR_SCRIPT`).
        "user_pref(\"browser.dom.window.dump.enabled\", true);".to_owned(),
        "user_pref(\"devtools.console.stdout.content\", true);".to_owned(),
        NO_AUDIO_DEVICE_PREF.to_owned(),
    ];
    if autoplay_without_gesture {
        // 0 = разрешить автовоспроизведение со звуком без жеста.
        lines.push("user_pref(\"media.autoplay.default\", 0);".to_owned());
        lines.push("user_pref(\"media.autoplay.blocking_policy\", 0);".to_owned());
    }
    lines.join("\n") + "\n"
}

/// Скрипт chrome-контекста: пересылает stderr посредника в stdout Firefox.
///
/// Firefox читает stderr посредника и кладёт строки в консольный сервис как
/// `stderr output from native app <имя>: <строка>`, но в stdout процесса их не печатает.
/// Слушатель консольного сервиса печатает их через `dump()`, и они попадают в
/// `firefox.stderr.log` рядом со строками Chromium-стиля («caller accepted» и т. д.).
const FORWARD_HOST_STDERR_SCRIPT: &str = r#"
const listener = {
  observe(message) {
    if (message.message.includes("stderr output from native app")) {
      dump(message.message + "\n");
    }
  },
};
Services.console.registerListener(listener);
// Ссылка на слушателя в окне не даёт сборщику мусора его убрать.
window.benchHostStderrListener = listener;
return true;
"#;

/// Запускает Firefox, подключает Marionette и ставит расширения как временные.
pub(crate) fn launch(
    environment: &BenchEnvironment,
    options: &LaunchOptions,
    executable: &Path,
) -> Result<Firefox, BrowserError> {
    let profile = environment.work_dir().join("firefox-profile");
    std::fs::create_dir_all(&profile)
        .map_err(|source| BrowserError::io(format!("create {}", profile.display()), source))?;
    let marionette_port = free_local_port()?;
    std::fs::write(
        profile.join("user.js"),
        user_js(marionette_port, options.autoplay_without_gesture),
    )
    .map_err(|source| BrowserError::io("write user.js", source))?;

    let arguments: Vec<OsString> = vec![
        "--marionette".into(),
        "--remote-allow-system-access".into(),
        "--no-remote".into(),
        "--profile".into(),
        profile.into(),
        "about:blank".into(),
    ];
    let mut env = environment.browser_command_env();
    env.push(("MOZ_CRASHREPORTER_DISABLE".into(), "1".into()));
    if environment.display_target() == DisplayTarget::Virtual {
        env.push(("MOZ_ENABLE_WAYLAND".into(), "1".into()));
    }
    let stderr_log = BrowserKind::Firefox.stderr_log_path(environment.work_dir());
    let process = spawn_browser(
        executable,
        &arguments,
        &env,
        &environment.browser_command_env_removed(),
        &stderr_log,
    )?;
    let mut marionette = Marionette::connect(marionette_port, MARIONETTE_WAIT)?;
    marionette.command(
        "WebDriver:NewSession",
        // `eager`: переход возвращается после DOMContentLoaded, не ждёт все ресурсы страницы.
        json!({ "capabilities": { "alwaysMatch": { "pageLoadStrategy": "eager" } } }),
    )?;
    forward_host_stderr(&mut marionette)?;
    for source in &options.extensions {
        let path = match source {
            ExtensionSource::UnpackedDir(path) | ExtensionSource::PackedFile(path) => path,
        };
        let absolute = std::fs::canonicalize(path).map_err(|error| {
            BrowserError::io(format!("resolve extension path {}", path.display()), error)
        })?;
        marionette.command(
            "Addon:Install",
            json!({ "path": absolute.to_string_lossy(), "temporary": true }),
        )?;
    }
    Ok(Firefox {
        process,
        marionette,
        stderr_log,
    })
}

/// Ставит слушатель консоли в chrome-контексте и возвращает контекст «content».
fn forward_host_stderr(marionette: &mut Marionette) -> Result<(), BrowserError> {
    marionette.command("Marionette:SetContext", json!({ "value": "chrome" }))?;
    let installed = marionette.command(
        "WebDriver:ExecuteScript",
        json!({ "script": FORWARD_HOST_STDERR_SCRIPT, "args": [] }),
    );
    // Контекст возвращаем в любом случае: дальше все скрипты — страничные.
    let restored = marionette.command("Marionette:SetContext", json!({ "value": "content" }));
    installed?;
    restored.map(|_| ())
}

impl Firefox {
    fn switch_to(&mut self, handle: &str) -> Result<(), BrowserError> {
        self.marionette
            .command("WebDriver:SwitchToWindow", json!({ "handle": handle }))
            .map(|_| ())
    }

    /// Новая вкладка, переход на `url`; возвращает ручку вкладки.
    fn open(&mut self, url: &str) -> Result<String, BrowserError> {
        let window = self.marionette.command(
            "WebDriver:NewWindow",
            json!({ "type": "tab", "focus": true }),
        )?;
        let handle = window
            .get("handle")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                BrowserError::protocol("Marionette", format!("NewWindow reply: {window}"))
            })?;
        self.switch_to(&handle)?;
        self.marionette
            .command("WebDriver:Navigate", json!({ "url": url }))?;
        Ok(handle)
    }

    /// Выполняет скрипт робота в странице экстеншена (контекст content). API экстеншена в
    /// такой странице лежит на самом окне; песочница Marionette видит его через
    /// `wrappedJSObject`. Пока страница грузится, повторяем попытку.
    fn run_robot(&mut self, page_url_prefix: &str) -> Result<HandOffOutcome, BrowserError> {
        self.marionette.command(
            "WebDriver:SetTimeouts",
            json!({ "script": HAND_OFF_SCRIPT_TIMEOUT_MS }),
        )?;
        let script = format!(
            "const done = arguments[arguments.length - 1];\n\
             const raw = window.wrappedJSObject || window;\n\
             const api = raw.browser || raw.chrome;\n\
             ({ROBOT_FUNCTION})(api, arguments[0]).then(done, (error) => done(JSON.stringify({{ robot: \"no_reply\", error: String(error) }})));"
        );
        let deadline = Instant::now() + EXTENSION_PAGE_TIMEOUT;
        loop {
            let attempt = self
                .marionette
                .command(
                    "WebDriver:ExecuteAsyncScript",
                    json!({ "script": script, "args": [page_url_prefix] }),
                )
                .and_then(|reply| {
                    reply
                        .get("value")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            BrowserError::protocol(
                                "Marionette",
                                format!("ExecuteAsyncScript reply: {reply}"),
                            )
                        })
                })
                .and_then(|raw| Ok(parse_robot_output(&raw, page_url_prefix)?));
            match attempt {
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
    /// (скрипт падает), повторяет попытку до таймаута.
    fn run_function_in_page(&mut self, async_function: &str) -> Result<String, BrowserError> {
        self.marionette.command(
            "WebDriver:SetTimeouts",
            json!({ "script": HAND_OFF_SCRIPT_TIMEOUT_MS }),
        )?;
        let script = format!(
            "const done = arguments[arguments.length - 1];\n\
             const raw = window.wrappedJSObject || window;\n\
             const api = raw.browser || raw.chrome;\n\
             ({async_function})(api).then(done, (error) => done('ERROR: ' + String(error)));"
        );
        let deadline = Instant::now() + EXTENSION_PAGE_TIMEOUT;
        loop {
            let attempt = self
                .marionette
                .command(
                    "WebDriver:ExecuteAsyncScript",
                    json!({ "script": script, "args": [] }),
                )
                .and_then(|reply| {
                    reply
                        .get("value")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            BrowserError::protocol(
                                "Marionette",
                                format!("ExecuteAsyncScript reply: {reply}"),
                            )
                        })
                });
            match attempt {
                Err(BrowserError::CommandFailed { .. }) if Instant::now() < deadline => {
                    std::thread::sleep(POLL_STEP);
                }
                other => return other,
            }
        }
    }

    fn body_text(&mut self) -> Result<String, BrowserError> {
        let reply = self.marionette.command(
            "WebDriver:ExecuteScript",
            json!({ "script": "return document.body ? document.body.innerText : '';", "args": [] }),
        )?;
        reply
            .get("value")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                BrowserError::protocol("Marionette", format!("ExecuteScript reply: {reply}"))
            })
    }
}

/// Первая ручка из ответа `CloseWindow` (массив строк).
fn first_window_handle(handles: &Value) -> Option<String> {
    handles.as_array()?.first()?.as_str().map(str::to_owned)
}

impl BrowserUnderTest for Firefox {
    fn kind(&self) -> BrowserKind {
        BrowserKind::Firefox
    }

    fn open_tab(&mut self, url: &str) -> Result<TabHandle, BrowserError> {
        Ok(TabHandle::new(self.open(url)?))
    }

    fn close_tab(&mut self, tab: TabHandle) -> Result<(), BrowserError> {
        self.switch_to(tab.as_str())?;
        // CloseWindow возвращает список оставшихся ручек. Закрытая вкладка остаётся «текущей»
        // и мертва: следующий NewWindow падал с "Browsing context has been discarded". Поэтому
        // сразу переключаемся на оставшуюся вкладку.
        let remaining = self
            .marionette
            .command("WebDriver:CloseWindow", json!({}))?;
        match first_window_handle(&remaining) {
            Some(handle) => self.switch_to(&handle),
            // Закрыли последнюю вкладку: переключаться некуда, браузер сам завершается.
            None => Ok(()),
        }
    }

    fn open_extension_page(&mut self, page: ExtensionPage) -> Result<String, BrowserError> {
        let url = format!("moz-extension://{EXTENSION_UUID}/{}", page.file);
        let handle = self.open(&url)?;
        let text = settle_page_text(&url, page.expect_text.as_deref(), || self.body_text());
        let closed = self.close_tab(TabHandle::new(handle));
        let text = text?;
        closed?;
        Ok(text)
    }

    fn hand_off_tab(
        &mut self,
        stand_tab: &TabHandle,
        page_url_prefix: &str,
    ) -> Result<HandOffOutcome, BrowserError> {
        let url = format!("moz-extension://{EXTENSION_UUID}/{HAND_OFF_EXTENSION_PAGE}");
        let handle = self.open(&url)?;
        let outcome = self.run_robot(page_url_prefix);
        // Служебную страницу закрываем в любом случае; затем возвращаемся на вкладку стенда
        // (close_tab выбрал «первую оставшуюся», а нужна именно вкладка стенда).
        let closed = self.close_tab(TabHandle::new(handle));
        let returned = self.switch_to(stand_tab.as_str());
        let outcome = outcome?;
        closed?;
        returned?;
        Ok(outcome)
    }

    fn run_in_extension_page(&mut self, async_function: &str) -> Result<String, BrowserError> {
        let url = format!("moz-extension://{EXTENSION_UUID}/{HAND_OFF_EXTENSION_PAGE}");
        let handle = self.open(&url)?;
        let result = self.run_function_in_page(async_function);
        // Служебную страницу закрываем в любом случае.
        let closed = self.close_tab(TabHandle::new(handle));
        let result = result?;
        closed?;
        Ok(result)
    }

    fn shutdown(mut self: Box<Self>) -> Result<BrowserExit, BrowserError> {
        // Quit закрывает сам Firefox; ответ может не прийти, если он уже выходит.
        let asked = self.marionette.command("Marionette:Quit", json!({}));
        let graceful_status = self.process.wait_timeout(GRACEFUL_EXIT_WAIT).ok().flatten();
        let graceful = asked.is_ok() || graceful_status.is_some();
        let _group_gone = self.process.wait_group_gone(GROUP_GONE_WAIT);
        let status = graceful_status.or_else(|| self.process.terminate(TERMINATE_GRACE));
        Ok(BrowserExit {
            exit_code: status.and_then(|status| status.code()),
            graceful: graceful && status.is_some(),
            stderr_log: self.stderr_log.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_remaining_handle_is_taken_from_the_close_window_reply() {
        assert_eq!(
            first_window_handle(&json!(["tab-1", "tab-2"])).as_deref(),
            Some("tab-1")
        );
        assert_eq!(first_window_handle(&json!([])), None);
        assert_eq!(first_window_handle(&json!({"unexpected": true})), None);
    }

    #[test]
    fn user_js_pins_the_extension_uuid_and_marionette_port() {
        let text = user_js(4242, true);
        assert!(text.contains("user_pref(\"marionette.port\", 4242);"));
        // Значение pref — JSON-строка с экранированными кавычками: проверяем её целиком.
        let expected_map = format!(
            "user_pref(\"extensions.webextensions.uuids\", \"{{\\\"{FIREFOX_EXTENSION_ID}\\\":\\\"{EXTENSION_UUID}\\\"}}\");"
        );
        assert!(text.contains(&expected_map), "{text}");
        assert!(text.contains("media.autoplay.default\", 0"));
    }

    #[test]
    fn user_js_gives_firefox_no_audio_device_in_every_profile() {
        // Без этой строки Firefox сам выбирает звук хозяина (ALSA → PipeWire) и изредка
        // навсегда вешает весь свой звук — см. `NO_AUDIO_DEVICE_PREF`.
        for autoplay_without_gesture in [true, false] {
            let text = user_js(1, autoplay_without_gesture);
            assert!(
                text.lines()
                    .any(|line| line == "user_pref(\"media.cubeb.force_null_context\", true);"),
                "{text}"
            );
        }
    }

    #[test]
    fn user_js_keeps_autoplay_blocking_when_not_asked() {
        assert!(!user_js(1, false).contains("media.autoplay"));
    }
}
