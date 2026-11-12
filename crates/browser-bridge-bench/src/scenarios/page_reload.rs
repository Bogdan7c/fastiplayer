//! Сценарий `page-reload` (сессия 16c, решения 59, 304, 305): страница с переданным видео перезагружается
//! или уходит по адресу В ТОЙ ЖЕ вкладке; экстеншен ждёт новый документ до срока (10 с по умолчанию).
//!
//! Строки (в Chromium и в Firefox, каждая в свежем окружении):
//!  1. `reload-same-url`: `location.reload()`, второй документ играет тот же фильм → замена в том же окне;
//!  2. `next-episode-new-url`: переход на тот же origin с `ep=2` → замена;
//!  3. `iframe-player-reload`: плеер в iframe чужого origin, вкладка на главном; верхний `reload()` → замена
//!     (сайт судится по адресу вкладки, а не рамки);
//!  4. `page-without-video`: переход на страницу того же origin без видео → через ~10 с прежний конец:
//!     «Source tab closed» (`notice=finished cause=tab_gone`), окно закрылось;
//!  5. `other-site`: переход на другой origin (`localhost` вместо `127.0.0.1`) → старый сеанс кончается сразу,
//!     видео нового документа играет сам браузер.
//!
//! Страницу уводит она сама по команде робота `hook:navigate` (настоящая навигация сайта). Робот ничего не
//! знает о вкладке: он смотрит на журнал плеера (окно, сеансы, полный экран, кадры, конец) и на журнал
//! страницы (какие документы загрузились, играет ли браузер сам). Приговоры — чистые функции в [`judge`].

mod judge;

use std::time::{Duration, Instant};

use self::judge::{
    EndObservation, EndSignal, Expectation, ExpectedDocument, FULLSCREEN_MARKER,
    MIN_BROWSER_FRAMES, MIN_FRAMES_AFTER_REPLACEMENT, PAGE_STATE_MARKER, PLAYER_PROCESS_MARKER,
    PlayerJournal, REPLACED_PREVIOUS_GRANT, ReloadEvidence, SESSION_OPENED_MARKER,
    WINDOW_CREATED_MARKER, judge, parse_load, parse_state,
};
use super::context::{ScenarioError, Stand};
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, wait_for_session_granted,
};
use super::handoff_run::{
    ACK_TIMEOUT, ExtensionMode, HANDOFF_MODE_QUERY, HandoffRun, PageReadiness,
};
use super::outcome::{StepResult, Verdict};
use super::page_ends::start_of_session;
use super::page_filter::PageFilter;
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::{BenchEnvironment, PlayerConfigProfile};
use crate::observe::{ObserveError, PlayerLog, parse_end_notice, read_host_evidence};
use crate::player_control::PlayerControl;

/// Страница сценария (вкладка) и её помощник (плеер в iframe).
const PAGE: &str = "page-reload";
/// Команда странице «уходи» (договор с `site/lib/page-reload-kit.js`).
const NAVIGATE_HOOK: &str = "hook:navigate";
/// Метка журнала страницы, с которой начинаются все её строки.
const PAGE_LINE_PREFIX: &str = "page-reload: ";

/// Сколько ждём замену сеанса: срок экстеншена (10 с), загрузка нового документа, тень, запас.
const REPLACEMENT_TIMEOUT: Duration = Duration::from_secs(50);
/// Насколько кадры должны вырасти между двумя сводками после замены.
const GROWTH_TIMEOUT: Duration = Duration::from_secs(45);
/// Сколько второй документ живёт под тенью, прежде чем судить его декодер (порог — 6 строк состояния).
const SHADOW_OBSERVE: Duration = Duration::from_secs(8);
/// Сколько ждём первый признак конца старого сеанса: срок ожидания экстеншена и запас.
const FIRST_END_SIGNAL_TIMEOUT: Duration = Duration::from_secs(40);
/// Сколько ждём сообщение о конце: после признака конца окно ещё доигрывает скачанное (ролик 30 с).
const END_NOTICE_TIMEOUT: Duration = Duration::from_secs(60);
/// Сколько окно может закрываться после сообщения о конце.
const WINDOW_GONE_TIMEOUT: Duration = Duration::from_secs(30);
/// Сколько ждём, пока браузер сам проиграет видео другого сайта настолько, что его кадры можно судить.
const BROWSER_PLAYBACK_TIMEOUT: Duration = Duration::from_secs(30);
/// Шаг опроса журнала страницы.
const PAGE_POLL: Duration = Duration::from_millis(500);

