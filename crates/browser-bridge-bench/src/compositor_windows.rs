//! Управление окнами невидимого kwin: вернуть браузер на передний план.
//!
//! Зачем. Окно плеера, открытое поверх браузера, забирает фокус; следующий клик робота по
//! попапу тогда ушёл бы не туда. Человек просто кликает по окну браузера; роботу нужен
//! способ сделать то же самое: скрипт KWin (`org.kde.kwin.Scripting`) назначает активным
//! окно браузера. У стенда шина kwin СВОЯ и не совпадает с частной шиной плеера: адрес
//! берётся из [`DesktopEndpoint`].

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::browsers::BrowserKind;
use crate::environment::{BenchEnvironment, DesktopEndpoint, EnvironmentError};

/// Сколько ждать отчёта скрипта в журнале kwin.
const REPORT_WAIT: Duration = Duration::from_secs(5);
/// Метка строки отчёта в журнале kwin.
const REPORT_MARKER: &str = "FPBENCH-ACTIVE";

/// Ошибки управления окнами.
#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    /// Нет невидимого экрана с правом управления.
    #[error(transparent)]
    Environment(#[from] EnvironmentError),
    /// Не удалось записать скрипт или прочитать журнал.
    #[error("{action}: {path}: {source}")]
    Io {
        /// Что делали.
        action: &'static str,
        /// Файл.
        path: PathBuf,
        /// Ошибка ОС.
        source: std::io::Error,
    },
    /// `busctl` не запустился или kwin отказал в вызове.
    #[error("KWin scripting call {call} failed: {message}")]
    Scripting {
        /// Вызов.
        call: String,
        /// Текст ошибки.
        message: String,
    },
    /// Скрипт отработал, но активным окном стало не окно браузера (окна нет или оно не приняло фокус).
    #[error("browser window is not active after the request; active window class: {active_class}")]
    NotActivated {
        /// Класс активного окна по отчёту скрипта (`none` — активного нет).
        active_class: String,
    },
    /// Отчёт скрипта не появился в журнале kwin вовремя.
    #[error("KWin script printed no report within {0:?} (see kwin.log)")]
    NoReport(Duration),
}

/// Окна невидимого экрана.
pub struct CompositorWindows<'env> {
    environment: &'env BenchEnvironment,
    endpoint: DesktopEndpoint,
    next_script: AtomicU32,
}

impl<'env> CompositorWindows<'env> {
    /// Нужно окружение с `DesktopInputPolicy::Allowed`.
    pub fn new(environment: &'env BenchEnvironment) -> Result<Self, WindowError> {
        Ok(Self {
            endpoint: environment.desktop_endpoint()?,
            environment,
            next_script: AtomicU32::new(0),
        })
    }

    /// Класс (`resourceClass`, строчными буквами) активного окна; `none` — активного нет.
    /// Нужен роботу, чтобы убедиться, КТО сейчас принимает клики и клавиши.
    pub fn active_window_class(&self) -> Result<String, WindowError> {
        let number = self.next_script.fetch_add(1, Ordering::Relaxed);
        self.run_script(number, &report_script(number, ""))?;
        self.wait_for_report(number)
    }

    /// Делает окно браузера активным и поднимает его поверх остальных (в том числе поверх
    /// окна плеера). Возвращает успех только если kwin подтвердил: активное окно — браузер.
    pub fn bring_browser_forward(&self, browser: BrowserKind) -> Result<(), WindowError> {
        let number = self.next_script.fetch_add(1, Ordering::Relaxed);
        let class_fragment = match browser {
            BrowserKind::Chromium => "chromium",
            BrowserKind::Firefox => "firefox",
        };
        let activation = format!(
            r#"const windows = workspace.windowList();
for (let index = 0; index < windows.length; index++) {{
    if (String(windows[index].resourceClass).toLowerCase().indexOf("{class_fragment}") >= 0) {{
        workspace.activeWindow = windows[index];
        break;
    }}
}}
"#
        );
        self.run_script(number, &report_script(number, &activation))?;
        let active_class = self.wait_for_report(number)?;
        if active_class.contains(class_fragment) {
            Ok(())
        } else {
            Err(WindowError::NotActivated { active_class })
        }
    }

    /// Загружает и запускает скрипт KWin через его шину.
    fn run_script(&self, number: u32, source: &str) -> Result<(), WindowError> {
        let script = self
            .environment
            .work_dir()
            .join(format!("kwin-script-{number}.js"));
        std::fs::write(&script, source).map_err(|source| WindowError::Io {
            action: "write KWin script",
            path: script.clone(),
            source,
        })?;
        let plugin = format!("fastiplayer-bench-{number}");
        self.scripting_call(&["loadScript", "ss", &script.to_string_lossy(), &plugin])?;
        // `start` запускает загруженные, но ещё не запущенные скрипты.
        self.scripting_call(&["start"])
    }

    fn scripting_call(&self, arguments: &[&str]) -> Result<(), WindowError> {
        let output = Command::new("busctl")
            .arg(format!("--address={}", self.endpoint.kwin_bus_address()))
            .args([
                "call",
                "org.kde.KWin",
                "/Scripting",
                "org.kde.kwin.Scripting",
            ])
            .args(arguments)
            .output()
            .map_err(|error| WindowError::Scripting {
                call: arguments.join(" "),
                message: error.to_string(),
            })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(WindowError::Scripting {
                call: arguments.join(" "),
                message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
    }

    /// Ждёт строку отчёта скрипта `number` в журнале kwin и возвращает класс активного окна.
    fn wait_for_report(&self, number: u32) -> Result<String, WindowError> {
        let log_path = self.environment.work_dir().join("kwin.log");
        let marker = format!("{REPORT_MARKER}{number} ");
        let deadline = Instant::now() + REPORT_WAIT;
        loop {
            let log = std::fs::read_to_string(&log_path).map_err(|source| WindowError::Io {
                action: "read kwin log",
                path: log_path.clone(),
                source,
            })?;
            if let Some(line) = log.lines().find(|line| line.contains(&marker)) {
                return Ok(line
                    .split(&marker)
                    .nth(1)
                    .unwrap_or_default()
                    .trim()
                    .to_owned());
            }
            if Instant::now() >= deadline {
                return Err(WindowError::NoReport(REPORT_WAIT));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Текст скрипта KWin: выполняет `actions`, затем печатает класс активного окна с меткой `number`.
fn report_script(number: u32, actions: &str) -> String {
    format!(
        r#"{actions}const active = workspace.activeWindow;
print("{REPORT_MARKER}{number} " + (active ? String(active.resourceClass).toLowerCase() : "none"));
"#
    )
}
