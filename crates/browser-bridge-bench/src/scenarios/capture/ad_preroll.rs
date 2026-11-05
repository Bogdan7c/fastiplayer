//! `ad-preroll` (09c): сначала играет ТОЛЬКО отдельный рекламный ролик, потом стартует фильм в
//! другом элементе. Страница идёт под командами робота в две фазы:
//!   1. реклама играет по кругу; робот просит передачу и обязан получить `ad_playing`: сеанса с
//!      плеером нет, а по журналу сервера фон экстеншена рекламный файл не качал;
//!   2. робот шлёт `start-film`; фильм играет в другом элементе; передача → `started`, кадры в
//!      плеере, браузер фильм не декодирует (общий приговор `capture`).
//!
//! Вариант `ad-preroll-unlisted` — живая форма Tortuga: домена ролика нет в списке рекламных, рядом
//! стоит пустой `<video>` той же площади. Рекламу видно только по устройству страницы.
//!
//! ВАЖНО: ожидание фазы 1 (`ad_playing`, без переезда) верно ТОЛЬКО ДО сессии 09c2 (решение 201).
//! В 09c2 передача во время рекламы переезжает на фильм сама, и эта проверка там меняется.

use std::time::{Duration, Instant};

use super::super::context::{ScenarioError, Stand};
use super::super::evidence::{
    FORBIDDEN_PLAYER_LINES, SESSION_GRANTED_TIMEOUT, format_browser_log, host_failures,
    page_failures, unexpected_player_lines, wait_for_session_granted,
};
use super::super::handoff_run::{ExtensionMode, HandoffRun, PageReadiness};
use super::super::outcome::{StepResult, Verdict};
use super::super::run_tweaks::RunTweaks;
use super::decoys::{AD_FILE_PATH, decoy_not_fetched_by_background};
use super::judge::browser_does_not_decode;
use super::{FRAMES_TIMEOUT, MIN_PRESENTED_FRAMES, judge_early_report};
use crate::browsers::{BrowserKind, HandOffOutcome};
use crate::environment::BenchEnvironment;
use crate::observe::{FrameEvidence, HostLogEvidence, read_host_evidence};
use crate::site::{PageReport, SiteError};

/// Идентификатор страницы.
pub(in crate::scenarios) const AD_PREROLL_PAGE: &str = "ad-preroll";
/// Вариант сценария: как страница показывает рекламу.
#[derive(Debug, Clone, Copy)]
pub(in crate::scenarios) struct AdPrerollVariant {
    /// Имя шага в отчёте и в фильтре `--page`.
    pub(in crate::scenarios) step_id: &'static str,
    /// Что добавить к адресу страницы.
    query_suffix: &'static str,
    /// Нужен ли рекламный домен из списка экстеншена (ведёт на сервер стенда).
    uses_listed_ad_host: bool,
}

/// Варианты сценария: реклама с домена из списка и живая форма Tortuga.
pub(in crate::scenarios) const VARIANTS: &[AdPrerollVariant] = &[
    AdPrerollVariant {
        step_id: AD_PREROLL_PAGE,
        query_suffix: "&mode=handoff",
        uses_listed_ad_host: true,
    },
    AdPrerollVariant {
        step_id: "ad-preroll-unlisted",
        query_suffix: "&mode=handoff&ad=unlisted",
        uses_listed_ad_host: false,
    },
];

/// Рекламный домен файла ролика: правила экстеншена считают его рекламным по суффиксу
/// `doubleclick.net`. Имя дублирует `AD_HOST` в `site/pages/ad-preroll.html` (тест их сверяет).
pub(in crate::scenarios) const AD_HOST: &str = "ad.doubleclick.net";
/// Метка страницы: реклама играет, фильма ещё нет.
const AD_READY_MARKER: &str = "ad-ready";
/// Команда странице: рекламу убрать, начать фильм.
const START_FILM_COMMAND: &str = "start-film";
/// Сколько ждём метку `ad-ready`.
const AD_READY_TIMEOUT: Duration = Duration::from_secs(60);
/// Пауза после отказа `ad_playing`: если бы фон всё же открыл сеанс или качал рекламу, это
/// успело бы появиться в журнале посредника и сервера.
const AD_REFUSAL_SETTLE: Duration = Duration::from_millis(1500);

