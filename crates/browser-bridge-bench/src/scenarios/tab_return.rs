//! `tab-return` (сессия 11b2, решения 240–241): вкладку с переданным видео закрывают, пока окно плеера на
//! паузе; зритель жмёт play в окне — экстеншен открывает ту же страницу заново, и тот же сеанс продолжается
//! в том же окне. Страница стенда — `site/pages/tab-return.html` (грузится дважды с одним `?run=`).
//!
//! Строки:
//!  1. `return-mse`, `return-file` — страница вернулась: новая вкладка с тем же адресом, плеер пишет «The page
//!     came back», кадры дальше идут в рендер, позиция — та же секунда ± 2 с (без прыжка к началу ролика),
//!     вернувшийся документ под тенью почти не декодирует (потолок `capture`), окно управляемо;
//!  2. `refused-other-video` — заново открытая страница показывает другой ролик (одностраничник): отказ
//!     `not_the_same_video`, окно досматривает скачанное и закрывается, открытая вкладка закрыта;
//!  3. `window-closed-while-reopening` — страница открылась, но видео не запускает, а зритель закрыл окно:
//!     открытие отменено, открытая экстеншеном вкладка закрыта.
//!
//! Мышь не нужна: плеером управляет MPRIS на частной шине, вкладками — служебная страница экстеншена.

mod judge;

use std::time::{Duration, Instant};

use self::judge::{judge_return_position, judge_second_document_decoding, parse_state};
use super::context::{ScenarioError, Stand};
use super::entries::extension_probe::read_tabs;
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, format_browser_log, host_failures, unexpected_player_lines,
};
use super::handoff_run::{ExtensionMode, HANDOFF_MODE_QUERY, HandoffRun, PageReadiness};
use super::outcome::{StepResult, Verdict};
use super::page_ends::start_of_session;
use super::page_filter::PageFilter;
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::read_host_evidence;
use crate::player_control::PlayerControl;

/// Страница сценария.
const TAB_RETURN_PAGE: &str = "tab-return";
/// Переменная окружения для отладки: только строки, в подписи которых есть этот текст.
const SUBJECT_FILTER_VARIABLE: &str = "BRIDGE_BENCH_TAB_RETURN_ROW";

/// Строки журнала плеера, по которым идёт сценарий (session_media/page_reopen.rs, translator/page_link.rs).
/// Главный цикл отдал новость о связи со страницей машине выхода (`runtime::follow_page_link_notices`).
const PAGE_LINK_CHANGED_LINE: &str = "followed the page link change=";
const REOPEN_ASKED_LINE: &str = "Asking the extension to bring the page back";
const PAGE_CAME_BACK_LINE: &str = "The page came back; the session continues";
const PAGE_NOT_BACK_LINE: &str = "The page could not be brought back";

/// Сколько окно стоит на паузе до закрытия вкладки: часы плеера успевают сказать «пауза».
const PAUSE_SETTLE: Duration = Duration::from_secs(1);
/// Сколько сирота ждёт на паузе до play зрителя: дольше одного тика часов, чтобы ожидание было настоящим.
const ORPHAN_PAUSE: Duration = Duration::from_secs(2);
/// Сколько ждём, что плеер узнал об уходе страницы (порт вкладки умирает сразу).
const PAGE_LEFT_TIMEOUT: Duration = Duration::from_secs(10);
/// Сколько ждём просьбу плеера после play (MPRIS → окно → просьба — доли секунды).
const REOPEN_ASKED_TIMEOUT: Duration = Duration::from_secs(10);
/// Сколько ждём новую вкладку после просьбы.
const REOPENED_TAB_TIMEOUT: Duration = Duration::from_secs(15);
/// Сколько ждём «страница вернулась»: загрузка страницы стенда и тень — секунды; срок экстеншена 35 с.
const PAGE_CAME_BACK_TIMEOUT: Duration = Duration::from_secs(40);
/// Сколько ждём отказ: срок экстеншена (`PAGE_REOPEN_DEADLINE_MS`, 35 с) и запас.
const PAGE_NOT_BACK_TIMEOUT: Duration = Duration::from_secs(50);
/// Через сколько после «вернулась» читать позицию: окно успевает применить начало нового поколения.
const POSITION_SETTLE: Duration = Duration::from_secs(1);
/// Сколько новых кадров рендера после возврата доказывают, что поток идёт (сводка — раз в 10 с).
const FRAMES_AFTER_RETURN: u64 = 50;
const FRAMES_AFTER_RETURN_TIMEOUT: Duration = Duration::from_secs(25);
/// Сколько вернувшийся документ живёт под тенью, прежде чем судить его декодер (порог — 6 с строк).
const SHADOW_OBSERVE: Duration = Duration::from_secs(8);
/// Сколько окно может закрываться после `Quit`.
const QUIT_WINDOW_GONE_TIMEOUT: Duration = Duration::from_secs(20);
/// Сколько окно досматривает скачанное после отказа: остаток ролика 30 с и запас медленной машины.
const DRAIN_WINDOW_GONE_TIMEOUT: Duration = Duration::from_secs(60);
/// Сколько ждём, что экстеншен закрыл открытую им вкладку.
const REOPENED_TAB_CLOSED_TIMEOUT: Duration = Duration::from_secs(10);
/// Шаг опроса вкладок.
const TABS_POLL: Duration = Duration::from_millis(250);

