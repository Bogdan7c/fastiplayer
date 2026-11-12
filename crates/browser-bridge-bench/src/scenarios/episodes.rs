//! Сценарий `episodes` (сессия 16a, решения 303-309): пауза между сериями и уход одностраничника,
//! через настоящий экстеншен и плеер.
//!
//! Что доказывает каждая строка (в Chromium и в Firefox):
//! 1. `delayed-next`: серия 1 кончилась, страница сняла источник и поставила серию 2 только через
//!    ~20 с (дольше прежнего допуска 5 с). Плеер стартует с запомненным полным экраном; окно за
//!    это время не пересоздаётся и не выходит из полного экрана, слова «ждём следующую серию»
//!    снимаются появлением серии 2, кадры идут, заголовок страницы дошёл до окна, сеанс не кончался;
//! 2. `delayed-next-hidden`: то же, но вкладку страницы стенд спрятал (открыл поверх пустую);
//! 3. `no-next-episode`: серии 2 нет вовсе — через ~60 с «The page didn't start the next episode»;
//! 4. `instant-next`: порядок PlayerJS Tortuga (16a3) — серия 2 тем же элементом сразу, `play()` и
//!    перемотка на 0 раньше `sourceopen`; плеер не перезапускает доигравшую серию 1;
//! 5. `spa-away`: одностраничник сменил адрес (`pushState`), снял источник и ничего не поставил —
//!    плеер говорит «вкладка закрыта» (`tab_gone`), а не «страница забрала видео».
//!
//! Серию 1 до конца доводит робот: ставит позицию плеера за 8 с до конца (MPRIS `Seek`), страница
//! следует за перемоткой плеера, как за перемоткой зрителя. Страницы: `episode-gap` (строки 1-4) и
//! `episode-spa-away` (строка 5). Приговоры — чистые функции в [`judge`].

mod judge;
mod replay_guard;

use std::time::{Duration, Instant};

use self::judge::{
    DRAIN_FINISHED_MARKER, END_NOTICE_MARKER, EpisodeEvidence, Expectation, FULLSCREEN_MARKER,
    ObservedEnd, PAGE_TITLE_MARKER, PLAYER_PROCESS_MARKER, PageTab, PlayerJournal,
    VIEWER_NOTICE_MARKER, WINDOW_CREATED_MARKER, judge,
};
use self::replay_guard::EPISODE_CHANGE_MARKERS;
use super::capture::judge_early_report;
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
use super::series::wait_for_lines_anywhere;
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::{BenchEnvironment, PlayerConfigProfile};
use crate::observe::{EndNotice, ObserveError, PlayerLog, read_host_evidence};
use crate::player_control::PlayerControl;

/// Страница с паузой между сериями (строки 1-3).
const GAP_PAGE: &str = "episode-gap";
/// Страница одностраничника, который уходит (строка 4).
const SPA_AWAY_PAGE: &str = "episode-spa-away";

/// Пауза между концом серии 1 и серией 2 на странице, мс: дольше прежних 5 с, короче 60 с экстеншена.
const NEXT_EPISODE_DELAY_MS: u64 = 20_000;
/// Длина ролика серии (`h264-aac` каталога стенда).
const EPISODE_LENGTH: Duration = Duration::from_secs(30);
/// Сколько секунд серии остаётся после перемотки робота: плеер дойдёт до конца сам.
const EPISODE_TAIL: Duration = Duration::from_secs(8);

/// Сколько ждём конца серии 1 на странице после перемотки (хвост + запас на медленную машину).
const EPISODE_END_TIMEOUT: Duration = Duration::from_secs(60);
/// Сколько ждём серию 2 на странице: пауза и запас (в скрытой вкладке таймеры тикают реже).
const NEXT_EPISODE_ATTACH_TIMEOUT: Duration =
    Duration::from_secs(NEXT_EPISODE_DELAY_MS / 1000 + 40);
