//! Сценарий `quality` (сессия 13c): смена качества на странице доходит до ОКНА плеера.
//!
//! Страница `mse-abr` внутри одного SourceBuffer переключает дорожку 360p → 720p (хук
//! `switch-quality`). Окно плеера обязано показать новое качество: в журнале строки
//! `Browser quality shown label=360p …`, затем `label=720p …` (пишет только окно браузерного
//! режима), а кадры после смены продолжают расти. Приговор — [`judge::judge_quality`].

mod judge;

use std::time::{Duration, Instant};

use self::judge::{QualityEvidence, TIER_360, TIER_720, judge_quality};
use super::capture::{MIN_PRESENTED_FRAMES, judge_early_report};
use super::context::{ScenarioError, Stand};
use super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, wait_for_session_granted,
};
use super::handoff_run::{ExtensionMode, HandoffRun, PageReadiness};
use super::outcome::{StepResult, Verdict};
use super::page_filter::PageFilter;
use super::player_commands::{FRAMES_TIMEOUT, tell_and_wait};
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::{
    BROWSER_QUALITY_SHOWN_MARKER, BrowserQualityLine, PlayerLog, parse_browser_quality_shown,
    read_host_evidence,
};

/// Единственная страница сценария (её же гоняет `capture`).
const QUALITY_PAGE: &str = "mse-abr";
/// Хук страницы: переключить дорожку на `video-720`.
const SWITCH_QUALITY_COMMAND: &str = "hook:switch-quality";
/// Сколько ждём строку «качество показано» в журнале плеера.
const QUALITY_LINE_TIMEOUT: Duration = Duration::from_secs(45);
/// Сколько ждём роста кадров после смены (сводки раз в 10 с).
const GROWTH_TIMEOUT: Duration = Duration::from_secs(45);
/// На сколько кадров должен вырасти показ между двумя сводками после смены.
const MIN_FRAMES_GAINED_AFTER_SWITCH: u64 = 30;

/// Идентификаторы страниц команды (для проверки `--page` в командной строке).
pub(super) fn page_ids() -> Vec<&'static str> {
    vec![QUALITY_PAGE]
}

/// Прогон в свежем окружении (если страница выбрана фильтром).
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    if !filter.admits(QUALITY_PAGE) {
        return Vec::new();
    }
    let started = Instant::now();
    let verdict = stand.with_environment(|environment| drive(stand, environment, browser));
    vec![StepResult::new(
        "quality",
        browser,
        QUALITY_PAGE,
        verdict,
        started.elapsed(),
    )]
}

/// Все строки «качество показано» журнала по порядку.
fn read_quality_lines(
    player_log: &mut PlayerLog,
) -> Result<Vec<BrowserQualityLine>, ScenarioError> {
    Ok(player_log
        .snapshot_lines_containing(BROWSER_QUALITY_SHOWN_MARKER)?
        .iter()
        .filter_map(|line| parse_browser_quality_shown(line))
        .collect())
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
) -> Result<Verdict, ScenarioError> {
    let mut run = HandoffRun::open(
        stand,
        environment,
        browser_kind,
        QUALITY_PAGE,
        "quality",
        ExtensionMode::Real,
    )?;
    match run.wait_page_ready() {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            run.browser.shutdown()?;
            return Ok(judge_early_report(QUALITY_PAGE, browser_kind, &report));
        }
        PageReadiness::TimedOut => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!("the page never became ready\n{log}")));
        }
    }
    match run.hand_off() {
        Ok(HandOffOutcome::Started) => {}
        other => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!("hand_off_tab: {other:?}\n{log}")));
        }
    }
    run.tell_page("handed-off");

    let mut failed_steps: Vec<String> = Vec::new();
    let frames_after_switch =
        match wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)? {
            Ok(()) => switch_and_watch(&mut run, &mut failed_steps),
            Err(reason) => {
                failed_steps.push(format!("session-granted: {reason}"));
                Err("not awaited: no session".to_owned())
            }
        };

    let quality_lines = read_quality_lines(&mut run.player_log)?;
    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let forbidden_player_lines = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_quality(
        QUALITY_PAGE,
        &QualityEvidence {
            failed_steps,
            quality_lines,
            frames_after_switch,
            host,
            page_report,
            forbidden_player_lines,
            page_log: format_browser_log(&page_log),
        },
    ))
}

/// Кадры → строка 360p → хук смены → строка 720p → рост кадров. Провал шага пишется в
/// `failed_steps`; возвращает, на сколько выросли кадры после смены.
fn switch_and_watch(
    run: &mut HandoffRun<'_>,
    failed_steps: &mut Vec<String>,
) -> Result<u64, String> {
    let mut fail = |step: &str, reason: String| -> String {
        let message = format!("{step}: {reason}");
        failed_steps.push(message.clone());
        message
    };
    // Порядок важен: журнал читается курсором, а строка о качестве пишется с ПЕРВЫМ кадром.
    // Ожидание кадров листает журнал вперёд и проглотило бы её, поэтому сначала — подпись.
    let initial_marker = format!("{BROWSER_QUALITY_SHOWN_MARKER} label={}", TIER_360.label);
    run.player_log
        .wait_for_line(&initial_marker, QUALITY_LINE_TIMEOUT)
        .map_err(|error| fail("window-shows-360p", error.to_string()))?;
    run.player_log
        .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
        .map_err(|error| fail("player-presents-frames", error.to_string()))?;
    tell_and_wait(run, SWITCH_QUALITY_COMMAND).map_err(|error| fail("switch-quality", error))?;
    let switched_marker = format!("{BROWSER_QUALITY_SHOWN_MARKER} label={}", TIER_720.label);
    run.player_log
        .wait_for_line(&switched_marker, QUALITY_LINE_TIMEOUT)
        .map_err(|error| fail("window-shows-720p", error.to_string()))?;
    run.player_log
        .wait_for_frame_growth(MIN_FRAMES_GAINED_AFTER_SWITCH, GROWTH_TIMEOUT)
        .map(|growth| growth.frames_gained())
        .map_err(|error| fail("frames-grow-after-switch", error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::page_catalog::PAGE_SCENARIOS;

    #[test]
    fn the_page_is_in_the_catalog_and_declares_the_hook_the_robot_sends() {
        assert!(
            PAGE_SCENARIOS
                .iter()
                .any(|scenario| scenario.id == QUALITY_PAGE)
        );
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("site/pages/{QUALITY_PAGE}.html")),
        )
        .expect("page file");
        let hook = SWITCH_QUALITY_COMMAND.trim_start_matches("hook:");
        assert!(
            source.contains(&format!("'{hook}'")),
            "page lacks hook {hook}"
        );
        assert_eq!(page_ids(), vec![QUALITY_PAGE]);
    }
}