/// Что делает заново открытая страница (параметр `second` адреса страницы).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecondLoad {
    /// Тот же фильм, как обычный сайт после перезагрузки.
    SameFilm,
    /// Другой ролик (одностраничник): вернуть нельзя.
    OtherVideo,
    /// Видео не запускает (сайт ждёт клика).
    Idle,
}

/// Чем играет фильм первый документ (параметр `source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilmSource {
    Mse,
    File,
}

/// Что проверяет строка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabReturnRow {
    Returned(FilmSource),
    RefusedOtherVideo,
    WindowClosedWhileReopening,
}

impl TabReturnRow {
    fn name(self) -> &'static str {
        match self {
            Self::Returned(FilmSource::Mse) => "return-mse",
            Self::Returned(FilmSource::File) => "return-file",
            Self::RefusedOtherVideo => "refused-other-video",
            Self::WindowClosedWhileReopening => "window-closed-while-reopening",
        }
    }

    fn source(self) -> FilmSource {
        match self {
            Self::Returned(source) => source,
            Self::RefusedOtherVideo | Self::WindowClosedWhileReopening => FilmSource::Mse,
        }
    }

    fn second_load(self) -> SecondLoad {
        match self {
            Self::Returned(_) => SecondLoad::SameFilm,
            Self::RefusedOtherVideo => SecondLoad::OtherVideo,
            Self::WindowClosedWhileReopening => SecondLoad::Idle,
        }
    }

    /// Хвост адреса страницы: режим передачи, форма фильма, поведение второго документа.
    fn page_query(self) -> String {
        let source = match self.source() {
            FilmSource::Mse => "mse",
            FilmSource::File => "file",
        };
        let second = match self.second_load() {
            SecondLoad::SameFilm => "same",
            SecondLoad::OtherVideo => "other",
            SecondLoad::Idle => "idle",
        };
        format!("{HANDOFF_MODE_QUERY}&source={source}&second={second}")
    }
}

const ROWS: &[TabReturnRow] = &[
    TabReturnRow::Returned(FilmSource::Mse),
    TabReturnRow::Returned(FilmSource::File),
    TabReturnRow::RefusedOtherVideo,
    TabReturnRow::WindowClosedWhileReopening,
];

fn subject(row: TabReturnRow) -> String {
    format!("{} x {TAB_RETURN_PAGE}", row.name())
}

/// Идентификаторы страниц сценария (для проверки `--page`).
pub(super) fn page_ids() -> Vec<&'static str> {
    vec![TAB_RETURN_PAGE]
}

/// Все строки одного браузера, каждая в свежем окружении.
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    if !filter.admits(TAB_RETURN_PAGE) {
        return Vec::new();
    }
    let subject_filter = std::env::var(SUBJECT_FILTER_VARIABLE).ok();
    ROWS.iter()
        .filter(|row| {
            subject_filter
                .as_deref()
                .is_none_or(|part| subject(**row).contains(part))
        })
        .map(|row| {
            let started = Instant::now();
            let verdict =
                stand.with_environment(|environment| drive(stand, environment, browser, *row));
            StepResult::new(
                "tab-return",
                browser,
                subject(*row),
                verdict,
                started.elapsed(),
            )
        })
        .collect()
}