/// Подстроки журнала плеера, любая из которых значит «старый сеанс кончается» (первый признак конца).
const END_SIGNAL_MARKERS: [&str; 3] = [
    "Process lifecycle followed the page link",
    "Browser session ended",
    "Browser session end notice shown",
];

/// Строка сценария: имя, хвост адреса, что она обязана доказать и куда уйдёт страница.
#[derive(Debug, Clone, Copy)]
struct ReloadRow {
    name: &'static str,
    /// Хвост адреса страницы после `mode=handoff`: вид документа и куда он уходит.
    page_query: &'static str,
    expectation: Expectation,
    /// Какие документы должны загрузиться после ухода (вид, серия).
    second_documents: &'static [ExpectedDocument],
    /// Меняется ли origin вкладки (только «другой сайт»).
    origin_changes: bool,
}

const ONE_FILM: &[ExpectedDocument] = &[ExpectedDocument {
    view: "film",
    episode: "1",
}];

/// Строки сценария в порядке прогона.
const ROWS: [ReloadRow; 5] = [
    ReloadRow {
        name: "reload-same-url",
        page_query: "&view=film&nav=reload",
        expectation: Expectation::Replacement,
        second_documents: ONE_FILM,
        origin_changes: false,
    },
    ReloadRow {
        name: "next-episode-new-url",
        page_query: "&view=film&ep=1&nav=episode2",
        expectation: Expectation::Replacement,
        second_documents: &[ExpectedDocument {
            view: "film",
            episode: "2",
        }],
        origin_changes: false,
    },
    ReloadRow {
        name: "iframe-player-reload",
        page_query: "&view=frame&nav=reload",
        expectation: Expectation::Replacement,
        second_documents: &[
            ExpectedDocument {
                view: "frame",
                episode: "1",
            },
            ExpectedDocument {
                view: "frame-inner",
                episode: "1",
            },
        ],
        origin_changes: false,
    },
    ReloadRow {
        name: "page-without-video",
        page_query: "&view=film&nav=empty",
        expectation: Expectation::EndsAfterWait,
        second_documents: &[ExpectedDocument {
            view: "empty",
            episode: "1",
        }],
        origin_changes: false,
    },
    ReloadRow {
        name: "other-site",
        page_query: "&view=film&nav=other-site",
        expectation: Expectation::EndsAtOnce,
        second_documents: ONE_FILM,
        origin_changes: true,
    },
];

impl ReloadRow {
    /// Хвост адреса страницы целиком.
    fn page_query(&self) -> String {
        format!("{HANDOFF_MODE_QUERY}{}", self.page_query)
    }

    /// Профиль настроек плеера: замена проверяет сохранение запомненного полного экрана.
    fn player_config(&self, bench_default: PlayerConfigProfile) -> PlayerConfigProfile {
        match self.expectation {
            Expectation::Replacement => PlayerConfigProfile::RememberedFullscreen,
            Expectation::EndsAfterWait | Expectation::EndsAtOnce => bench_default,
        }
    }
}

/// Идентификаторы страниц сценария (для проверки `--page` в командной строке).
pub(super) fn page_ids() -> Vec<&'static str> {
    vec![PAGE]
}

/// Все строки одного браузера (каждая в свежем окружении), с учётом фильтра по страницам.
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    if !filter.admits(PAGE) {
        return Vec::new();
    }
    ROWS.iter()
        .map(|row| run_row(stand, browser, *row))
        .collect()
}

fn run_row(stand: &Stand, browser: BrowserKind, row: ReloadRow) -> StepResult {
    let started = Instant::now();
    let profile = row.player_config(stand.settings.player_config);
    let verdict = stand.with_environment_for_player_config(profile, |environment| {
        drive(stand, environment, browser, row)
    });
    StepResult::new("page-reload", browser, row.name, verdict, started.elapsed())
}

