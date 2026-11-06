//! Строки входа «правый клик по ссылке» (решение 217): ссылка открывается вкладкой СПЕРЕДИ,
//! вход ждёт видео, отдаёт его плееру; вкладку закрывает только конец сеанса со стороны плеера.
//! Судья: вкладки браузера (читаются служебной страницей, активная вкладка не меняется),
//! кадры плеера, журнал страницы цели и плашка на вкладке входа.

use std::time::{Duration, Instant};

use super::super::context::{ScenarioError, Stand};
use super::super::handoff_run::ACK_TIMEOUT;
use super::super::outcome::Verdict;
use super::super::run_tweaks::RunTweaks;
use super::Entry;
use super::common::{
    DEFAULT_QUERY, EarlyUnsupported, Opened, ReadyRun, open_ready, poll_until, with_extra_failures,
};
use super::entry_evidence::collect;
use super::extension_probe::{TabInfo, read_tabs};
use super::menu_entry::ClickSpot;
use super::start_rows::prepare_entry;
use crate::browsers::BrowserKind;
use crate::environment::BenchEnvironment;
use crate::observe::read_host_evidence;
use crate::player_control::PlayerControl;
use crate::site::RunId;

/// Страница-источник со ссылками.
const SOURCE_PAGE: &str = "link-source";
/// Срок, за который вкладка по ссылке обязана открыться.
const TAB_OPEN_TIMEOUT: Duration = Duration::from_secs(20);
/// Срок, за который окно плеера закрывается после `Quit` или после возврата видео страницей.
const WINDOW_GONE_TIMEOUT: Duration = Duration::from_secs(40);
/// Срок, за который вкладка входа обязана закрыться после конца сеанса.
const TAB_CLOSE_TIMEOUT: Duration = Duration::from_secs(20);
/// Сколько вкладка входа обязана оставаться открытой после того, как страница забрала видео.
const TAB_MUST_STAY: Duration = Duration::from_secs(10);
/// Срок ожидания видео у входа по ссылке (`LINK_VIDEO_WAIT_MS` экстеншена) плюс запас на показ плашки.
const NO_VIDEO_BANNER_TIMEOUT: Duration = Duration::from_secs(90);
/// Плашка у входа по ссылке без видео.
const NO_VIDEO_BANNER: &str = "link_no_video_started";

/// Какая ссылка и что должно случиться.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LinkRow {
    /// Страница по ссылке играет сама: вкладка активна, кадры в плеере, `Quit` закрывает вкладку.
    Plays,
    /// Страница по ссылке не запускает видео сама: человек жмёт play на странице, и видео уезжает в плеер.
    ManualPlay,
    /// Страница по ссылке без видео: по сроку плашка, вкладка остаётся.
    NoVideo,
    /// Страница по ссылке забирает видео у плеера: вкладка не закрывается.
    PageWithdraws,
}

impl LinkRow {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Plays => "link-plays-and-closes",
            Self::ManualPlay => "link-manual-play-then-player",
            Self::NoVideo => "link-no-video",
            Self::PageWithdraws => "link-page-withdraws",
        }
    }

    /// Имя цели ссылки на странице-источнике и страница, на которую она ведёт.
    fn link_and_target(self) -> (&'static str, &'static str) {
        match self {
            Self::Plays => ("link:autoplay", "video-src"),
            Self::ManualPlay => ("link:manual", "link-manual-play"),
            Self::NoVideo => ("link:novideo", "link-empty"),
            Self::PageWithdraws => ("link:withdrawn", "source-withdrawn"),
        }
    }
}