/// Провалы и заметки строки.
#[derive(Default)]
struct RowNotes {
    failures: Vec<String>,
    notes: Vec<String>,
}

impl RowNotes {
    fn record(&mut self, result: Result<String, String>) -> bool {
        match result {
            Ok(note) => {
                self.notes.push(note);
                true
            }
            Err(problem) => {
                self.failures.push(problem);
                false
            }
        }
    }
}

/// Что сеанс знал в миг просьбы вернуть страницу.
struct AskedReturn {
    asked_s: f64,
    asked_at: Instant,
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser: BrowserKind,
    row: TabReturnRow,
) -> Result<Verdict, ScenarioError> {
    let mut run = HandoffRun::open_with_query(
        stand,
        environment,
        browser,
        TAB_RETURN_PAGE,
        "tab-return",
        ExtensionMode::Real,
        &row.page_query(),
    )?;
    if !matches!(run.wait_page_ready(), PageReadiness::Ready) {
        return finish_early(run, "the page never became ready to hand off".to_owned());
    }
    match run.hand_off() {
        Ok(HandOffOutcome::Started) => {}
        other => return finish_early(run, format!("hand_off_tab: {other:?}")),
    }
    // По этой команде второй документ узнает себя (см. шапку страницы).
    run.tell_page("handed-off");
    let control = PlayerControl::on_bus(environment.bus_address()?);
    if let Err(message) = start_of_session(&mut run, &control)? {
        return finish_early(run, message);
    }
    let mut row_notes = RowNotes::default();
    let reopened_prefix = format!("{}?run={}", run.page_url_prefix(), run.run);
    match close_tab_and_ask_return(&mut run, &control, &reopened_prefix) {
        Ok(asked) => match row {
            TabReturnRow::Returned(_) => follow_return(&mut run, &control, &asked, &mut row_notes),
            TabReturnRow::RefusedOtherVideo => {
                follow_refusal(&mut run, &control, &reopened_prefix, &mut row_notes)
            }
            TabReturnRow::WindowClosedWhileReopening => {
                follow_window_closed(&mut run, &control, &reopened_prefix, &mut row_notes)
            }
        },
        Err(problem) => row_notes.failures.push(problem),
    }
    finish(run, row_notes)
}

/// Окно на паузе → вкладку закрыли → сирота ждёт → play в окне → просьба → новая вкладка той же страницы.
fn close_tab_and_ask_return(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    reopened_prefix: &str,
) -> Result<AskedReturn, String> {
    control.pause().map_err(|error| format!("pause: {error}"))?;
    std::thread::sleep(PAUSE_SETTLE);
    // Служебная страница — до закрытия: Firefox, закрыв последнюю вкладку, закрыл бы весь браузер.
    run.open_service_page()
        .map_err(|error| format!("open the service page: {error}"))?;
    run.player_log
        .skip_history()
        .map_err(|error| error.to_string())?;
    run.close_page_tab()
        .map_err(|error| format!("close the source tab: {error}"))?;
    let left = run
        .player_log
        .wait_for_line(PAGE_LINK_CHANGED_LINE, PAGE_LEFT_TIMEOUT)
        .map_err(|error| format!("the player never heard that the page left: {error}"))?;
    if !left.contains("change=Left") {
        return Err(format!("the page link changed, but not to «left»: {left}"));
    }
    std::thread::sleep(ORPHAN_PAUSE);
    control.play().map_err(|error| format!("play: {error}"))?;
    run.player_log
        .wait_for_line(REOPEN_ASKED_LINE, REOPEN_ASKED_TIMEOUT)
        .map_err(|error| {
            format!("play in the window did not ask to bring the page back: {error}")
        })?;
    let asked_at = Instant::now();
    let asked_s = position_seconds(control)?;
    wait_for_tab(
        run,
        reopened_prefix,
        TabPresence::Open,
        REOPENED_TAB_TIMEOUT,
    )
    .map_err(|problem| format!("the page was not opened again: {problem}"))?;
    Ok(AskedReturn { asked_s, asked_at })
}