/// Открыть страницу, дождаться готовности и отдать вкладку. `Err` — готовый вердикт провала.
fn open_and_hand_off<'a>(
    stand: &'a Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    row: &ReloadRow,
) -> Result<Result<HandoffRun<'a>, Verdict>, ScenarioError> {
    let mut run = HandoffRun::open_with_query(
        stand,
        environment,
        browser_kind,
        PAGE,
        "page-reload",
        ExtensionMode::Real,
        &row.page_query(),
    )?;
    if !matches!(run.wait_page_ready(), PageReadiness::Ready) {
        let log = format_browser_log(&run.take_page_log());
        run.browser.shutdown()?;
        return Ok(Err(Verdict::fail(format!(
            "the page never became ready for the hand-off\n{log}"
        ))));
    }
    if let Err(problem) = apply_wait_override(&mut run) {
        run.browser.shutdown()?;
        return Ok(Err(Verdict::fail(problem)));
    }
    match run.hand_off() {
        Ok(HandOffOutcome::Started) => {}
        other => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Err(Verdict::fail(format!(
                "hand_off_tab: {other:?}\n{log}"
            ))));
        }
    }
    // По этой команде второй документ узнает себя (см. шапку `page-reload-kit.js`).
    run.tell_page("handed-off");
    Ok(Ok(run))
}

/// Отладочный рычаг: переменная окружения задаёт срок ожидания экстеншена (секунды, 0 = не ждать).
/// Нужна, чтобы доказать, что приговоры ловят сломанный экстеншен: при 0 строки замены обязаны провалиться.
const WAIT_OVERRIDE_VARIABLE: &str = "BRIDGE_BENCH_PAGE_RELOAD_WAIT_SECONDS";
/// Ключ настройки в `storage.local` экстеншена (`pageReloadWaitSettings`, значение `{ seconds }`).
const WAIT_SETTINGS_KEY: &str = "pageReloadWaitSettings";
/// Сколько даём экстеншену подхватить записанную настройку.
const SETTINGS_SETTLE: Duration = Duration::from_secs(2);

/// Если задан [`WAIT_OVERRIDE_VARIABLE`], пишет срок ожидания в хранилище экстеншена (как страница настроек).
fn apply_wait_override(run: &mut HandoffRun<'_>) -> Result<(), String> {
    let Ok(raw) = std::env::var(WAIT_OVERRIDE_VARIABLE) else {
        return Ok(());
    };
    let seconds: u32 = raw
        .parse()
        .map_err(|error| format!("{WAIT_OVERRIDE_VARIABLE}={raw}: {error}"))?;
    run.open_service_page()
        .map_err(|error| format!("open the service page: {error}"))?;
    let script = format!(
        "async (api) => {{ await api.storage.local.set({{ {WAIT_SETTINGS_KEY}: {{ seconds: {seconds} }} }}); \
         return JSON.stringify((await api.storage.local.get({WAIT_SETTINGS_KEY:?}))[{WAIT_SETTINGS_KEY:?}] ?? null); }}"
    );
    let stored = run
        .browser
        .run_in_service_page(&script)
        .map_err(|error| format!("write the wait setting: {error}"))?;
    if !stored.contains(&format!("\"seconds\":{seconds}")) {
        return Err(format!("the wait setting was not stored: {stored}"));
    }
    std::thread::sleep(SETTINGS_SETTLE);
    run.activate_page_tab()
        .map_err(|error| format!("activate the page tab: {error}"))
}

/// Что стенд увидел после ухода страницы (то, что зависит от строки).
#[derive(Debug, Default)]
struct RowOutcome {
    frame_growth: Option<Result<u64, String>>,
    end: Option<EndObservation>,
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    row: ReloadRow,
) -> Result<Verdict, ScenarioError> {
    let mut run = match open_and_hand_off(stand, environment, browser_kind, &row)? {
        Ok(run) => run,
        Err(verdict) => return Ok(verdict),
    };
    let control = PlayerControl::on_bus(environment.bus_address()?);
    let mut failed_steps: Vec<String> = Vec::new();
    let mut outcome = RowOutcome::default();
    let started = match wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)? {
        Ok(()) => start_of_session(&mut run, &control)?,
        Err(reason) => Err(format!("session-granted: {reason}")),
    };
    match started {
        Ok(()) => outcome = walk_row(&mut run, &control, &row, &mut failed_steps)?,
        Err(reason) => failed_steps.push(format!("hand-off: {reason}")),
    }
    let journal = read_journal(&mut run.player_log)?;
    let page_lines = run.page_log_lines(PAGE_LINE_PREFIX);
    let page_log = run.take_page_log();
    let forbidden_player_lines = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    Ok(judge(
        row.expectation,
        row.second_documents,
        row.origin_changes,
        &ReloadEvidence {
            failed_steps,
            journal,
            loads: page_lines
                .iter()
                .filter_map(|line| parse_load(line))
                .collect(),
            states: page_lines
                .iter()
                .filter_map(|line| parse_state(line))
                .collect(),
            frame_growth: outcome.frame_growth,
            end: outcome.end,
            host: read_host_evidence(&exit.stderr_log)?,
            forbidden_player_lines,
            page_log: format_browser_log(&page_log),
        },
    ))
}

