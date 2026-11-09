//! Один сеанс страницы «сериал» для сценариев памяти выбора (14b2): запуск браузера (обычного или
//! приватного) на профиле окружения, передача вкладки, запись улик и честное закрытие.
//!
//! Зачем отдельный модуль. Сценарии памяти запускают браузер несколько раз ПОДРЯД на одном профиле
//! (перезапуск, приватное окно после обычного), и у каждого запуска свои улики: журнал страницы,
//! stderr браузера с отметками посредника, журнал плеера. Здесь они собираются в одном месте и
//! привязываются к ИМЕННО этому сеансу:
//! - журнал страницы у каждого сеанса свой (номер прогона `RunId` уникален);
//! - stderr браузера при новом запуске перезаписывается, поэтому отметки посредника читаются до
//!   следующего запуска;
//! - журнал плеера один на окружение и растёт: новый сеанс «забывает» прежнюю историю
//!   (`skip_history`), а прежний плеер к этому моменту обязан уйти с шины (`wait_until_gone`),
//!   иначе его сводки кадров засчитались бы новому сеансу.

use std::time::Duration;

use super::super::context::{ScenarioError, Stand};
use super::super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, wait_for_session_granted,
};
use super::super::handoff_run::{HANDOFF_MODE_QUERY, HandoffRun};
use super::super::outcome::Verdict;
use super::super::run_tweaks::RunTweaks;
use super::super::series::open_and_hand_off;
use crate::browsers::{BrowserKind, WindowPrivacy};
use crate::environment::BenchEnvironment;
use crate::observe::{HostLogEvidence, read_host_evidence};
use crate::player_control::PlayerControl;
use crate::site::{PageReport, SiteError};

/// Начало строки журнала страницы про новое поколение потока (`series-variants.html`).
pub(super) const GENERATION_LINE_PREFIX: &str = "generation cause=";
/// Сколько ждём, пока окно прежнего плеера уйдёт с шины после закрытия браузера.
const WINDOW_GONE_TIMEOUT: Duration = Duration::from_secs(20);

/// Выбор зрителя на самой странице: какую озвучку страница нажмёт сама и через сколько после команды.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PagePick {
    /// Идентификатор озвучки страницы (`dub-b`).
    pub dub_id: &'static str,
    /// Пауза между командой робота и кликом, мс.
    pub delay_ms: u32,
}

impl PagePick {
    /// Хвост адреса страницы: `&pick_dub=<id>@<мс>` (разбор — в `series-variants.html`).
    fn query_suffix(self) -> String {
        format!("&pick_dub={}@{}", self.dub_id, self.delay_ms)
    }
}

/// Как открыть сеанс.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SessionPlan {
    /// Окно браузера.
    pub window_privacy: WindowPrivacy,
    /// Выбор, который страница сделает сама (`None` — страница сама ничего не нажимает).
    pub page_pick: Option<PagePick>,
}

impl SessionPlan {
    /// Хвост адреса страницы прогона.
    pub(super) fn page_query(self) -> String {
        match self.page_pick {
            Some(pick) => format!("{HANDOFF_MODE_QUERY}{}", pick.query_suffix()),
            None => HANDOFF_MODE_QUERY.to_owned(),
        }
    }
}

/// Открывает страницу, ждёт её готовности и отдаёт вкладку плееру. Внутренний `Err` — готовый вердикт
/// провала (страница не готова, передача не началась).
pub(super) fn begin_session<'a>(
    stand: &'a Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    plan: SessionPlan,
) -> Result<Result<HandoffRun<'a>, Verdict>, ScenarioError> {
    let tweaks = RunTweaks {
        window_privacy: plan.window_privacy,
        ..RunTweaks::default()
    };
    let mut run = match open_and_hand_off(stand, environment, browser, &plan.page_query(), &tweaks)?
    {
        Ok(run) => run,
        Err(verdict) => return Ok(Err(verdict)),
    };
    // Журнал плеера один на окружение: прежние сеансы (если были) не должны засчитываться этому.
    run.player_log.skip_history()?;
    Ok(Ok(run))
}

/// Ждёт пропуск посредника на сеанс; причина провала дописывается в `failed_steps`.
pub(super) fn wait_for_grant(run: &HandoffRun<'_>, failed_steps: &mut Vec<String>) -> bool {
    match wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT) {
        Ok(Ok(())) => true,
        Ok(Err(reason)) => {
            failed_steps.push(format!("session-granted: {reason}"));
            false
        }
        Err(error) => {
            failed_steps.push(format!("session-granted: {error}"));
            false
        }
    }
}

/// Улики одного закончившегося сеанса.
pub(super) struct SessionRecord {
    /// Строки страницы про поколения потока (`generation cause=…`) в порядке появления.
    pub generation_lines: Vec<String>,
    /// Отметки посредника ЭТОГО запуска браузера.
    pub host: HostLogEvidence,
    /// Итоговый отчёт страницы.
    pub page_report: Result<PageReport, SiteError>,
    /// Запрещённые строки журнала плеера.
    pub forbidden_player_lines: Vec<String>,
    /// Журнал страницы для сообщения о провале.
    pub page_log: String,
}

/// Закрывает сеанс: отчёт страницы, улики, браузер, уход окна плеера. Провалы закрытия — в
/// `failed_steps`, а не молчаливое «закрылось».
pub(super) fn end_session(
    mut run: HandoffRun<'_>,
    environment: &BenchEnvironment,
    failed_steps: &mut Vec<String>,
) -> Result<SessionRecord, ScenarioError> {
    let page_report = run.finish_page();
    let generation_lines = run.page_log_lines(GENERATION_LINE_PREFIX);
    let page_log = format_browser_log(&run.take_page_log());
    let forbidden_player_lines = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    // Браузер закрыт: посредник уходит, плеер закрывает окно. Следующий сеанс не должен начаться,
    // пока этого не случилось.
    let control = PlayerControl::on_bus(environment.bus_address()?);
    if let Err(error) = control.wait_until_gone(WINDOW_GONE_TIMEOUT) {
        failed_steps.push(format!("player-window-gone: {error}"));
    }
    Ok(SessionRecord {
        generation_lines,
        host: read_host_evidence(&exit.stderr_log)?,
        page_report,
        forbidden_player_lines,
        page_log,
    })
}
