//! Один прогон «страница стенда в режиме передачи»: настоящий экстеншен, настоящий посредник,
//! настоящий плеер. Общая часть сценариев `capture`, `smoke` и `player-commands`.
//!
//! Что делает: ставит посредника, запускает браузер с `extensions/browser/dist/<браузер>`,
//! открывает страницу с `mode=handoff`, ждёт её `handoff-ready`, «отдаёт вкладку» роботом
//! (решение 175) и дальше даёт сценарию команды странице и ожидания по ней.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::context::{ScenarioError, Stand};
use super::pages::page_url;
use crate::browsers::{
    BrowserError, BrowserKind, BrowserUnderTest, ExtensionSource, HandOffOutcome, TabHandle,
};
use crate::environment::BenchEnvironment;
use crate::observe::PlayerLog;
use crate::site::{PageReport, RunId, SiteError};

/// Сколько ждать `handoff-ready`: загрузка страницы, её собственные проверки «играет»
/// (сторож в `mse-stall-reload` пересоздаёт MediaSource до 15 с).
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// Шаг проверки «страница уже прислала отчёт, не дойдя до готовности».
const READY_POLL: Duration = Duration::from_millis(300);
/// Сколько ждать подтверждение команды страницей (`ack <команда>`).
pub(super) const ACK_TIMEOUT: Duration = Duration::from_secs(45);
/// Сколько ждать итоговый отчёт после `finish`.
pub(super) const FINAL_REPORT_TIMEOUT: Duration = Duration::from_secs(30);
/// Метка в журнале страницы: страница сыграла свои проверки и ждёт команд робота.
const READY_MARKER: &str = "handoff-ready";
/// Параметр адреса: страница стенда работает под командой робота.
const HANDOFF_MODE_QUERY: &str = "&mode=handoff";

/// Каталог сборки настоящего экстеншена для браузера.
pub(super) fn real_extension_dir(stand: &Stand, browser: BrowserKind) -> PathBuf {
    let name = match browser {
        BrowserKind::Chromium => "chrome",
        BrowserKind::Firefox => "firefox",
    };
    stand
        .settings
        .repo_root
        .join("extensions/browser/dist")
        .join(name)
}

/// Какой экстеншен стоит в браузере прогона.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExtensionMode {
    /// Настоящий экстеншен из `extensions/browser/dist` и посредник в профиле.
    Real,
    /// Никакого: контрольный прогон «браузер играет сам» (проверка самой линейки).
    ControlWithoutExtension,
}

/// Чем кончилось ожидание готовности страницы.
pub(super) enum PageReadiness {
    /// Страница сыграла и ждёт команд.
    Ready,
    /// Страница отчиталась, не дойдя до готовности (например, `unsupported` или `fail`).
    ReportedEarly(PageReport),
    /// Ни готовности, ни отчёта.
    TimedOut,
}

/// Запущенный прогон одной страницы.
pub(super) struct HandoffRun<'a> {
    stand: &'a Stand,
    pub(super) browser: Box<dyn BrowserUnderTest>,
    tab: TabHandle,
    pub(super) run: RunId,
    page_id: &'static str,
    pub(super) player_log: PlayerLog,
    pub(super) stderr_log: PathBuf,
}

impl<'a> HandoffRun<'a> {
    /// Ставит посредника, запускает браузер с настоящим экстеншеном и открывает страницу.
    pub(super) fn open(
        stand: &'a Stand,
        environment: &BenchEnvironment,
        browser_kind: BrowserKind,
        page_id: &'static str,
        run_prefix: &str,
        extension: ExtensionMode,
    ) -> Result<Self, ScenarioError> {
        let extensions = match extension {
            ExtensionMode::Real => {
                let dist = real_extension_dir(stand, browser_kind);
                if !dist.join("manifest.json").is_file() {
                    return Err(ScenarioError::RealExtensionNotBuilt { path: dist });
                }
                // Установщик ДО запуска браузера: манифест посредника должен лежать в профиле.
                environment.connect_browser(browser_kind)?;
                vec![ExtensionSource::UnpackedDir(dist)]
            }
            ExtensionMode::ControlWithoutExtension => Vec::new(),
        };
        // Журнал открываем до старта: плеера ещё нет, его разбудит посредник.
        let player_log = PlayerLog::open(environment.player_log());
        let mut browser = stand.launch_browser(browser_kind, environment, extensions)?;
        let run = RunId::new_unique(run_prefix);
        let mut url = page_url(
            &stand.site.main_origin(),
            &stand.site.other_origin(),
            page_id,
            &run,
        );
        url.push_str(HANDOFF_MODE_QUERY);
        let tab = browser.open_tab(&url)?;
        Ok(Self {
            stand,
            browser,
            tab,
            run,
            page_id,
            player_log,
            stderr_log: browser_kind.stderr_log_path(environment.work_dir()),
        })
    }

    /// Ждёт, пока страница будет готова к передаче (или отчитается раньше).
    pub(super) fn wait_page_ready(&self) -> PageReadiness {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if let Ok(report) = self.stand.site.wait_report(&self.run, Duration::ZERO) {
                return PageReadiness::ReportedEarly(report);
            }
            if self
                .stand
                .site
                .wait_log_line(&self.run, READY_MARKER, READY_POLL)
                .is_ok()
            {
                return PageReadiness::Ready;
            }
            if Instant::now() >= deadline {
                return PageReadiness::TimedOut;
            }
        }
    }

    /// «Отдать вкладку»: служебная страница экстеншена шлёт фону `hand_off_tab`.
    pub(super) fn hand_off(&mut self) -> Result<HandOffOutcome, BrowserError> {
        let prefix = format!(
            "{}/site/pages/{}.html",
            self.stand.site.main_origin(),
            self.page_id
        );
        self.browser.hand_off_tab(&self.tab, &prefix)
    }

    /// Команда странице (она заберёт её опросом).
    pub(super) fn tell_page(&self, command: &str) {
        self.stand.site.send_command(&self.run, command);
    }

    /// Ждёт, пока страница подтвердит команду (`ack <команда>` в её журнале).
    pub(super) fn wait_ack(&self, command: &str, timeout: Duration) -> Result<(), String> {
        self.stand
            .site
            .wait_log_line(&self.run, &format!("ack {command}"), timeout)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Ждёт строку журнала страницы с `needle` и возвращает её целиком.
    pub(super) fn wait_page_log_line(
        &self,
        needle: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        self.stand
            .site
            .wait_log_line(&self.run, needle, timeout)
            .map_err(|error| error.to_string())
    }

    /// Просит страницу подвести итог и ждёт её отчёт.
    pub(super) fn finish_page(&self) -> Result<PageReport, SiteError> {
        self.tell_page("finish");
        self.stand.site.wait_report(&self.run, FINAL_REPORT_TIMEOUT)
    }

    /// Строки журнала страницы за прогон (расходуются).
    pub(super) fn take_page_log(&self) -> Vec<crate::site::BrowserLogLine> {
        self.stand.site.take_logs(&self.run)
    }
}