/// Сколько ждём слов окна «серия 2 показана».
const LINE_TIMEOUT: Duration = Duration::from_secs(45);
/// Насколько кадры должны вырасти между двумя сводками после серии 2.
const MIN_FRAMES_GAINED: u64 = 30;
/// Сколько ждём роста кадров (сводки раз в 10 с).
const GROWTH_TIMEOUT: Duration = Duration::from_secs(45);
/// Сколько ждём конца сеанса «серия 2 не пришла» (60 с экстеншена и запас).
const NEXT_EPISODE_NOTICE_TIMEOUT: Duration = Duration::from_secs(100);
/// Сколько ждём сообщения о том, что вкладка ушла (доигрывание скачанного и запас).
const TAB_GONE_NOTICE_TIMEOUT: Duration = Duration::from_secs(60);
/// Сколько окно может показывать сообщение до закрытия.
const WINDOW_GONE_TIMEOUT: Duration = Duration::from_secs(30);
/// Пустая вкладка, которой стенд закрывает страницу.
const BLANK_TAB_URL: &str = "about:blank";

/// Метки журнала страницы `episode-gap` (договор со страницей).
const PAGE_EPISODE_ENDED: &str = "episode-1-ended";
const PAGE_SOURCE_REMOVED: &str = "episode-1-source-removed";
const PAGE_NEXT_ATTACHED: &str = "episode-2-attached";
/// Действие `episode-spa-away` и его метка.
const SPA_AWAY_HOOK: &str = "hook:spa-away";

/// Строка сценария: имя в таблице результатов, страница, что она обязана доказать.
#[derive(Debug, Clone, Copy)]
struct EpisodeRow {
    name: &'static str,
    page_id: &'static str,
    expectation: Expectation,
}

/// Строки сценария в порядке прогона.
const ROWS: [EpisodeRow; 5] = [
    EpisodeRow {
        name: "delayed-next",
        page_id: GAP_PAGE,
        expectation: Expectation::DelayedNext(PageTab::Left),
    },
    EpisodeRow {
        name: "delayed-next-hidden",
        page_id: GAP_PAGE,
        expectation: Expectation::DelayedNext(PageTab::Hidden),
    },
    EpisodeRow {
        name: "no-next-episode",
        page_id: GAP_PAGE,
        expectation: Expectation::NoNextEpisode,
    },
    EpisodeRow {
        name: "instant-next",
        page_id: GAP_PAGE,
        expectation: Expectation::InstantNext,
    },
    EpisodeRow {
        name: "spa-away",
        page_id: SPA_AWAY_PAGE,
        expectation: Expectation::SpaAway,
    },
];

impl EpisodeRow {
    /// Хвост адреса страницы: режим передачи и параметры строки.
    fn page_query(&self) -> String {
        let row_query = match self.expectation {
            Expectation::DelayedNext(PageTab::Left) => {
                format!("&next=delayed&delay_ms={NEXT_EPISODE_DELAY_MS}")
            }
            Expectation::DelayedNext(PageTab::Hidden) => {
                format!("&next=delayed&delay_ms={NEXT_EPISODE_DELAY_MS}&hidden=1")
            }
            Expectation::NoNextEpisode => "&next=never".to_owned(),
            Expectation::InstantNext => "&next=instant".to_owned(),
            Expectation::SpaAway => String::new(),
        };
        format!("{HANDOFF_MODE_QUERY}{row_query}")
    }

    /// Профиль настроек плеера: строки с серией 2 стартуют с запомненным полным экраном.
    fn player_config(&self, bench_default: PlayerConfigProfile) -> PlayerConfigProfile {
        match self.expectation {
            Expectation::DelayedNext(_) | Expectation::InstantNext => {
                PlayerConfigProfile::RememberedFullscreen
            }
            Expectation::NoNextEpisode | Expectation::SpaAway => bench_default,
        }
    }
}

/// Идентификаторы страниц сценария (для проверки `--page` в командной строке).
pub(super) fn page_ids() -> Vec<&'static str> {
    vec![GAP_PAGE, SPA_AWAY_PAGE]
}

/// Все строки одного браузера (каждая в свежем окружении), с учётом фильтра по страницам.
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    ROWS.iter()
        .filter(|row| filter.admits(row.page_id))
        .map(|row| run_row(stand, browser, *row))
        .collect()
}

fn run_row(stand: &Stand, browser: BrowserKind, row: EpisodeRow) -> StepResult {
    let started = Instant::now();
    let profile = row.player_config(stand.settings.player_config);
    let verdict = stand.with_environment_for_player_config(profile, |environment| {
        drive(stand, environment, browser, row)
    });
    StepResult::new("episodes", browser, row.name, verdict, started.elapsed())
}

