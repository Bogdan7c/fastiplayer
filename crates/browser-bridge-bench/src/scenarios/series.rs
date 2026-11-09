//! Сценарий `series` (сессия 14b): каркас адаптеров сайтов на странице «сериал» стенда.
//!
//! Страница `series-variants` устроена как сериальный сайт: озвучки — кнопки (адаптер жмёт их
//! кликом), качества и следующая серия — функции плеера страницы (адаптер зовёт их рукой-функцией).
//! Выбор «в нашем меню» делает тестовая переменная плеера (`PlayerTestVariables`): она зовёт то же
//! намерение окна, что и пункт меню ♪ или кнопки качества.
//!
//! Прогон «варианты» (один браузер, одна передача):
//! 1. меню озвучек и качеств и «следующая серия есть» дошли до плеера;
//! 2. выбор 720p → страница открыла поток 720p → окно показало 720p;
//! 3. выбор Dub B → страница открыла озвучку с нуля (как Ashdi) → плеер вернулся на ту же секунду
//!    ± 1 с (решение 280) → кадры растут;
//! 4. MPRIS Next → страница открыла серию 2 в той же озвучке → кадры растут.
//!
//! Прогон «сломанный адаптер» (`&adapter=broken`): плеер страницы бросает, адаптер падает, видео
//! играет обычным перехватом, меню вариантов у плеера нет.
//!
//! Журнал плеера читается курсором, а ожидание кадров листает курсор вперёд. Поэтому события
//! ищутся по ВСЕМУ журналу ([`wait_for_lines_anywhere`]), а курсор нужен только росту кадров.

mod judge;

use std::time::{Duration, Instant};

use self::judge::{BrokenAdapterEvidence, VariantsEvidence, judge_broken_adapter, judge_variants};
use super::capture::{MIN_PRESENTED_FRAMES, judge_early_report};
use super::context::{ScenarioError, Stand};
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, wait_for_session_granted,
};
use super::handoff_run::{ExtensionMode, HANDOFF_MODE_QUERY, HandoffRun, PageReadiness};
use super::outcome::{StepResult, Verdict};
use super::page_filter::PageFilter;
use super::player_commands::FRAMES_TIMEOUT;
use super::run_tweaks::RunTweaks;
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::{BenchEnvironment, PlayerTestVariables};
use crate::observe::{BROWSER_QUALITY_SHOWN_MARKER, PlayerLog, read_host_evidence};
use crate::player_control::PlayerControl;

/// Страница сценария.
pub(super) const SERIES_PAGE: &str = "series-variants";
/// Хвост адреса прогона «сломанный адаптер».
const BROKEN_ADAPTER_QUERY: &str = "&adapter=broken";

/// Выборы «в нашем меню» по очереди: 720p на 2,5 с, Dub B на 6 с (номера пунктов — порядок меню).
const VARIANT_CHOICES: &str = "quality:1@2500,audio:1@6000";
/// Секунда выбора озвучки — та, на которую плеер обязан вернуться.
const DUB_SELECTED_AT: Duration = Duration::from_millis(6_000);

/// Строки журнала плеера (их пишет `app-egui::browser_session`, сессия 14a).
const AUDIO_MENU_LINE: &str = "Page variant menu kind=audio";
const QUALITY_MENU_LINE: &str = "Page variant menu kind=quality";
const NAVIGATION_LINE: &str = "Page navigation changed";
const NEXT_EPISODE_AVAILABLE: &str = "next_episode=true";
const POSITION_CARRIED_LINE: &str = "Position carried to the new generation after a variant switch";
const NEXT_EPISODE_REQUESTED_LINE: &str = "Next episode requested";
/// Общий корень строк меню: в прогоне «сломанный адаптер» их не должно быть ни одной.
const ANY_VARIANT_MENU_LINE: &str = "Page variant menu";