/// Собирает строки журнала плеера по смыслу.
fn read_journal(player_log: &mut PlayerLog) -> Result<PlayerJournal, ObserveError> {
    Ok(PlayerJournal {
        window_created: player_log.snapshot_lines_containing(WINDOW_CREATED_MARKER)?,
        process_starts: player_log.snapshot_lines_containing(PLAYER_PROCESS_MARKER)?,
        session_opened: player_log.snapshot_lines_containing(SESSION_OPENED_MARKER)?,
        fullscreen_changes: player_log.snapshot_lines_containing(FULLSCREEN_MARKER)?,
    })
}

/// Шаги строки после передачи: страницу просят уйти, дальше — смотрим на замену или на конец.
fn walk_row(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    row: &ReloadRow,
    failed_steps: &mut Vec<String>,
) -> Result<RowOutcome, ScenarioError> {
    run.player_log.skip_history().map_err(ScenarioError::from)?;
    run.tell_page(NAVIGATE_HOOK);
    if let Err(reason) = run.wait_ack(NAVIGATE_HOOK, ACK_TIMEOUT) {
        failed_steps.push(format!("page-acknowledged-leaving: {reason}"));
        return Ok(RowOutcome::default());
    }
    // Отсчёт сроков — от подтверждения: страница пишет его прямо перед переходом.
    let navigated_at = Instant::now();
    let mut outcome = RowOutcome::default();
    match row.expectation {
        Expectation::Replacement => walk_replacement(run, control, &mut outcome, failed_steps),
        Expectation::EndsAfterWait => {
            outcome.end = Some(observe_old_end(run, control, navigated_at));
        }
        Expectation::EndsAtOnce => {
            outcome.end = Some(observe_old_end(run, control, navigated_at));
            if let Err(reason) = wait_browser_plays_by_itself(run) {
                failed_steps.push(format!("browser-plays-other-site: {reason}"));
            }
        }
    }
    Ok(outcome)
}

/// Замена: окно получило новый сеанс (`ReplacedPrevious`), кадры идут, второй документ под тенью.
fn walk_replacement(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    outcome: &mut RowOutcome,
    failed_steps: &mut Vec<String>,
) {
    if let Err(error) = run
        .player_log
        .wait_for_line(REPLACED_PREVIOUS_GRANT, REPLACEMENT_TIMEOUT)
    {
        failed_steps.push(format!("replacement-session-opened: {error}"));
        return;
    }
    let growth = run
        .player_log
        .skip_history()
        .map_err(|error| error.to_string())
        .and_then(|()| {
            run.player_log
                .wait_for_frame_growth(MIN_FRAMES_AFTER_REPLACEMENT, GROWTH_TIMEOUT)
                .map(|growth| growth.frames_gained())
                .map_err(|error| error.to_string())
        });
    outcome.frame_growth = Some(growth);
    // Второму документу нужно пожить под тенью, чтобы его декодер можно было судить.
    std::thread::sleep(SHADOW_OBSERVE);
    if let Err(error) = control.wait_until_controllable(Duration::from_secs(5)) {
        failed_steps.push(format!("window-still-controllable: {error}"));
    }
}

/// Старый сеанс кончается без замены: первый признак конца и срок, сообщение, закрытие окна.
fn observe_old_end(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    navigated_at: Instant,
) -> EndObservation {
    let first_signal = run
        .player_log
        .wait_for_any_line(&END_SIGNAL_MARKERS, FIRST_END_SIGNAL_TIMEOUT)
        .map(|line| EndSignal {
            line,
            after_navigation: navigated_at.elapsed(),
        })
        .map_err(|error| format!("no sign that the old session ended: {error}"));
    // Если первым признаком уже было сообщение о конце, повторно его ждать нечего.
    let notice = match &first_signal {
        Ok(signal) if parse_end_notice(&signal.line).is_some() => {
            parse_end_notice(&signal.line).ok_or_else(|| "end notice line vanished".to_owned())
        }
        _ => run
            .player_log
            .wait_for_end_notice(END_NOTICE_TIMEOUT)
            .map_err(|error| format!("no end notice from the player: {error}")),
    };
    let window_closed = control
        .wait_until_gone(WINDOW_GONE_TIMEOUT)
        .map_err(|error| format!("the player window did not close: {error}"));
    EndObservation {
        first_signal,
        notice,
        window_closed,
    }
}