/// Открыть страницу, дождаться готовности и отдать вкладку. `Err` — готовый вердикт провала.
fn open_and_hand_off<'a>(
    stand: &'a Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    row: &EpisodeRow,
) -> Result<Result<HandoffRun<'a>, Verdict>, ScenarioError> {
    let mut run = HandoffRun::open_with_query(
        stand,
        environment,
        browser_kind,
        row.page_id,
        "episodes",
        ExtensionMode::Real,
        &row.page_query(),
    )?;
    match run.wait_page_ready() {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            run.browser.shutdown()?;
            return Ok(Err(judge_early_report(row.page_id, browser_kind, &report)));
        }
        PageReadiness::TimedOut => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Err(Verdict::fail(format!(
                "the page never became ready for the hand-off\n{log}"
            ))));
        }
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
    run.tell_page("handed-off");
    Ok(Ok(run))
}

/// Что стенд увидел после конца серии 1 (то, что зависит от строки).
#[derive(Debug, Default)]
struct RowOutcome {
    end: Option<ObservedEnd>,
    window_closed: Option<Result<(), String>>,
    /// Сообщения о конце сеанса, замеченные до сворачивания страницы стендом.
    end_notices_while_running: Vec<String>,
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    row: EpisodeRow,
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
        Ok(()) => {
            outcome = walk_row(&mut run, &control, &row, &mut failed_steps)?;
        }
        Err(reason) => failed_steps.push(format!("hand-off: {reason}")),
    }
    let journal = read_journal(&mut run.player_log)?;
    let page_ended_lines = run.page_log_lines(PAGE_EPISODE_ENDED);
    let drain_lines = run
        .player_log
        .snapshot_forbidden(&[DRAIN_FINISHED_MARKER])?;
    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let forbidden_player_lines = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    Ok(judge(
        row.page_id,
        row.expectation,
        &EpisodeEvidence {
            failed_steps,
            journal,
            end_notices_while_running: outcome.end_notices_while_running,
            end: outcome.end,
            window_closed: outcome.window_closed,
            page_ended_lines,
            drain_lines,
            host: read_host_evidence(&exit.stderr_log)?,
            page_report,
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
        fullscreen_changes: player_log.snapshot_lines_containing(FULLSCREEN_MARKER)?,
        viewer_notices: player_log.snapshot_lines_containing(VIEWER_NOTICE_MARKER)?,
        page_titles: player_log.snapshot_lines_containing(PAGE_TITLE_MARKER)?,
        episode_change: player_log.snapshot_forbidden(&EPISODE_CHANGE_MARKERS)?,
    })
}

/// Шаги строки после передачи. Провал шага пишется в `failed_steps`; ошибка стенда — наружу.
fn walk_row(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    row: &EpisodeRow,
    failed_steps: &mut Vec<String>,
) -> Result<RowOutcome, ScenarioError> {
    let mut outcome = RowOutcome::default();
    match row.expectation {
        Expectation::DelayedNext(tab) => {
            if tab == PageTab::Hidden
                && let Err(reason) = hide_page_tab(run)
            {
                failed_steps.push(format!("hide-page-tab: {reason}"));
                return Ok(outcome);
            }
            if let Err(reason) = walk_delayed_next(run, control) {
                failed_steps.push(reason);
            }
            outcome.end_notices_while_running = run
                .player_log
                .snapshot_lines_containing(END_NOTICE_MARKER)?;
        }
        Expectation::InstantNext => {
            if let Err(reason) = walk_instant_next(run, control) {
                failed_steps.push(reason);
            }
            outcome.end_notices_while_running = run
                .player_log
                .snapshot_lines_containing(END_NOTICE_MARKER)?;
        }
        Expectation::NoNextEpisode => match carry_episode_to_its_end(run, control) {
            Ok(()) => match run.wait_page_log_line(PAGE_SOURCE_REMOVED, EPISODE_END_TIMEOUT) {
                Ok(_) => {
                    // Срок «не началась» отсчитывается от снятия источника страницей.
                    let removed_at = Instant::now();
                    outcome.end = Some(observe_end(run, NEXT_EPISODE_NOTICE_TIMEOUT, removed_at));
                    outcome.window_closed = Some(wait_window_gone(control));
                }
                Err(reason) => failed_steps.push(format!("episode-1-source-removed: {reason}")),
            },
            Err(reason) => failed_steps.push(reason),
        },
        Expectation::SpaAway => {
            let started_at = Instant::now();
            run.tell_page(SPA_AWAY_HOOK);
            outcome.end = Some(observe_end(run, TAB_GONE_NOTICE_TIMEOUT, started_at));
            outcome.window_closed = Some(wait_window_gone(control));
            if let Err(reason) = run.wait_ack(SPA_AWAY_HOOK, ACK_TIMEOUT) {
                failed_steps.push(format!("page-finished-leaving: {reason}"));
            }
        }
    }
    Ok(outcome)
}