/// Строки журнала страницы: новое поколение после каждого шага (`series-variants.html`).
const QUALITY_GENERATION: &str = "generation cause=quality episode=1 dub=dub-a quality=video-720";
const DUB_GENERATION: &str = "generation cause=dub episode=1 dub=dub-b quality=video-720";
const NEXT_GENERATION: &str = "generation cause=next episode=2 dub=dub-b quality=video-720";
const BROKEN_PLAYER_CALLED: &str = "broken page player was called";

/// Сколько ждём строки плеера и страницы.
const LINE_TIMEOUT: Duration = Duration::from_secs(45);
/// Как часто перечитываем журнал плеера целиком.
const LINE_POLL: Duration = Duration::from_millis(100);
/// Насколько кадры должны вырасти между двумя сводками после шага.
const MIN_FRAMES_GAINED: u64 = 30;
/// Сколько ждём роста кадров (сводки раз в 10 с).
const GROWTH_TIMEOUT: Duration = Duration::from_secs(45);
/// Сколько ждём, пока окно плеера станет управляемым по MPRIS.
const CONTROLLABLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Сколько ещё смотрим после первых кадров «сломанного» прогона: адаптер успевает попробовать много раз.
const BROKEN_ADAPTER_OBSERVATION: Duration = Duration::from_secs(5);

/// Идентификаторы страниц команды (для проверки `--page` в командной строке).
pub(super) fn page_ids() -> Vec<&'static str> {
    vec![SERIES_PAGE]
}

/// Оба прогона, каждый в свежем окружении.
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    if !filter.admits(SERIES_PAGE) {
        return Vec::new();
    }
    let started = Instant::now();
    let choices = PlayerTestVariables::default().with_page_variant_choices(VARIANT_CHOICES);
    let variants = stand.with_environment_for_player_test(choices, |environment| {
        drive_variants(stand, environment, browser)
    });
    let variants_elapsed = started.elapsed();
    let started = Instant::now();
    let broken =
        stand.with_environment(|environment| drive_broken_adapter(stand, environment, browser));
    vec![
        StepResult::new("series", browser, SERIES_PAGE, variants, variants_elapsed),
        StepResult::new(
            "series-broken-adapter",
            browser,
            SERIES_PAGE,
            broken,
            started.elapsed(),
        ),
    ]
}

/// Ждёт, пока во ВСЁМ журнале плеера будет не меньше `count` строк с `needle`; возвращает их.
fn wait_for_lines_anywhere(
    player_log: &mut PlayerLog,
    needle: &str,
    count: usize,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let lines = player_log
            .snapshot_lines_containing(needle)
            .map_err(|error| error.to_string())?;
        if lines.len() >= count {
            return Ok(lines);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{} of {count} player lines '{needle}' in {} s",
                lines.len(),
                timeout.as_secs()
            ));
        }
        std::thread::sleep(LINE_POLL);
    }
}

/// Ждёт строку журнала страницы.
fn wait_for_page_line(run: &HandoffRun<'_>, marker: &str) -> Result<(), String> {
    match run.wait_page_marker(marker, LINE_TIMEOUT) {
        PageReadiness::Ready => Ok(()),
        PageReadiness::ReportedEarly(report) => Err(format!(
            "the page reported '{:?}' before '{marker}'",
            report.status
        )),
        PageReadiness::TimedOut => Err(format!("no page line '{marker}'")),
    }
}