/// Сколько сеансов плеера выдал за этот прогон (журнал посредника).
fn session_grants(run_stderr: &std::path::Path) -> Result<u32, ScenarioError> {
    Ok(read_host_evidence(run_stderr)?.session_grants)
}

pub(in crate::scenarios) fn run(
    stand: &Stand,
    browser: BrowserKind,
    variant: AdPrerollVariant,
) -> StepResult {
    let started = Instant::now();
    let verdict =
        stand.with_environment(|environment| drive(stand, environment, browser, variant, started));
    StepResult::new(
        "capture",
        browser,
        variant.step_id,
        verdict,
        started.elapsed(),
    )
}

fn drive(
    stand: &Stand,
    environment: &BenchEnvironment,
    browser_kind: BrowserKind,
    variant: AdPrerollVariant,
    run_started_at: Instant,
) -> Result<Verdict, ScenarioError> {
    let tweaks = RunTweaks {
        loopback_host_aliases: if variant.uses_listed_ad_host {
            vec![AD_HOST.to_owned()]
        } else {
            Vec::new()
        },
        ..RunTweaks::default()
    };
    let mut run = HandoffRun::open_tweaked(
        stand,
        environment,
        browser_kind,
        AD_PREROLL_PAGE,
        variant.step_id,
        ExtensionMode::Real,
        variant.query_suffix,
        &tweaks,
    )?;
    match run.wait_page_marker(AD_READY_MARKER, AD_READY_TIMEOUT) {
        PageReadiness::Ready => {}
        PageReadiness::ReportedEarly(report) => {
            let verdict = judge_early_report(AD_PREROLL_PAGE, browser_kind, &report);
            run.browser.shutdown()?;
            return Ok(verdict);
        }
        PageReadiness::TimedOut => {
            let log = format_browser_log(&run.take_page_log());
            run.browser.shutdown()?;
            return Ok(Verdict::fail(format!(
                "the page never showed the ad\n{log}"
            )));
        }
    }

    // Фаза 1: передача во время рекламы.
    let ad_attempt_at = Instant::now();
    let ad_outcome = run.hand_off().map_err(|error| error.to_string());
    std::thread::sleep(AD_REFUSAL_SETTLE);
    let grants_after_ad = session_grants(&run.stderr_log)?;
    // Журнал сервера общий на весь запуск: запросы прошлых прогонов в улики не идут, иначе линейка
    // «элемент качал рекламу» подтвердилась бы чужим прогоном.
    let mut ad_requests = stand.site.media_requests();
    ad_requests.retain(|request| request.at >= run_started_at);
    let ad_decoy = decoy_not_fetched_by_background(AD_FILE_PATH, &ad_requests, ad_attempt_at);

    // Фаза 2: фильм.
    run.tell_page(START_FILM_COMMAND);
    let film = match run.wait_page_ready() {
        PageReadiness::Ready => drive_film(&mut run)?,
        PageReadiness::ReportedEarly(report) => FilmPart::not_reached(format!(
            "the page reported {:?} before the film was ready",
            report.status
        )),
        PageReadiness::TimedOut => FilmPart::not_reached("the film never became ready".to_owned()),
    };
    let page_report = run.finish_page();
    let page_log = run.take_page_log();
    let forbidden = run.player_log.snapshot_forbidden(FORBIDDEN_PLAYER_LINES)?;
    let exit = run.browser.shutdown()?;
    let host = read_host_evidence(&exit.stderr_log)?;
    Ok(judge_ad_preroll(&AdPrerollEvidence {
        ad_outcome,
        grants_after_ad,
        ad_decoy,
        film,
        host,
        page_report,
        forbidden_player_lines: forbidden,
        page_log: format_browser_log(&page_log),
    }))
}