/// Серия 1 доиграна, серия 2 пришла после паузы, окно показало её, кадры растут.
fn walk_delayed_next(run: &mut HandoffRun<'_>, control: &PlayerControl) -> Result<(), String> {
    carry_episode_to_its_end(run, control)?;
    run.wait_page_log_line(PAGE_NEXT_ATTACHED, NEXT_EPISODE_ATTACH_TIMEOUT)
        .map_err(|reason| format!("episode-2-attached-on-page: {reason}"))?;
    // Окно сняло «ждём следующую серию»: серия 2 показана. Кадры считаем уже после этого.
    wait_for_lines_anywhere(
        &mut run.player_log,
        "the waiting notice is cleared",
        1,
        LINE_TIMEOUT,
    )
    .map_err(|reason| format!("window-shows-episode-2: {reason}"))?;
    run.player_log
        .skip_history()
        .map_err(|error| format!("frames-after-episode-2: {error}"))?;
    run.player_log
        .wait_for_frame_growth(MIN_FRAMES_GAINED, GROWTH_TIMEOUT)
        .map_err(|error| format!("frames-after-episode-2: {error}"))?;
    Ok(())
}

/// Серия 1 доиграна, серия 2 тем же элементом сразу (16a3): плеер загрузил её поколение, кадры растут.
fn walk_instant_next(run: &mut HandoffRun<'_>, control: &PlayerControl) -> Result<(), String> {
    carry_episode_to_its_end(run, control)?;
    run.wait_page_log_line(PAGE_NEXT_ATTACHED, NEXT_EPISODE_ATTACH_TIMEOUT)
        .map_err(|reason| format!("episode-2-attached-on-page: {reason}"))?;
    wait_for_lines_anywhere(
        &mut run.player_log,
        "Browser stream generation loaded generation=2",
        1,
        LINE_TIMEOUT,
    )
    .map_err(|reason| format!("player-loads-episode-2: {reason}"))?;
    run.player_log
        .skip_history()
        .map_err(|error| format!("frames-after-episode-2: {error}"))?;
    run.player_log
        .wait_for_frame_growth(MIN_FRAMES_GAINED, GROWTH_TIMEOUT)
        .map_err(|error| format!("frames-after-episode-2: {error}"))?;
    Ok(())
}

/// Ставит позицию плеера за [`EPISODE_TAIL`] до конца серии и ждёт, что страница увидела конец.
fn carry_episode_to_its_end(run: &HandoffRun<'_>, control: &PlayerControl) -> Result<(), String> {
    let position_micros = control
        .position_micros()
        .map_err(|error| format!("read-player-position: {error}"))?;
    let target = EPISODE_LENGTH.saturating_sub(EPISODE_TAIL);
    let target_micros =
        i64::try_from(target.as_micros()).map_err(|error| format!("seek-target: {error}"))?;
    // Если плеер уже дальше цели (медленная машина), перематывать не нужно: до конца и так близко.
    let offset_micros = target_micros.saturating_sub(position_micros);
    if offset_micros > 0 {
        control
            .seek_by(offset_micros)
            .map_err(|error| format!("seek-near-the-end: {error}"))?;
    }
    run.wait_page_log_line(PAGE_EPISODE_ENDED, EPISODE_END_TIMEOUT)
        .map(|_| ())
        .map_err(|reason| format!("episode-1-ended-on-page: {reason}"))
}

/// Прячет вкладку страницы: поверх открывается пустая и становится активной.
fn hide_page_tab(run: &mut HandoffRun<'_>) -> Result<(), String> {
    let blank = run
        .browser
        .open_tab(BLANK_TAB_URL)
        .map_err(|error| error.to_string())?;
    run.browser
        .activate_tab(&blank)
        .map_err(|error| error.to_string())
}

/// Ждёт сообщение о конце сеанса и считает, через сколько оно пришло после `started_at`.
fn observe_end(run: &mut HandoffRun<'_>, timeout: Duration, started_at: Instant) -> ObservedEnd {
    run.player_log
        .wait_for_end_notice(timeout)
        .map(|found: EndNotice| (found, started_at.elapsed()))
        .map_err(|error| format!("no end notice from the player: {error}"))
}