/// Ждёт, пока документ другого сайта покажет достаточно настоящих кадров браузера (он играет сам).
fn wait_browser_plays_by_itself(run: &HandoffRun<'_>) -> Result<(), String> {
    let deadline = Instant::now() + BROWSER_PLAYBACK_TIMEOUT;
    loop {
        let best = run
            .page_log_lines(PAGE_STATE_MARKER)
            .iter()
            .filter_map(|line| parse_state(line))
            .filter(|state| state.second_load)
            .map(|state| state.real_frames)
            .max()
            .unwrap_or(0);
        if best >= MIN_BROWSER_FRAMES {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the other-site document showed {best} real frames within {BROWSER_PLAYBACK_TIMEOUT:?}"
            ));
        }
        std::thread::sleep(PAGE_POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::page_catalog::PAGE_SCENARIOS;

    fn page_source(file: &str) -> String {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file))
            .expect("site file")
    }

    #[test]
    fn rows_cover_the_promised_cases_on_a_known_page() {
        let names: Vec<&str> = ROWS.iter().map(|row| row.name).collect();
        assert_eq!(
            names,
            [
                "reload-same-url",
                "next-episode-new-url",
                "iframe-player-reload",
                "page-without-video",
                "other-site"
            ]
        );
        assert!(PAGE_SCENARIOS.iter().any(|scenario| scenario.id == PAGE));
        assert_eq!(page_ids(), [PAGE]);
    }

    #[test]
    fn only_replacement_rows_start_in_remembered_fullscreen() {
        let profiles: Vec<PlayerConfigProfile> = ROWS
            .iter()
            .map(|row| row.player_config(PlayerConfigProfile::Clean))
            .collect();
        assert_eq!(
            profiles,
            [
                PlayerConfigProfile::RememberedFullscreen,
                PlayerConfigProfile::RememberedFullscreen,
                PlayerConfigProfile::RememberedFullscreen,
                PlayerConfigProfile::Clean,
                PlayerConfigProfile::Clean,
            ]
        );
    }

    #[test]
    fn the_page_filter_decides_whether_the_rows_run_at_all() {
        let other_page = PageFilter::only(["mse-fmp4".to_owned()]);
        assert!(!other_page.admits(PAGE));
        assert!(PageFilter::only([PAGE.to_owned()]).admits(PAGE));
    }

    #[test]
    fn row_queries_carry_the_mode_the_view_and_the_way_out() {
        assert_eq!(ROWS[0].page_query(), "&mode=handoff&view=film&nav=reload");
        assert!(ROWS[1].page_query().ends_with("&nav=episode2"));
        assert!(ROWS[2].page_query().contains("&view=frame&"));
        assert!(ROWS[3].page_query().ends_with("&nav=empty"));
        assert!(ROWS[4].page_query().ends_with("&nav=other-site"));
    }

    #[test]
    fn the_pages_speak_the_language_the_robot_and_the_judge_expect() {
        let page = page_source("site/pages/page-reload.html");
        for fragment in [
            "'reload'",
            "'episode2'",
            "'empty'",
            "'other-site'",
            "get('view')",
            "get('nav')",
            "second_load",
        ] {
            assert!(page.contains(fragment), "page-reload.html lacks {fragment}");
        }
        let kit = page_source("site/lib/page-reload-kit.js");
        for fragment in [
            NAVIGATE_HOOK,
            "'handed-off'",
            "handoff-ready",
            "location.reload()",
            "location.assign(url)",
            "state doc=",
            "load doc=",
            "ack ",
            judge::PAGE_LOAD_MARKER.trim_start_matches("page-reload: "),
        ] {
            assert!(
                kit.contains(fragment),
                "page-reload-kit.js lacks {fragment}"
            );
        }
        let frame = page_source("site/pages/page-reload-frame.html");
        assert!(frame.contains("second_load"));
        assert!(frame.contains("frame-inner"));
    }

    #[test]
    fn an_end_notice_line_is_recognised_as_the_first_signal_of_the_end() {
        let notice =
            "INFO Browser session end notice shown notice=finished cause=tab_gone end=drained";
        assert!(
            END_SIGNAL_MARKERS
                .iter()
                .any(|marker| notice.contains(marker))
        );
        assert!(parse_end_notice(notice).is_some());
    }
}
