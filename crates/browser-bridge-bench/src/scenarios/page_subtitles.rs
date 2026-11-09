//! Сценарий `page-subtitles` (сессия 13b2): субтитры, которые страница берёт САМА, доходят до плеера.
//!
//! Четыре страницы — четыре способа, которыми настоящие сайты получают субтитры:
//!   - `subtitles-track`: `<video src>` с `<track default>` (en) и выключенной `<track>` (uk);
//!   - `subtitles-fetch`: MSE-плеер качает `.vtt` через `fetch` со ВТОРОГО origin;
//!   - `subtitles-xhr`: MSE-плеер качает `.srt` (XHR текстом) и json3 (XHR как JSON);
//!   - `subtitles-text-track`: как hls.js — `addTextTrack` и реплики без файла; после передачи
//!     дорожка растёт, плеер получает замену документа.
//!
//! Порядок: страница играет → робот «отдаёт вкладку» → плеер показывает кадры → ждём дорожки в
//! журнале плеера (`Page subtitle track ready`) → (растущая дорожка: команда `hook:add-cues`) →
//! ждём, пока сводка плеера покажет `subtitle_overlay_frames > 0` (слой субтитров лёг на кадры).
//! Приговор — [`judge::judge_page_subtitles`]; сами строки журнала разбирает `observe`.

mod judge;

use std::time::{Duration, Instant};

use self::judge::{
    ExpectedDocument, ExpectedSelected, PageExpectation, PageSubtitlesEvidence,
    judge_page_subtitles,
};
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
    PAGE_SUBTITLE_DOCUMENT_MARKER, PAGE_SUBTITLE_TRACK_READY_MARKER, PageSubtitleDocumentLine,
    PageSubtitleTrackLine, PlayerLog, max_subtitle_overlay_frames, parse_page_subtitle_document,
    parse_page_subtitle_track_ready, read_host_evidence,
};

/// Сколько ждём все ожидаемые дорожки в журнале плеера после передачи.
const TRACKS_TIMEOUT: Duration = Duration::from_secs(30);
/// Сколько ждём замену растущей дорожки: экстеншен шлёт растущую дорожку не чаще раза в 5 с.
const REPLACEMENT_TIMEOUT: Duration = Duration::from_secs(30);
/// Сколько ждём сводку плеера с `subtitle_overlay_frames > 0` (сводки раз в 10 с).
const OVERLAY_TIMEOUT: Duration = Duration::from_secs(45);
/// Шаг опроса журнала плеера.
const POLL_STEP: Duration = Duration::from_millis(500);
/// Команда страницы `subtitles-text-track`: добавить остальные реплики.
const ADD_CUES_COMMAND: &str = "hook:add-cues";

/// Документ `.vtt`, который страница показывала сама.
const VTT_SHOWING: ExpectedDocument = ExpectedDocument {
    format_hint: "web_vtt",
    page_showing: true,
};
/// Документ `.vtt`, который страница не показывала (выключенная `<track>`).
const VTT_NOT_SHOWING: ExpectedDocument = ExpectedDocument {
    format_hint: "web_vtt",
    page_showing: false,
};
/// `.srt`, скачанный раньше json3: страница его не показывала.
const SRT_NOT_SHOWING: ExpectedDocument = ExpectedDocument {
    format_hint: "srt",
    page_showing: false,
};
/// json3, скачанный последним: включён (решение 271 «скачала = включено»).
const JSON3_SHOWING: ExpectedDocument = ExpectedDocument {
    format_hint: "json3",
    page_showing: true,
};
/// Включённая дорожка: английский `.vtt`.
const SELECTED_ENGLISH_VTT: ExpectedSelected = ExpectedSelected {
    language: Some("en"),
    format: "web_vtt",
};
/// Включённая дорожка: `.vtt` без языка (плеер пишет `unknown`).
const SELECTED_UNNAMED_VTT: ExpectedSelected = ExpectedSelected {
    language: None,
    format: "web_vtt",
};
/// Включённая дорожка: json3.
const SELECTED_JSON3: ExpectedSelected = ExpectedSelected {
    language: None,
    format: "json3",
};

/// Страница сценария и что она должна дать.
struct SubtitlePage {
    id: &'static str,
    expectation: PageExpectation,
}

const PAGES: &[SubtitlePage] = &[
    SubtitlePage {
        id: "subtitles-track",
        expectation: PageExpectation {
            documents: &[VTT_SHOWING, VTT_NOT_SHOWING],
            selected: SELECTED_ENGLISH_VTT,
            growing_track: false,
        },
    },
    SubtitlePage {
        id: "subtitles-fetch",
        expectation: PageExpectation {
            documents: &[VTT_SHOWING],
            selected: SELECTED_UNNAMED_VTT,
            growing_track: false,
        },
    },
    SubtitlePage {
        id: "subtitles-xhr",
        expectation: PageExpectation {
            documents: &[SRT_NOT_SHOWING, JSON3_SHOWING],
            selected: SELECTED_JSON3,
            growing_track: false,
        },
    },
    SubtitlePage {
        id: "subtitles-text-track",
        expectation: PageExpectation {
            documents: &[VTT_SHOWING],
            selected: SELECTED_ENGLISH_VTT,
            growing_track: true,
        },
    },
];