/// Ждёт, пока окно плеера уйдёт с шины.
fn wait_window_gone(control: &PlayerControl) -> Result<(), String> {
    control
        .wait_until_gone(WINDOW_GONE_TIMEOUT)
        .map_err(|error| format!("the player window did not close: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::page_catalog::PAGE_SCENARIOS;

    fn page_source(page_id: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("site/pages/{page_id}.html")),
        )
        .expect("page file")
    }

    #[test]
    fn rows_cover_the_five_promised_cases_on_known_pages() {
        let names: Vec<&str> = ROWS.iter().map(|row| row.name).collect();
        assert_eq!(
            names,
            [
                "delayed-next",
                "delayed-next-hidden",
                "no-next-episode",
                "instant-next",
                "spa-away"
            ]
        );
        for page_id in page_ids() {
            assert!(
                PAGE_SCENARIOS.iter().any(|scenario| scenario.id == page_id),
                "{page_id} is not in the page catalog"
            );
        }
    }

    #[test]
    fn row_queries_carry_the_gap_the_hidden_flag_and_the_never_mode() {
        let query = |name: &str| {
            ROWS.iter()
                .find(|row| row.name == name)
                .expect("known row")
                .page_query()
        };
        assert_eq!(
            query("delayed-next"),
            "&mode=handoff&next=delayed&delay_ms=20000"
        );
        assert_eq!(
            query("delayed-next-hidden"),
            "&mode=handoff&next=delayed&delay_ms=20000&hidden=1"
        );
        assert_eq!(query("no-next-episode"), "&mode=handoff&next=never");
        assert_eq!(query("instant-next"), "&mode=handoff&next=instant");
        assert_eq!(query("spa-away"), "&mode=handoff");
        // Пауза строго между прежним допуском (5 с) и ожиданием экстеншена (60 с).
        assert!((5_000..60_000).contains(&NEXT_EPISODE_DELAY_MS));
    }

    #[test]
    fn only_rows_with_a_second_episode_start_in_remembered_fullscreen() {
        let bench_default = PlayerConfigProfile::Clean;
        let profiles: Vec<PlayerConfigProfile> = ROWS
            .iter()
            .map(|row| row.player_config(bench_default))
            .collect();
        assert_eq!(
            profiles,
            [
                PlayerConfigProfile::RememberedFullscreen,
                PlayerConfigProfile::RememberedFullscreen,
                PlayerConfigProfile::Clean,
                PlayerConfigProfile::RememberedFullscreen,
                PlayerConfigProfile::Clean,
            ]
        );
    }

    #[test]
    fn the_page_filter_picks_rows_by_page() {
        let only_spa = PageFilter::only([SPA_AWAY_PAGE.to_owned()]);
        let admitted: Vec<&str> = ROWS
            .iter()
            .filter(|row| only_spa.admits(row.page_id))
            .map(|row| row.name)
            .collect();
        assert_eq!(admitted, ["spa-away"]);
    }

    #[test]
    fn the_pages_write_the_marks_the_robot_waits_for_and_declare_the_hook() {
        let gap = page_source(GAP_PAGE);
        for fragment in [
            PAGE_EPISODE_ENDED,
            PAGE_SOURCE_REMOVED,
            PAGE_NEXT_ATTACHED,
            "visibility=",
            "document.title",
            "get('next')",
            "get('delay_ms')",
            "get('hidden')",
            "'instant'",
            "playBeforeOpen: true",
        ] {
            assert!(gap.contains(fragment), "{GAP_PAGE} lacks {fragment}");
        }
        let spa = page_source(SPA_AWAY_PAGE);
        let hook_name = SPA_AWAY_HOOK.trim_start_matches("hook:");
        assert!(
            spa.contains(&format!("'{hook_name}'")),
            "{SPA_AWAY_PAGE} lacks the hook"
        );
        assert!(spa.contains("history.pushState"));
    }

    #[test]
    fn the_episode_tail_leaves_time_to_finish_inside_the_asset() {
        assert!(EPISODE_TAIL < EPISODE_LENGTH);
        assert!(EPISODE_LENGTH.saturating_sub(EPISODE_TAIL) > Duration::ZERO);
    }
}