pub(super) fn run_link(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    row: LinkRow,
) -> Result<Verdict, ScenarioError> {
    let mut ready = match open_ready(
        stand,
        environment,
        browser,
        SOURCE_PAGE,
        &RunTweaks::default(),
        DEFAULT_QUERY,
        EarlyUnsupported::OnlyWhereTheTableAllows,
    )? {
        Opened::Ready(ready) => *ready,
        Opened::Finished(verdict) => return Ok(verdict),
    };
    prepare_entry(&mut ready, Entry::LinkMenu)?;
    let (link, target_page) = row.link_and_target();
    let source_prefix = ready.run.sibling_page_prefix(SOURCE_PAGE);
    let target_prefix = ready.run.sibling_page_prefix(target_page);
    let mut failures = Vec::new();
    ready.enter_at(Entry::LinkMenu, ClickSpot::center_of(link))?;
    let mut notes = vec![check_tab_opened_and_active(
        &mut ready,
        &target_prefix,
        &mut failures,
    )];
    let frames = match row {
        LinkRow::NoVideo => {
            if let Err(problem) =
                ready.wait_for_banner_on(&target_prefix, NO_VIDEO_BANNER, NO_VIDEO_BANNER_TIMEOUT)
            {
                failures.push(problem);
            }
            failures.extend(tab_still_open(&mut ready, &target_prefix));
            notes.push(
                "no video started within the limit: banner shown, the tab stays open".to_owned(),
            );
            Err("no player expected on this row".to_owned())
        }
        LinkRow::Plays | LinkRow::ManualPlay | LinkRow::PageWithdraws => {
            let target_run = target_run(&ready)?;
            if row == LinkRow::ManualPlay {
                match press_play_on_the_page(&ready, &target_run)? {
                    PlayStep::Pressed(note) => notes.push(note),
                    PlayStep::SessionBeforePlay => {
                        ready.run.browser.shutdown()?;
                        return Ok(Verdict::fail(
                            "a session started within the idle period although the viewer had not pressed play: the link entry must hand over only a video that plays",
                        ));
                    }
                }
            }
            // Плашка живёт 5 секунд: ждём её сразу, а не после ожидания кадров.
            if let Err(problem) = ready.wait_for_banner_on(
                &target_prefix,
                "opened",
                super::menu_entry::BANNER_TIMEOUT,
            ) {
                failures.push(problem);
            }
            let frames = ready.wait_frames()?;
            stand.site.send_command(&target_run, "handed-off");
            let control = PlayerControl::on_bus(environment.bus_address()?);
            if row != LinkRow::PageWithdraws {
                notes.push(end_session_with_quit(
                    &mut ready,
                    &control,
                    &source_prefix,
                    &target_prefix,
                    &mut failures,
                ));
            } else {
                notes.push(withdraw_and_check_tab_stays(
                    stand,
                    &mut ready,
                    &control,
                    &target_run,
                    &target_prefix,
                    &mut failures,
                ));
            }
            frames
        }
    };
    let mut evidence = collect(ready, frames)?;
    let mut all_failures = failures;
    if row == LinkRow::NoVideo {
        // На этой строке плеера нет: судим по отсутствию пропуска, а не по кадрам.
        evidence.frames = Ok(crate::observe::FrameEvidence {
            presented_frames: 0,
            position_regressions: 0,
            summary_line: "no player expected".to_owned(),
        });
        if evidence.host.session_granted {
            all_failures
                .push("the player granted a session although the page has no video".to_owned());
        }
        all_failures.extend(
            evidence
                .failures(SOURCE_PAGE)
                .into_iter()
                .filter(|failure| !failure.contains("host did not")),
        );
    } else {
        all_failures.extend(evidence.failures(SOURCE_PAGE));
    }
    let summary = format!(
        "{}; presented_frames={}",
        notes.join("; "),
        evidence.presented_frames()
    );
    Ok(if all_failures.is_empty() {
        Verdict::pass(summary)
    } else {
        with_extra_failures(
            Verdict::pass(format!("{summary}\n{}", evidence.page_log)),
            &all_failures,
        )
    })
}

/// Сколько вход по ссылке ждёт, ничего не получив: заведомо дольше первого опроса (1 с) и заведомо
/// короче срока ожидания видео (60 с). За это время сеанса быть не должно.
const IDLE_BEFORE_PLAY: Duration = Duration::from_secs(15);

