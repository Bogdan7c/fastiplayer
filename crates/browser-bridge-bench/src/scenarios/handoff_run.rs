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
use super::run_tweaks::{RunTweaks, RuntimeInstall};
use crate::browsers::{
    BrowserError, BrowserKind, BrowserUnderTest, ExtensionSource, HandOffOptions, HandOffOutcome,
    TabHandle,
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
pub(super) const HANDOFF_MODE_QUERY: &str = "&mode=handoff";

/// Адрес страницы стенда под заданным адресом сервера (`report_origin` остаётся настоящим
/// `127.0.0.1`: отчёты и журнал страницы идут на сервер стенда, а не на «публичное» имя).
fn page_url_under(stand: &Stand, page_origin: &str, page_id: &str, run: &RunId) -> String {
    page_url_with_report_origin(
        page_origin,
        &stand.site.main_origin(),
        &stand.site.other_origin(),
        page_id,
        run,
    )
}

/// Чистая часть [`page_url_under`]: страница под `page_origin`, отчёты на `report_origin`.
fn page_url_with_report_origin(
    page_origin: &str,
    report_origin: &str,
    other_origin: &str,
    page_id: &str,
    run: &RunId,
) -> String {
    let url = page_url(page_origin, other_origin, page_id, run);
    if page_origin == report_origin {
        return url;
    }
    url.replace(
        &format!("report_origin={page_origin}"),
        &format!("report_origin={report_origin}"),
    )
}

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
    /// Экстеншена при запуске нет, его ставит сама строка позже (10d: вкладка открыта раньше).
    /// Посредник в профиле уже лежит: установщик отрабатывает до запуска, как у `Real`.
    /// `RunTweaks::runtime_install` обязан быть задан: иначе браузер не примет позднюю установку.
    InstalledLater,
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

/// Страница, отложенная в своей вкладке, пока текущей стала другая (см. [`HandoffRun::park_and_open`]).
pub(super) struct ParkedPage {
    tab: TabHandle,
    run: RunId,
    page_id: &'static str,
}

/// Запущенный прогон одной страницы.
pub(super) struct HandoffRun<'a> {
    stand: &'a Stand,
    pub(super) browser: Box<dyn BrowserUnderTest>,
    tab: TabHandle,
    pub(super) run: RunId,
    page_id: &'static str,
    /// Адрес сервера стенда, под которым открыта страница (имя хоста может быть «публичным»).
    page_origin: String,
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
        Self::open_with_query(
            stand,
            environment,
            browser_kind,
            page_id,
            run_prefix,
            extension,
            HANDOFF_MODE_QUERY,
        )
    }

    /// То же, но с произвольным хвостом адреса (`&ключ=значение…`) вместо `mode=handoff`:
    /// страницы, у которых нет режима передачи под командой робота (повтор записи `replay`).
    pub(super) fn open_with_query(
        stand: &'a Stand,
        environment: &BenchEnvironment,
        browser_kind: BrowserKind,
        page_id: &'static str,
        run_prefix: &str,
        extension: ExtensionMode,
        query_suffix: &str,
    ) -> Result<Self, ScenarioError> {
        Self::open_tweaked(
            stand,
            environment,
            browser_kind,
            page_id,
            run_prefix,
            extension,
            query_suffix,
            &RunTweaks::default(),
        )
    }

    /// То же с отклонениями прогона (порядок установки экстеншенов, блокировщик, имя хоста
    /// страницы): матрица блокировщиков, защита локального адреса.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn open_tweaked(
        stand: &'a Stand,
        environment: &BenchEnvironment,
        browser_kind: BrowserKind,
        page_id: &'static str,
        run_prefix: &str,
        extension: ExtensionMode,
        query_suffix: &str,
        tweaks: &RunTweaks,
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
            ExtensionMode::InstalledLater => {
                environment.connect_browser(browser_kind)?;
                Vec::new()
            }
            ExtensionMode::ControlWithoutExtension => Vec::new(),
        };
        // Журнал открываем до старта: плеера ещё нет, его разбудит посредник.
        let player_log = PlayerLog::open(environment.player_log());
        let mut browser =
            stand.launch_browser_tweaked(browser_kind, environment, extensions, tweaks)?;
        // Строка «обновление»: установка уже стоит ДО страницы, обновят её позже.
        if let RuntimeInstall::BeforeThePage(directory) = &tweaks.runtime_install {
            browser
                .install_extension_at_runtime(&ExtensionSource::UnpackedDir(directory.clone()))?;
        }
        let run = RunId::new_unique(run_prefix);
        let page_origin = tweaks.page_origin(&stand.site.main_origin());
        let mut url = page_url_under(stand, &page_origin, page_id, &run);
        url.push_str(query_suffix);
        let tab = browser.open_tab(&url)?;
        Ok(Self {
            stand,
            browser,
            tab,
            run,
            page_id,
            page_origin,
            player_log,
            stderr_log: browser_kind.stderr_log_path(environment.work_dir()),
        })
    }

    /// Ждёт, пока страница будет готова к передаче (или отчитается раньше).
    pub(super) fn wait_page_ready(&self) -> PageReadiness {
        self.wait_page_marker(READY_MARKER, READY_TIMEOUT)
    }

    /// Ждёт отметку `marker` в журнале страницы; если страница раньше отчиталась — отчёт.
    /// `Ready` здесь значит «отметка появилась» (для `wait_page_ready` это `handoff-ready`).
    pub(super) fn wait_page_marker(&self, marker: &str, timeout: Duration) -> PageReadiness {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(report) = self.stand.site.wait_report(&self.run, Duration::ZERO) {
                return PageReadiness::ReportedEarly(report);
            }
            if self
                .stand
                .site
                .wait_log_line(&self.run, marker, READY_POLL)
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
        self.hand_off_with_options(HandOffOptions::default())
    }

    /// То же с согласием человека в сообщении (`consented_local_host`).
    pub(super) fn hand_off_with_options(
        &mut self,
        options: HandOffOptions,
    ) -> Result<HandOffOutcome, BrowserError> {
        let prefix = format!("{}/site/pages/{}.html", self.page_origin, self.page_id);
        self.browser
            .hand_off_tab_with_options(&self.tab, &prefix, options)
    }

    /// «Отдать вкладку» ровно ЭТОГО прогона: адрес ищется вместе с `?run=<прогон>`. Нужна, когда в
    /// браузере открыты две вкладки одной и той же страницы (замена из второй вкладки, 11a): по одному
    /// пути служебная страница нашла бы первую попавшуюся. Страница не должна менять свой адрес.
    pub(super) fn hand_off_this_run(&mut self) -> Result<HandOffOutcome, BrowserError> {
        let prefix = format!(
            "{}/site/pages/{}.html?run={}",
            self.page_origin, self.page_id, self.run
        );
        self.browser
            .hand_off_tab_with_options(&self.tab, &prefix, HandOffOptions::default())
    }

    /// Открывает в ТОМ ЖЕ браузере следующую страницу стенда (новая вкладка, новый прогон
    /// страницы) и делает её текущей: дальше `wait_page_ready`, `hand_off`, `tell_page` и
    /// `finish_page` работают с ней. Журнал плеера и браузера остаются прежними, поэтому
    /// сценарий видит, что плеер и посредник пережили предыдущую страницу.
    /// Старую вкладку закрываем ПОСЛЕ открытия новой: закрытие последней вкладки Firefox
    /// закрыло бы весь браузер.
    pub(super) fn open_next_page(
        &mut self,
        page_id: &'static str,
        run_prefix: &str,
    ) -> Result<(), ScenarioError> {
        let run = RunId::new_unique(run_prefix);
        let mut url = page_url_under(self.stand, &self.page_origin, page_id, &run);
        url.push_str(HANDOFF_MODE_QUERY);
        let next_tab = self.browser.open_tab(&url)?;
        let previous_tab = std::mem::replace(&mut self.tab, next_tab);
        self.run = run;
        self.page_id = page_id;
        self.browser.close_tab(previous_tab)?;
        Ok(())
    }

    /// Делает вкладку страницы стенда активной: попап экстеншена работает с активной вкладкой,
    /// а служебные страницы (настройки) её сбивают.
    pub(super) fn activate_page_tab(&mut self) -> Result<(), BrowserError> {
        self.browser.activate_tab(&self.tab)
    }

    /// Открывает служебную страницу экстеншена фоновой вкладкой (активной остаётся вкладка стенда):
    /// через неё робот читает вкладки и слой экстеншена, ничего не переключая.
    pub(super) fn open_service_page(&mut self) -> Result<(), BrowserError> {
        self.browser.open_service_page(&self.tab)
    }

    /// Закрывает вкладку страницы прогона, как зритель (11b2: возврат закрытой вкладки). Служебная
    /// страница должна быть уже открыта: Firefox, закрыв последнюю вкладку, закрыл бы весь браузер.
    pub(super) fn close_page_tab(&mut self) -> Result<(), BrowserError> {
        self.browser.close_tab(self.tab.clone())
    }

    /// Адрес страницы стенда без строки запроса: по нему служебная страница находит вкладку.
    pub(super) fn page_url_prefix(&self) -> String {
        format!("{}/site/pages/{}.html", self.page_origin, self.page_id)
    }

    /// Адрес другой страницы стенда на том же сервере (без строки запроса).
    pub(super) fn sibling_page_prefix(&self, page_id: &str) -> String {
        format!("{}/site/pages/{page_id}.html", self.page_origin)
    }

    /// Открывает ещё одну страницу стенда НОВОЙ вкладкой и делает её текущей, не закрывая прежнюю
    /// (в отличие от `open_next_page`): сеанс прежней вкладки живёт. Новая вкладка становится активной.
    pub(super) fn open_additional_page(
        &mut self,
        page_id: &'static str,
        run_prefix: &str,
    ) -> Result<(), ScenarioError> {
        let run = RunId::new_unique(run_prefix);
        let mut url = page_url_under(self.stand, &self.page_origin, page_id, &run);
        url.push_str(HANDOFF_MODE_QUERY);
        self.tab = self.browser.open_tab(&url)?;
        self.run = run;
        self.page_id = page_id;
        Ok(())
    }

    /// Открывает ещё одну страницу стенда НОВОЙ вкладкой с хвостом адреса `query_suffix` (после
    /// `run=…`) и делает её текущей, а прежнюю откладывает: её вкладка и сеанс живут, а вернуть её
    /// текущей можно [`HandoffRun::swap_with_parked`] (замена из второй вкладки, 11a).
    pub(super) fn park_and_open(
        &mut self,
        page_id: &'static str,
        run_prefix: &str,
        query_suffix: &str,
    ) -> Result<ParkedPage, ScenarioError> {
        let run = RunId::new_unique(run_prefix);
        let mut url = page_url_under(self.stand, &self.page_origin, page_id, &run);
        url.push_str(query_suffix);
        let tab = self.browser.open_tab(&url)?;
        Ok(self.swap_with_parked(ParkedPage { tab, run, page_id }))
    }

    /// Делает отложенную страницу текущей, а текущую откладывает (вкладки не активирует и не закрывает).
    pub(super) fn swap_with_parked(&mut self, parked: ParkedPage) -> ParkedPage {
        ParkedPage {
            tab: std::mem::replace(&mut self.tab, parked.tab),
            run: std::mem::replace(&mut self.run, parked.run),
            page_id: std::mem::replace(&mut self.page_id, parked.page_id),
        }
    }

    /// Строки журнала страницы с `needle` (журнал не расходуется).
    pub(super) fn page_log_lines(&self, needle: &str) -> Vec<String> {
        self.stand.site.log_lines_containing(&self.run, needle)
    }

    /// Строки журнала ДРУГОГО прогона страницы (например, страницы по ссылке: `<прогон>-t`) с `needle`.
    pub(super) fn log_lines_of(&self, run: &RunId, needle: &str) -> Vec<String> {
        self.stand.site.log_lines_containing(run, needle)
    }

    /// Ждёт строку журнала другого прогона страницы (`needle`).
    pub(super) fn wait_page_log_line_of(
        &self,
        run: &RunId,
        needle: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        self.stand
            .site
            .wait_log_line(run, needle, timeout)
            .map_err(|error| error.to_string())
    }

    /// Команда странице (она заберёт её опросом).
    pub(super) fn tell_page(&self, command: &str) {
        self.stand.site.send_command(&self.run, command);
    }

    /// Команда странице ДРУГОГО прогона (отложенная вкладка, страница по ссылке).
    pub(super) fn tell_page_of(&self, run: &RunId, command: &str) {
        self.stand.site.send_command(run, command);
    }

    /// Ждёт подтверждение команды страницей другого прогона.
    pub(super) fn wait_ack_of(
        &self,
        run: &RunId,
        command: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        self.wait_page_log_line_of(run, &format!("ack {command}"), timeout)
            .map(|_| ())
    }

    /// Просит страницу другого прогона подвести итог и ждёт её отчёт.
    pub(super) fn finish_page_of(&self, run: &RunId) -> Result<PageReport, SiteError> {
        self.tell_page_of(run, "finish");
        self.stand.site.wait_report(run, FINAL_REPORT_TIMEOUT)
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

    /// Ждёт итоговый отчёт страницы, которая отчитывается сама (без команды `finish`).
    pub(super) fn wait_final_report(&self, timeout: Duration) -> Result<PageReport, SiteError> {
        self.stand.site.wait_report(&self.run, timeout)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_public_page_still_reports_to_the_real_stand_origin() {
        let run = RunId::new("r-1").expect("run id");
        let url = page_url_with_report_origin(
            "http://public.bench.example:7",
            "http://127.0.0.1:7",
            "http://localhost:8",
            "video-src-local",
            &run,
        );
        assert_eq!(
            url,
            "http://public.bench.example:7/site/pages/video-src-local.html?run=r-1&report_origin=http://127.0.0.1:7&other_origin=http://localhost:8"
        );
        // Обычная страница: ничего не подменяется.
        let plain = page_url_with_report_origin(
            "http://127.0.0.1:7",
            "http://127.0.0.1:7",
            "http://localhost:8",
            "mse-fmp4",
            &run,
        );
        assert!(plain.contains("report_origin=http://127.0.0.1:7"));
    }
}