/// Открыть страницу, дождаться готовности и отдать вкладку. `Err` — готовый вердикт провала.
pub(super) fn open_and_hand_off<'a>(
    stand: &'a Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    query: &str,
    tweaks: &RunTweaks,
) -> Result<Result<HandoffRun<'a>, Verdict>, ScenarioError> {
    let mut run = HandoffRun::open_tweaked(
        stand,
        environment,
        browser_kind,
        SERIES_PAGE,
        "series",
        ExtensionMode::Real,
        query,
        tweaks,
    )?;
    match run.wait_page_ready() {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            run.browser.shutdown()?;
            return Ok(Err(judge_early_report(SERIES_PAGE, browser_kind, &report)));
        }
        PageReadiness::TimedOut => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Err(Verdict::fail(format!(
                "the page never became ready\n{log}"
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

fn drive_variants(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
) -> Result<Verdict, ScenarioError> {
    let mut run = match open_and_hand_off(
        stand,
        environment,
        browser_kind,
        HANDOFF_MODE_QUERY,
        &RunTweaks::default(),
    )? {
        Ok(run) => run,
        Err(verdict) => return Ok(verdict),
    };
    let mut failed_steps: Vec<String> = Vec::new();
    let mut position_after_dub = None;
    match wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)? {
        Ok(()) => {
            let control = PlayerControl::on_bus(environment.bus_address()?);
            position_after_dub = walk_variants(&mut run, &control, &mut failed_steps).ok();
        }
        Err(reason) => failed_steps.push(format!("session-granted: {reason}")),
    }
    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let forbidden_player_lines = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    Ok(judge_variants(
        SERIES_PAGE,
        &VariantsEvidence {
            failed_steps,
            dub_selected_at: DUB_SELECTED_AT,
            position_after_dub,
            host: read_host_evidence(&exit.stderr_log)?,
            page_report,
            forbidden_player_lines,
            page_log: format_browser_log(&page_log),
        },
    ))
}

/// Шаги прогона «варианты». Провал шага пишется в `failed_steps`; возвращает позицию плеера
/// сразу после возврата на секунду озвучки.
fn walk_variants(
    run: &mut HandoffRun<'_>,
    control: &PlayerControl,
    failed_steps: &mut Vec<String>,
) -> Result<Duration, String> {
    let mut fail = |step: &str, reason: String| -> String {
        let message = format!("{step}: {reason}");
        failed_steps.push(message.clone());
        message
    };
    // 1. Меню адаптера дошли до плеера.
    for (step, line) in [
        ("audio-menu", AUDIO_MENU_LINE),
        ("quality-menu", QUALITY_MENU_LINE),
    ] {
        wait_for_lines_anywhere(&mut run.player_log, line, 1, LINE_TIMEOUT)
            .map_err(|error| fail(step, error))?;
    }
    let navigation = wait_for_lines_anywhere(&mut run.player_log, NAVIGATION_LINE, 1, LINE_TIMEOUT)
        .map_err(|error| fail("page-navigation", error))?;
    if !navigation
        .iter()
        .any(|line| line.contains(NEXT_EPISODE_AVAILABLE))
    {
        return Err(fail(
            "page-navigation",
            format!("no '{NEXT_EPISODE_AVAILABLE}' in {navigation:?}"),
        ));
    }
    // 2. Качество: страница открыла 720p, окно его показало.
    wait_for_page_line(run, QUALITY_GENERATION).map_err(|error| fail("quality-on-page", error))?;
    let shown_720 = format!("{BROWSER_QUALITY_SHOWN_MARKER} label=720p");
    wait_for_lines_anywhere(&mut run.player_log, &shown_720, 1, LINE_TIMEOUT)
        .map_err(|error| fail("window-shows-720p", error))?;
    // 3. Озвучка: страница начала с нуля, плеер вернулся на секунду выбора (второй перенос позиции:
    //    первый — после смены качества).
    wait_for_page_line(run, DUB_GENERATION).map_err(|error| fail("dub-on-page", error))?;
    wait_for_lines_anywhere(&mut run.player_log, POSITION_CARRIED_LINE, 2, LINE_TIMEOUT)
        .map_err(|error| fail("dub-position-carried", error))?;
    let position = control
        .position_micros()
        .map_err(|error| fail("dub-position-read", error.to_string()))?;
    let position_after_dub = Duration::from_micros(u64::try_from(position).unwrap_or(0));
    run.player_log
        .wait_for_frame_growth(MIN_FRAMES_GAINED, GROWTH_TIMEOUT)
        .map_err(|error| fail("frames-after-dub", error.to_string()))?;
    // 4. Next: следующая серия в той же озвучке, кадры растут.
    control
        .wait_until_controllable(CONTROLLABLE_TIMEOUT)
        .and_then(|()| control.next())
        .map_err(|error| fail("mpris-next", error.to_string()))?;
    wait_for_lines_anywhere(
        &mut run.player_log,
        NEXT_EPISODE_REQUESTED_LINE,
        1,
        LINE_TIMEOUT,
    )
    .map_err(|error| fail("next-episode-requested", error))?;
    wait_for_page_line(run, NEXT_GENERATION)
        .map_err(|error| fail("next-episode-on-page", error))?;
    run.player_log
        .wait_for_frame_growth(MIN_FRAMES_GAINED, GROWTH_TIMEOUT)
        .map_err(|error| fail("frames-after-next-episode", error.to_string()))?;
    Ok(position_after_dub)
}

fn drive_broken_adapter(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
) -> Result<Verdict, ScenarioError> {
    let query = format!("{HANDOFF_MODE_QUERY}{BROKEN_ADAPTER_QUERY}");
    let mut run = match open_and_hand_off(
        stand,
        environment,
        browser_kind,
        &query,
        &RunTweaks::default(),
    )? {
        Ok(run) => run,
        Err(verdict) => return Ok(verdict),
    };
    let mut failed_steps: Vec<String> = Vec::new();
    match wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)? {
        Ok(()) => watch_broken_adapter(&mut run, &mut failed_steps),
        Err(reason) => failed_steps.push(format!("session-granted: {reason}")),
    }
    let variant_menu_lines = run
        .player_log
        .snapshot_lines_containing(ANY_VARIANT_MENU_LINE)?;
    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let forbidden_player_lines = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    Ok(judge_broken_adapter(
        SERIES_PAGE,
        &BrokenAdapterEvidence {
            failed_steps,
            variant_menu_lines,
            host: read_host_evidence(&exit.stderr_log)?,
            page_report,
            forbidden_player_lines,
            page_log: format_browser_log(&page_log),
        },
    ))
}

/// Кадры в плеере, адаптер точно дошёл до плеера страницы и упал, ещё немного смотрим.
fn watch_broken_adapter(run: &mut HandoffRun<'_>, failed_steps: &mut Vec<String>) {
    if let Err(error) = run
        .player_log
        .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
    {
        failed_steps.push(format!("player-presents-frames: {error}"));
        return;
    }
    if let Err(error) = wait_for_page_line(run, BROKEN_PLAYER_CALLED) {
        failed_steps.push(format!("adapter-reached-the-broken-page-player: {error}"));
        return;
    }
    std::thread::sleep(BROKEN_ADAPTER_OBSERVATION);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::page_catalog::PAGE_SCENARIOS;

    fn page_source() -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("site/pages/{SERIES_PAGE}.html")),
        )
        .expect("page file")
    }

    #[test]
    fn the_page_is_in_the_catalog_and_writes_the_lines_the_robot_waits_for() {
        assert!(
            PAGE_SCENARIOS
                .iter()
                .any(|scenario| scenario.id == SERIES_PAGE)
        );
        let source = page_source();
        // Строки поколений собираются страницей из шаблона: проверяем шаблон и причины.
        for fragment in [
            "generation cause=${cause} episode=${state.episode} dub=${state.dub} quality=${state.quality}",
            "'quality'",
            "'dub'",
            "'next'",
            BROKEN_PLAYER_CALLED,
        ] {
            assert!(source.contains(fragment), "page lacks {fragment}");
        }
        // Метка стенда, по которой адаптер `bench-series` узнаёт страницу.
        assert!(source.contains("data-fastiplayer-bench-series"));
        assert!(source.contains("window.fastiplayerBenchSeries"));
    }

    #[test]
    fn the_choices_select_720p_before_dub_b_at_the_judged_second() {
        assert_eq!(VARIANT_CHOICES, "quality:1@2500,audio:1@6000");
        assert!(VARIANT_CHOICES.ends_with(&format!("@{}", DUB_SELECTED_AT.as_millis())));
        assert_eq!(page_ids(), vec![SERIES_PAGE]);
    }
}