/// Чем кончилось ожидание перед нажатием play.
enum PlayStep {
    /// Сеанс до нажатия не начался, потом настоящий клик по кнопке страницы.
    Pressed(String),
    /// Сеанс начался, пока видео только стояло: вход по ссылке обязан передавать только играющее видео.
    SessionBeforePlay,
}

/// Человек сам жмёт play: пока видео стоит, сеанса нет; потом НАСТОЯЩИЙ левый клик по кнопке страницы.
fn press_play_on_the_page(
    ready: &ReadyRun<'_, '_>,
    target_run: &RunId,
) -> Result<PlayStep, ScenarioError> {
    let deadline = Instant::now() + IDLE_BEFORE_PLAY;
    while Instant::now() < deadline {
        if read_host_evidence(&ready.run.stderr_log)?.session_granted {
            return Ok(PlayStep::SessionBeforePlay);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    ready
        .run
        .wait_page_log_line_of(target_run, "handoff-ready", Duration::from_secs(30))
        .map_err(ScenarioError::EntryNotActivated)?;
    let place = ready.locate_page_of(target_run)?;
    let button = place
        .point_in("play", 0.5, 0.5)
        .map_err(ScenarioError::EntryNotActivated)?;
    ready.robot.click_screen(button)?;
    Ok(PlayStep::Pressed(format!(
        "no session for {IDLE_BEFORE_PLAY:?} while the video stood; then a real click on the page's play button"
    )))
}

/// Имя прогона страницы цели: `<прогон>-t` (его строит страница-источник для ссылок).
fn target_run(ready: &ReadyRun<'_, '_>) -> Result<RunId, ScenarioError> {
    Ok(RunId::new(&format!("{}-t", ready.run.run))?)
}

/// Вкладка по ссылке открылась и стала АКТИВНОЙ (решение 217: видео в фоновой вкладке не стартует).
fn check_tab_opened_and_active(
    ready: &mut ReadyRun<'_, '_>,
    target_prefix: &str,
    failures: &mut Vec<String>,
) -> String {
    let found = poll_until(TAB_OPEN_TIMEOUT, || {
        read_tabs(ready.run.browser.as_mut())
            .ok()
            .filter(|tabs| tabs.iter().any(|tab| tab.url.starts_with(target_prefix)))
    });
    match found {
        Some(tabs) => {
            let active = tabs
                .iter()
                .any(|tab| tab.active && tab.url.starts_with(target_prefix));
            if !active {
                failures.push(format!(
                    "the link tab opened but is not the active tab: {tabs:?}"
                ));
            }
            format!("link tab opened, active={active}")
        }
        None => {
            failures.push(format!(
                "no tab with {target_prefix} appeared within {TAB_OPEN_TIMEOUT:?}"
            ));
            "link tab never opened".to_owned()
        }
    }
}

/// Вкладка по ссылке всё ещё открыта (иначе причина).
fn tab_still_open(ready: &mut ReadyRun<'_, '_>, target_prefix: &str) -> Option<String> {
    match read_tabs(ready.run.browser.as_mut()) {
        Ok(tabs) if tabs.iter().any(|tab| tab.url.starts_with(target_prefix)) => None,
        Ok(tabs) => Some(format!(
            "the link tab was closed although it must stay open: {tabs:?}"
        )),
        Err(problem) => Some(problem),
    }
}

/// Человек закрывает окно плеера: вкладка входа закрывается, активной становится исходная.
fn end_session_with_quit(
    ready: &mut ReadyRun<'_, '_>,
    control: &PlayerControl,
    source_prefix: &str,
    target_prefix: &str,
    failures: &mut Vec<String>,
) -> String {
    if let Err(error) = control.wait_until_controllable(Duration::from_secs(30)) {
        failures.push(format!("the player window is not controllable: {error}"));
    }
    if let Err(error) = control.quit_window() {
        failures.push(format!("quit_window: {error}"));
    }
    if let Err(error) = control.wait_until_gone(WINDOW_GONE_TIMEOUT) {
        failures.push(format!("the player window did not close: {error}"));
    }
    let settled = poll_until(TAB_CLOSE_TIMEOUT, || {
        read_tabs(ready.run.browser.as_mut())
            .ok()
            .filter(|tabs| tab_closed_and_source_active(tabs, source_prefix, target_prefix))
    });
    match settled {
        Some(_) => "player window closed: the link tab closed and the source tab is active again"
            .to_owned(),
        None => {
            let tabs = read_tabs(ready.run.browser.as_mut());
            failures.push(format!(
                "after the player closed, the link tab must be gone and the source tab active; tabs now: {tabs:?}"
            ));
            "the link tab did not close".to_owned()
        }
    }
}

/// Вкладки после конца сеанса: вкладки по ссылке нет, активна исходная.
fn tab_closed_and_source_active(
    tabs: &[TabInfo],
    source_prefix: &str,
    target_prefix: &str,
) -> bool {
    !tabs.iter().any(|tab| tab.url.starts_with(target_prefix))
        && tabs
            .iter()
            .any(|tab| tab.active && tab.url.starts_with(source_prefix))
}

/// Страница сама забирает видео; сеанс кончается, а вкладка НЕ закрывается: она уже человека.
fn withdraw_and_check_tab_stays(
    stand: &Stand,
    ready: &mut ReadyRun<'_, '_>,
    control: &PlayerControl,
    target_run: &RunId,
    target_prefix: &str,
    failures: &mut Vec<String>,
) -> String {
    const WITHDRAW: &str = "hook:withdraw-source";
    stand.site.send_command(target_run, WITHDRAW);
    if let Err(error) =
        stand
            .site
            .wait_log_line(target_run, &format!("ack {WITHDRAW}"), ACK_TIMEOUT)
    {
        failures.push(format!(
            "the page did not confirm taking the video back: {error}"
        ));
    }
    if let Err(error) = control.wait_until_gone(WINDOW_GONE_TIMEOUT) {
        failures.push(format!(
            "the player window did not close after the page took the video back: {error}"
        ));
    }
    let deadline = Instant::now() + TAB_MUST_STAY;
    while Instant::now() < deadline {
        if let Some(problem) = tab_still_open(ready, target_prefix) {
            failures.push(problem);
            return "the link tab was closed after the page took its video back".to_owned();
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    format!(
        "the page took the video back, the player window closed, the link tab stayed open for {TAB_MUST_STAY:?}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(url: &str, active: bool) -> TabInfo {
        TabInfo {
            url: url.to_owned(),
            active,
        }
    }

    #[test]
    fn the_end_state_needs_the_link_tab_gone_and_the_source_tab_active() {
        let source = "http://h/site/pages/link-source.html";
        let target = "http://h/site/pages/video-src.html";
        let done = [tab(&format!("{source}?run=a"), true)];
        assert!(tab_closed_and_source_active(&done, source, target));
        let target_still_open = [
            tab(&format!("{source}?run=a"), false),
            tab(&format!("{target}?run=a-t"), true),
        ];
        assert!(!tab_closed_and_source_active(
            &target_still_open,
            source,
            target
        ));
        let source_not_active = [
            tab(&format!("{source}?run=a"), false),
            tab("about:blank", true),
        ];
        assert!(!tab_closed_and_source_active(
            &source_not_active,
            source,
            target
        ));
    }

    #[test]
    fn every_link_row_points_at_a_known_link_and_target_page() {
        for row in [
            LinkRow::Plays,
            LinkRow::ManualPlay,
            LinkRow::NoVideo,
            LinkRow::PageWithdraws,
        ] {
            let (link, target) = row.link_and_target();
            assert!(link.starts_with("link:"));
            assert!(
                crate::scenarios::page_catalog::PAGE_SCENARIOS
                    .iter()
                    .any(|page| page.id == target)
            );
        }
    }
}