/// Страница вернулась: тот же сеанс, та же секунда, кадры дальше, браузер под тенью почти не декодирует.
fn follow_return(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    asked: &AskedReturn,
    notes: &mut RowNotes,
) {
    if let Err(error) = run
        .player_log
        .wait_for_line(PAGE_CAME_BACK_LINE, PAGE_CAME_BACK_TIMEOUT)
    {
        notes
            .failures
            .push(format!("the page did not come back: {error}"));
        return;
    }
    let waited_s = asked.asked_at.elapsed().as_secs_f64();
    notes
        .notes
        .push(format!("came back {waited_s:.1} s after the request"));
    std::thread::sleep(POSITION_SETTLE);
    let position = position_seconds(control).and_then(|back_s| {
        judge_return_position(
            asked.asked_s,
            back_s,
            waited_s + POSITION_SETTLE.as_secs_f64(),
        )
    });
    notes.record(position);
    let frames = run
        .player_log
        .wait_for_frame_growth(FRAMES_AFTER_RETURN, FRAMES_AFTER_RETURN_TIMEOUT)
        .map(|growth| {
            format!(
                "{} new frames in render after the return",
                growth.frames_gained()
            )
        })
        .map_err(|error| format!("no new frames in render after the return: {error}"));
    notes.record(frames);
    std::thread::sleep(SHADOW_OBSERVE);
    let states: Vec<_> = run
        .page_log_lines("tab-return: state")
        .iter()
        .filter_map(|line| parse_state(line))
        .collect();
    notes.record(judge_second_document_decoding(&states));
    let alive = control
        .wait_until_controllable(Duration::from_secs(5))
        .map(|()| "the window still plays the same session".to_owned())
        .map_err(|error| format!("the window is gone after the return: {error}"));
    notes.record(alive);
    let quit = control
        .quit_window()
        .map_err(|error| error.to_string())
        .and_then(|()| {
            control
                .wait_until_gone(QUIT_WINDOW_GONE_TIMEOUT)
                .map_err(|error| error.to_string())
        })
        .map(|()| "the window closed on Quit".to_owned());
    notes.record(quit);
}

/// Одностраничник: отказ «не то видео», досмотр скачанного, окно закрыто, открытая вкладка закрыта.
fn follow_refusal(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    reopened_prefix: &str,
    notes: &mut RowNotes,
) {
    let refusal = run
        .player_log
        .wait_for_line(PAGE_NOT_BACK_LINE, PAGE_NOT_BACK_TIMEOUT)
        .map_err(|error| format!("no refusal from the extension: {error}"))
        .and_then(|line| {
            if line.contains("class=not_the_same_video") {
                Ok("refused: not_the_same_video".to_owned())
            } else {
                Err(format!("the refusal names another reason: {line}"))
            }
        });
    notes.record(refusal);
    let closed = control
        .wait_until_gone(DRAIN_WINDOW_GONE_TIMEOUT)
        .map(|()| "the window watched the rest and closed".to_owned())
        .map_err(|error| format!("the window did not close after the refusal: {error}"));
    notes.record(closed);
    notes.record(
        wait_for_tab(
            run,
            reopened_prefix,
            TabPresence::Closed,
            REOPENED_TAB_CLOSED_TIMEOUT,
        )
        .map(|()| "the reopened tab was closed".to_owned()),
    );
}

/// Окно закрыто, пока страница открывалась: открытие отменено, открытая вкладка закрыта, возврата не было.
fn follow_window_closed(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    reopened_prefix: &str,
    notes: &mut RowNotes,
) {
    let quit = control
        .quit_window()
        .map_err(|error| error.to_string())
        .and_then(|()| {
            control
                .wait_until_gone(QUIT_WINDOW_GONE_TIMEOUT)
                .map_err(|error| error.to_string())
        })
        .map(|()| "the window closed while the page was reopening".to_owned());
    notes.record(quit);
    notes.record(
        wait_for_tab(
            run,
            reopened_prefix,
            TabPresence::Closed,
            REOPENED_TAB_CLOSED_TIMEOUT,
        )
        .map(|()| "the reopened tab was closed".to_owned()),
    );
    if !run
        .player_log
        .forbidden_lines(&[PAGE_CAME_BACK_LINE])
        .is_empty()
    {
        notes
            .failures
            .push("the page came back although the window was closed".to_owned());
    }
}