/// Идентификаторы страниц команды (для проверки `--page` в командной строке).
pub(super) fn page_ids() -> Vec<&'static str> {
    PAGES.iter().map(|page| page.id).collect()
}

/// Прогон выбранных страниц в свежем окружении каждая.
pub(super) fn run_all(stand: &Stand, browser: BrowserKind, filter: &PageFilter) -> Vec<StepResult> {
    PAGES
        .iter()
        .filter(|page| filter.admits(page.id))
        .map(|page| {
            let started = Instant::now();
            let verdict =
                stand.with_environment(|environment| drive(stand, environment, browser, page));
            StepResult::new(
                "page-subtitles",
                browser,
                page.id,
                verdict,
                started.elapsed(),
            )
        })
        .collect()
}

/// Опрашивает `check` до срока; `true`, если условие выполнилось.
fn poll_until(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_STEP);
    }
}

/// Строки журнала плеера про дорожки и документы страницы (разобранные, по порядку).
fn read_subtitle_lines(
    player_log: &mut PlayerLog,
) -> Result<(Vec<PageSubtitleTrackLine>, Vec<PageSubtitleDocumentLine>), ScenarioError> {
    let ready = player_log
        .snapshot_lines_containing(PAGE_SUBTITLE_TRACK_READY_MARKER)?
        .iter()
        .filter_map(|line| parse_page_subtitle_track_ready(line))
        .collect();
    let documents = player_log
        .snapshot_lines_containing(PAGE_SUBTITLE_DOCUMENT_MARKER)?
        .iter()
        .filter_map(|line| parse_page_subtitle_document(line))
        .collect();
    Ok((ready, documents))
}

/// Сколько разных дорожек уже на складе плеера; чтение журнала упало — считаем «пока ноль»
/// (ошибку чтения покажет итоговый снимок).
fn shelved_track_count(player_log: &mut PlayerLog) -> usize {
    read_subtitle_lines(player_log).map_or(0, |(_, documents)| {
        let mut numbers: Vec<u64> = documents.iter().map(|line| line.track).collect();
        numbers.sort_unstable();
        numbers.dedup();
        numbers.len()
    })
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    page: &SubtitlePage,
) -> Result<Verdict, ScenarioError> {
    let mut run = HandoffRun::open(
        stand,
        environment,
        browser_kind,
        page.id,
        "pagesubs",
        ExtensionMode::Real,
    )?;
    match run.wait_page_ready() {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            run.browser.shutdown()?;
            return Ok(judge_early_report(page.id, browser_kind, &report));
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
    let granted = wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)?;
    if let Err(reason) = granted {
        failed_steps.push(format!("session-granted: {reason}"));
    } else {
        collect_subtitle_steps(&mut run, page, &mut failed_steps);
    }

    let (ready_lines, document_lines) = read_subtitle_lines(&mut run.player_log)?;
    let overlay_frames = max_subtitle_overlay_frames(&run.player_log.snapshot_summary_lines()?);
    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let forbidden_player_lines = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_page_subtitles(
        page.id,
        &page.expectation,
        &PageSubtitlesEvidence {
            failed_steps,
            ready_lines,
            document_lines,
            overlay_frames,
            host,
            page_report,
            forbidden_player_lines,
            page_log: format_browser_log(&page_log),
        },
    ))
}

/// Шаги после пропуска: кадры → дорожки → (рост) → слой на кадрах. Провал шага пишется в
/// `failed_steps`, остальные шаги всё равно идут: итоговый снимок журнала нужен целиком.
fn collect_subtitle_steps(
    run: &mut HandoffRun<'_>,
    page: &SubtitlePage,
    failed_steps: &mut Vec<String>,
) {
    if let Err(error) = run
        .player_log
        .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
    {
        failed_steps.push(format!("player-presents-frames: {error}"));
        return;
    }
    let expected_tracks = page.expectation.documents.len();
    if !poll_until(TRACKS_TIMEOUT, || {
        shelved_track_count(&mut run.player_log) >= expected_tracks
    }) {
        failed_steps.push(format!(
            "tracks-shelved: fewer than {expected_tracks} 'Page subtitle document received' track(s) in {TRACKS_TIMEOUT:?}"
        ));
    }
    if page.expectation.growing_track {
        if let Err(error) = tell_and_wait(run, ADD_CUES_COMMAND) {
            failed_steps.push(format!("add-cues: {error}"));
        } else if !poll_until(REPLACEMENT_TIMEOUT, || {
            read_subtitle_lines(&mut run.player_log)
                .is_ok_and(|(_, documents)| documents.iter().any(|line| line.replaced))
        }) {
            failed_steps.push(format!(
                "track-replaced: no 'replaced=true' document in {REPLACEMENT_TIMEOUT:?}"
            ));
        }
    }
    let overlay_seen = poll_until(OVERLAY_TIMEOUT, || {
        run.player_log
            .snapshot_summary_lines()
            .is_ok_and(|lines| max_subtitle_overlay_frames(&lines).is_some_and(|frames| frames > 0))
    });
    if !overlay_seen {
        failed_steps.push(format!(
            "overlay-on-frames: no summary with subtitle_overlay_frames > 0 in {OVERLAY_TIMEOUT:?}"
        ));
    }
}

#[cfg(test)]
mod tests;