/// Что стенд узнал о фильме (фаза 2).
struct FilmPart {
    outcome: Result<HandOffOutcome, String>,
    frames: Result<FrameEvidence, String>,
}

impl FilmPart {
    fn not_reached(reason: String) -> Self {
        Self {
            outcome: Err(reason.clone()),
            frames: Err(format!("not awaited: {reason}")),
        }
    }
}

/// Передача фильма: исход, пропуск посредника, кадры в плеере.
fn drive_film(run: &mut HandoffRun<'_>) -> Result<FilmPart, ScenarioError> {
    let outcome = run.hand_off().map_err(|error| error.to_string());
    if !matches!(outcome, Ok(HandOffOutcome::Started)) {
        return Ok(FilmPart {
            frames: Err("not awaited: the film was not handed off".to_owned()),
            outcome,
        });
    }
    run.tell_page("handed-off");
    let frames = match wait_for_session_granted(&run.stderr_log, SESSION_GRANTED_TIMEOUT)? {
        Ok(()) => run
            .player_log
            .wait_for_presented_frames(MIN_PRESENTED_FRAMES, FRAMES_TIMEOUT)
            .map_err(|error| format!("player did not present frames: {error}")),
        Err(message) => Err(format!("{message}; frames were not awaited")),
    };
    Ok(FilmPart { outcome, frames })
}

/// Вход чистого приговора.
struct AdPrerollEvidence {
    /// Исход передачи во время рекламы.
    ad_outcome: Result<HandOffOutcome, String>,
    /// Сколько сеансов выдал плеер к концу паузы после отказа (должно быть 0).
    grants_after_ad: u32,
    /// Провалы и сводка по журналу сервера: фон рекламу не качал.
    ad_decoy: (Vec<String>, String),
    film: FilmPart,
    /// Итоговые отметки посредника.
    host: HostLogEvidence,
    page_report: Result<PageReport, SiteError>,
    forbidden_player_lines: Vec<String>,
    page_log: String,
}