/// Какой вкладки ждём.
#[derive(Debug, Clone, Copy)]
enum TabPresence {
    Open,
    Closed,
}

/// Ждёт, пока вкладка с адресом `prefix` появится или исчезнет (так её видит экстеншен).
fn wait_for_tab(
    run: &mut HandoffRun<'_>,
    prefix: &str,
    wanted: TabPresence,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let tabs = read_tabs(run.browser.as_mut());
        if let Ok(tabs) = &tabs {
            let open = tabs.iter().any(|tab| tab.url.starts_with(prefix));
            if matches!(
                (wanted, open),
                (TabPresence::Open, true) | (TabPresence::Closed, false)
            ) {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no {wanted:?} tab for {prefix} within {timeout:?}; last read: {tabs:?}"
            ));
        }
        std::thread::sleep(TABS_POLL);
    }
}

/// Позиция окна плеера по MPRIS, секунды.
fn position_seconds(control: &PlayerControl) -> Result<f64, String> {
    control
        .position_micros()
        .map(|micros| micros as f64 / 1_000_000.0)
        .map_err(|error| format!("the window position: {error}"))
}

/// До сеанса не дошло: журнал страницы в текст, браузер закрыт.
fn finish_early(run: HandoffRun<'_>, message: String) -> Result<Verdict, ScenarioError> {
    let log = format_browser_log(&run.take_page_log());
    run.browser.shutdown()?;
    Ok(Verdict::fail(format!("{message}\n{log}")))
}

/// Итог строки: провалы шагов, посредник и запрещённые строки плеера; браузер закрыт.
fn finish(mut run: HandoffRun<'_>, mut row_notes: RowNotes) -> Result<Verdict, ScenarioError> {
    let page_log = format_browser_log(&run.take_page_log());
    let forbidden = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    row_notes.failures.extend(host_failures(&host));
    row_notes.failures.extend(
        unexpected_player_lines(&forbidden).map(|line| format!("forbidden player line: {line}")),
    );
    let summary = row_notes.notes.join("; ");
    if row_notes.failures.is_empty() {
        Ok(Verdict::pass(summary))
    } else {
        Ok(Verdict::fail(format!(
            "{}\n{summary}\n{page_log}",
            row_notes.failures.join("; ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::page_catalog::PAGE_SCENARIOS;

    #[test]
    fn the_page_is_a_stand_page_and_rows_cover_the_assignment() {
        assert!(PAGE_SCENARIOS.iter().any(|page| page.id == TAB_RETURN_PAGE));
        for wanted in [
            TabReturnRow::Returned(FilmSource::Mse),
            TabReturnRow::Returned(FilmSource::File),
            TabReturnRow::RefusedOtherVideo,
            TabReturnRow::WindowClosedWhileReopening,
        ] {
            assert!(ROWS.contains(&wanted), "{wanted:?}");
        }
        let subjects: std::collections::HashSet<String> =
            ROWS.iter().map(|row| subject(*row)).collect();
        assert_eq!(subjects.len(), ROWS.len());
    }

    #[test]
    fn the_page_query_tells_the_page_what_its_second_load_does() {
        assert_eq!(
            TabReturnRow::Returned(FilmSource::File).page_query(),
            format!("{HANDOFF_MODE_QUERY}&source=file&second=same")
        );
        assert!(
            TabReturnRow::RefusedOtherVideo
                .page_query()
                .ends_with("&second=other")
        );
        assert!(
            TabReturnRow::WindowClosedWhileReopening
                .page_query()
                .ends_with("&second=idle")
        );
    }

    /// Параметры, которые робот шлёт странице, страница объявляет: иначе стенд упал бы только вживую.
    #[test]
    fn the_page_understands_every_query_value_and_writes_the_lines_the_robot_reads() {
        let page = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("site/pages/tab-return.html"),
        )
        .expect("page");
        for needle in [
            "'idle'",
            "'other'",
            "'file'",
            "'handed-off'",
            "state doc=",
            "handoff-ready",
        ] {
            assert!(
                page.contains(needle),
                "tab-return.html does not mention {needle}"
            );
        }
    }
}