/// Чистый приговор: реклама не ушла, фильм ушёл и играет в плеере.
fn judge_ad_preroll(evidence: &AdPrerollEvidence) -> Verdict {
    let mut failures: Vec<String> = Vec::new();
    match &evidence.ad_outcome {
        Ok(HandOffOutcome::AdPlaying) => {}
        Ok(other) => failures.push(format!(
            "hand_off_tab during the ad: {other:?}, expected ad_playing (09c2 changes this, decision 201)"
        )),
        Err(error) => failures.push(format!("hand_off_tab during the ad: {error}")),
    }
    if evidence.grants_after_ad != 0 {
        failures.push(format!(
            "the player granted {} session(s) while only the ad was playing",
            evidence.grants_after_ad
        ));
    }
    failures.extend(evidence.ad_decoy.0.iter().cloned());
    match &evidence.film.outcome {
        Ok(HandOffOutcome::Started) => {}
        Ok(other) => failures.push(format!(
            "hand_off_tab for the film: {other:?}, expected started"
        )),
        Err(error) => failures.push(format!("hand_off_tab for the film: {error}")),
    }
    match &evidence.film.frames {
        Ok(found) if found.position_regressions > 0 => failures.push(format!(
            "playback position went backwards {} time(s): {}",
            found.position_regressions, found.summary_line
        )),
        Ok(_) => {}
        Err(message) => failures.push(message.clone()),
    }
    failures.extend(host_failures(&evidence.host));
    if evidence.host.session_grants != 1 {
        failures.push(format!(
            "the player granted {} session(s) in the whole run, expected exactly one (the film)",
            evidence.host.session_grants
        ));
    }
    failures.extend(page_failures(&evidence.page_report, AD_PREROLL_PAGE));
    if let Ok(report) = &evidence.page_report {
        failures.extend(browser_does_not_decode(report));
    }
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    let summary = format!(
        "ad: {:?}, sessions after the ad: {}; {}; film: {:?}, presented_frames={:?}",
        evidence.ad_outcome,
        evidence.grants_after_ad,
        evidence.ad_decoy.1,
        evidence.film.outcome,
        evidence
            .film
            .frames
            .as_ref()
            .map(|found| found.presented_frames)
    );
    let summary = if failures.is_empty() {
        summary
    } else {
        format!("{summary}\n{}", evidence.page_log)
    };
    Verdict::from_failures(&failures, summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::capture::judge::tests::report_with;
    use crate::scenarios::outcome::Outcome;

    fn healthy() -> AdPrerollEvidence {
        AdPrerollEvidence {
            ad_outcome: Ok(HandOffOutcome::AdPlaying),
            grants_after_ad: 0,
            ad_decoy: (Vec::new(), "decoy".into()),
            film: FilmPart {
                outcome: Ok(HandOffOutcome::Started),
                frames: Ok(FrameEvidence {
                    presented_frames: 240,
                    position_regressions: 0,
                    summary_line: "summary".into(),
                }),
            },
            host: HostLogEvidence {
                caller_accepted: true,
                hello_accepted: true,
                session_granted: true,
                session_grants: 1,
            },
            page_report: Ok(PageReport {
                scenario: AD_PREROLL_PAGE.into(),
                ..report_with(&[
                    ("real_probe_available", 1.0),
                    ("shadow_window_s", 12.0),
                    ("real_frames_in_window", 8.0),
                ])
            }),
            forbidden_player_lines: Vec::new(),
            page_log: String::new(),
        }
    }

    fn verdict_of(change: impl FnOnce(&mut AdPrerollEvidence)) -> Verdict {
        let mut evidence = healthy();
        change(&mut evidence);
        judge_ad_preroll(&evidence)
    }

    #[test]
    fn ad_refused_then_film_in_the_player_passes() {
        let verdict = judge_ad_preroll(&healthy());
        assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    }

    #[test]
    fn every_broken_link_of_the_chain_fails() {
        let cases: Vec<(&str, Verdict)> = vec![
            (
                "the ad was handed off",
                verdict_of(|e| e.ad_outcome = Ok(HandOffOutcome::Started)),
            ),
            (
                "a session opened for the ad",
                verdict_of(|e| e.grants_after_ad = 1),
            ),
            (
                "the background downloaded the ad",
                verdict_of(|e| e.ad_decoy.0.push("background downloaded".into())),
            ),
            (
                "the film was refused",
                verdict_of(|e| e.film.outcome = Ok(HandOffOutcome::NoVideo)),
            ),
            (
                "no frames of the film",
                verdict_of(|e| e.film.frames = Err("no frames".into())),
            ),
            (
                "two sessions in the run",
                verdict_of(|e| e.host.session_grants = 2),
            ),
            (
                "the browser kept decoding the film",
                verdict_of(|e| {
                    e.page_report = Ok(PageReport {
                        scenario: AD_PREROLL_PAGE.into(),
                        ..report_with(&[
                            ("real_probe_available", 1.0),
                            ("shadow_window_s", 12.0),
                            ("real_frames_in_window", 300.0),
                        ])
                    })
                }),
            ),
        ];
        for (what, verdict) in cases {
            assert_eq!(verdict.outcome, Outcome::Fail, "{what} must fail");
        }
    }

    #[test]
    fn the_ad_host_is_the_one_the_page_uses_and_the_rules_call_an_ad_domain() {
        let page = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("site/pages/ad-preroll.html"),
        )
        .expect("the ad-preroll page");
        assert!(page.contains(&format!("const AD_HOST = '{AD_HOST}';")));
        // Рекламный суффикс из правил экстеншена (`doubleclick.net`).
        assert!(AD_HOST.ends_with(".doubleclick.net"));
    }
}
