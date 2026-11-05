//! Чистый приговор страниц «реклама → фильм» (сессия 09c2, решения 201 и 204): по собранным уликам
//! решает, что сеанс пережил конец рекламы и фильм пошёл в плеере в ТОМ ЖЕ окне, а браузер почти
//! не декодирует. Ввода-вывода здесь нет, поэтому каждое нарушение проверяется юнит-тестом.
//!
//! Что считается результатом, а не вызовом (правило тестов проекта):
//!  * `hand_off_tab` во время рекламы дал `started`;
//!  * плеер выдал ровно ОДИН пропуск за весь прогон (одно окно) и ни разу не показал зрителю
//!    сообщение о конце сеанса;
//!  * в сводке плеера `loads=N` не меньше числа загрузок (реклама и фильм; у цепочки больше);
//!  * показ кадров вырос не меньше чем на 60 ПОСЛЕ того, как страница запустила фильм;
//!  * часы фильма на странице идут под тенью, `play()` фильма исполнено, фильм не на паузе;
//!  * настоящий счётчик кадров браузера у фильма мал (кадры показывает плеер);
//!  * `ended` рекламы сработал ровно один раз на ролик;
//!  * фон экстеншена скачал файлы рекламы (она ушла в плеер), а баннер остался у браузера.

use super::super::super::evidence::{host_failures, page_failures, unexpected_player_lines};
use super::super::super::outcome::Verdict;
use super::super::decoys::FetchCounts;
use super::super::judge::max_real_frames;
use crate::browsers::HandOffOutcome;
use crate::observe::{FrameGrowth, HostLogEvidence};
use crate::site::{PageReport, SiteError};

/// На сколько должен вырасти показ кадров плеера после запуска фильма страницей: 60 кадров —
/// 2,4 с видео при 25 к/с, «играет», а не «один кадр».
pub(super) const MIN_FILM_FRAMES_AFTER_START: u64 = 60;
/// Самое короткое окно, на котором часы фильма что-то доказывают, с (робот ждёт две сводки по 10 с).
const MIN_FILM_WINDOW_S: f64 = 8.0;
/// На сколько секунд часы фильма обязаны уйти вперёд за это окно.
const MIN_FILM_CLOCK_ADVANCE_S: f64 = 1.0;
/// Потолок настоящих кадров браузера у элемента фильма за всё окно (запас на момент перехода).
const MAX_FILM_REAL_FRAMES: f64 = 15.0;
/// Баннер, оставшийся у браузера, обязан декодироваться (25 к/с за ~10 с — сотни кадров).
const MIN_BANNER_REAL_FRAMES: f64 = 30.0;

/// Что обязана показать страница, чтобы приговор был «пройдено».
#[derive(Debug, Clone, Copy)]
pub(in crate::scenarios::capture) struct AdHopExpectation {
    /// Сколько роликов рекламы должны дойти до конца (у каждого ровно один `ended`).
    pub(in crate::scenarios::capture) ads_ended: u32,
    /// Не меньше стольких загрузок источника в плеере (`loads=` сводки).
    pub(in crate::scenarios::capture) min_loads: u64,
    /// Файлы, которые фон экстеншена обязан скачать (они ушли в плеер).
    pub(in crate::scenarios::capture) background_files: &'static [&'static str],
    /// Файл баннера рядом: фон его качать НЕ должен, браузер должен.
    pub(in crate::scenarios::capture) banner_file: Option<&'static str>,
}

/// Всё, что собрал прогон: вход чистого приговора.
pub(in crate::scenarios::capture) struct AdHopEvidence {
    /// Идентификатор страницы (отчёт должен быть от неё).
    pub(in crate::scenarios::capture) page_id: &'static str,
    /// Исход передачи во время рекламы.
    pub(in crate::scenarios::capture) hand_off: Result<HandOffOutcome, String>,
    /// Отметки посредника за весь прогон.
    pub(in crate::scenarios::capture) host: HostLogEvidence,
    /// Рост показа кадров между двумя сводками ПОСЛЕ запуска фильма страницей.
    pub(in crate::scenarios::capture) film_growth: Result<FrameGrowth, String>,
    /// Наибольшее `loads=` среди сводок плеера.
    pub(in crate::scenarios::capture) max_loads: Option<u64>,
    /// Сколько раз файлы просили элемент страницы и фон.
    pub(in crate::scenarios::capture) fetches: Vec<FetchCounts>,
    /// Строки «окно показало сообщение о конце сеанса» (в этих сценариях их быть не должно).
    pub(in crate::scenarios::capture) end_notice_lines: Vec<String>,
    /// Отчёт страницы или ошибка ожидания.
    pub(in crate::scenarios::capture) page_report: Result<PageReport, SiteError>,
    /// Запрещённые строки журнала плеера (с шумом).
    pub(in crate::scenarios::capture) forbidden_player_lines: Vec<String>,
    /// Хвост журнала страницы (прикладывается при провале).
    pub(in crate::scenarios::capture) page_log: String,
}

/// Метрика отчёта страницы; нет метрики — это провал, а не нуль.
fn metric(report: &PageReport, name: &str, failures: &mut Vec<String>) -> Option<f64> {
    let value = report.metrics.get(name).copied();
    if value.is_none() {
        failures.push(format!("the page report has no metric '{name}'"));
    }
    value
}

/// Проверки по метрикам страницы: фильм, реклама, баннер.
fn page_metric_failures(
    expectation: &AdHopExpectation,
    report: &PageReport,
    failures: &mut Vec<String>,
) {
    if let Some(window_s) = metric(report, "film_window_s", failures)
        && window_s < MIN_FILM_WINDOW_S
    {
        failures.push(format!(
            "the film window was {window_s:.1} s, too short to judge the film clock (need {MIN_FILM_WINDOW_S} s)"
        ));
    }
    if let Some(advance_s) = metric(report, "film_clock_advance_s", failures)
        && advance_s < MIN_FILM_CLOCK_ADVANCE_S
    {
        failures.push(format!(
            "the film clock on the page advanced only {advance_s:.2} s (need {MIN_FILM_CLOCK_ADVANCE_S} s): the film does not play under the shadow"
        ));
    }
    if let Some(frames) = metric(report, "film_real_frames", failures)
        && frames > MAX_FILM_REAL_FRAMES
    {
        failures.push(format!(
            "the browser decoded {frames} real frames of the film (allowed {MAX_FILM_REAL_FRAMES}): the film plays in the browser, not in the player"
        ));
    }
    if metric(report, "film_paused", failures).is_some_and(|paused| paused > 0.0) {
        failures.push("the page sees the film paused".to_owned());
    }
    if metric(report, "film_play_rejected", failures).is_some_and(|rejected| rejected > 0.0) {
        failures.push("the page's film play() promise was rejected".to_owned());
    }
    if let Some(ended) = metric(report, "ad_ended_total", failures)
        && (ended - f64::from(expectation.ads_ended)).abs() > f64::EPSILON
    {
        failures.push(format!(
            "the ad 'ended' handler fired {ended} time(s) in total, expected exactly {}",
            expectation.ads_ended
        ));
    }
    let ad_window_s = metric(report, "ad_shadow_window_s", failures);
    if let (Some(window_s), Some(frames)) =
        (ad_window_s, metric(report, "ad_real_frames", failures))
    {
        let allowed = max_real_frames(window_s);
        if frames > allowed {
            failures.push(format!(
                "the browser decoded {frames} real frames of the ad in {window_s:.1} s under the shadow (allowed {allowed:.0})"
            ));
        }
    }
    if expectation.banner_file.is_some()
        && let Some(frames) = metric(report, "banner_real_frames", failures)
        && frames < MIN_BANNER_REAL_FRAMES
    {
        failures.push(format!(
            "the banner decoded only {frames} real frames (need {MIN_BANNER_REAL_FRAMES}): it was taken from the browser"
        ));
    }
}

/// Проверки по журналу сервера: что фон качал, а что оставил браузеру.
fn fetch_failures(expectation: &AdHopExpectation, fetches: &[FetchCounts]) -> Vec<String> {
    let mut failures = Vec::new();
    let find = |path: &str| fetches.iter().find(|counts| counts.path == path);
    for path in expectation.background_files {
        match find(path) {
            Some(counts) if counts.background_requests > 0 => {}
            Some(_) => failures.push(format!(
                "the extension background never downloaded {path}: the ad did not go to the player"
            )),
            None => failures.push(format!("no fetch evidence for {path}")),
        }
    }
    if let Some(path) = expectation.banner_file {
        match find(path) {
            Some(counts) if counts.element_requests == 0 => failures.push(format!(
                "the banner {path} was never loaded by the page's own <video>: the ruler is blind"
            )),
            Some(counts) if counts.background_requests > 0 => failures.push(format!(
                "the extension background downloaded the banner {path} ({} request(s)): it must stay in the browser",
                counts.background_requests
            )),
            Some(_) => {}
            None => failures.push(format!("no fetch evidence for the banner {path}")),
        }
    }
    failures
}

/// Чистое решение «реклама кончилась, фильм пошёл в плеере в том же окне».
pub(in crate::scenarios::capture) fn judge_ad_hop(
    expectation: &AdHopExpectation,
    evidence: &AdHopEvidence,
) -> Verdict {
    let mut failures: Vec<String> = Vec::new();
    match &evidence.hand_off {
        Ok(HandOffOutcome::Started) => {}
        Ok(other) => failures.push(format!(
            "hand_off_tab during the ad: {other:?}, expected started"
        )),
        Err(error) => failures.push(format!("hand_off_tab during the ad: {error}")),
    }
    failures.extend(host_failures(&evidence.host));
    if evidence.host.session_grants != 1 {
        failures.push(format!(
            "the player granted {} session(s) in the whole run, expected exactly one (one window for the ad and the film)",
            evidence.host.session_grants
        ));
    }
    if !evidence.end_notice_lines.is_empty() {
        failures.push(format!(
            "the window showed a session end notice, the session did not survive the hop: {}",
            evidence.end_notice_lines.join(" | ")
        ));
    }
    match evidence.max_loads {
        Some(loads) if loads >= expectation.min_loads => {}
        Some(loads) => failures.push(format!(
            "the player summaries show loads={loads}, expected at least {} (the ad and the film)",
            expectation.min_loads
        )),
        None => failures.push("no 'loads=' in any player summary".to_owned()),
    }
    match &evidence.film_growth {
        Ok(growth) if growth.frames_gained() < MIN_FILM_FRAMES_AFTER_START => {
            failures.push(format!(
                "after the page started the film the player presented only {} more frames (need {MIN_FILM_FRAMES_AFTER_START}): {}",
                growth.frames_gained(),
                growth.after.summary_line
            ));
        }
        Ok(growth) if growth.after.position_regressions > 0 => failures.push(format!(
            "playback position went backwards {} time(s): {}",
            growth.after.position_regressions, growth.after.summary_line
        )),
        Ok(_) => {}
        Err(message) => failures.push(message.clone()),
    }
    failures.extend(page_failures(&evidence.page_report, evidence.page_id));
    if let Ok(report) = &evidence.page_report {
        page_metric_failures(expectation, report, &mut failures);
    }
    failures.extend(fetch_failures(expectation, &evidence.fetches));
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    let summary = summary_of(evidence);
    let summary = if failures.is_empty() {
        summary
    } else {
        format!("{summary}\n{}", evidence.page_log)
    };
    Verdict::from_failures(&failures, summary)
}

/// Короткая сводка улик для строки отчёта.
fn summary_of(evidence: &AdHopEvidence) -> String {
    let gained = evidence
        .film_growth
        .as_ref()
        .ok()
        .map(FrameGrowth::frames_gained);
    let metric_of = |name: &str| {
        evidence
            .page_report
            .as_ref()
            .ok()
            .and_then(|report| report.metrics.get(name).copied())
    };
    format!(
        "hand_off={:?} sessions={} loads={:?} film_frames_gained={gained:?} film_real_frames={:?} ad_real_frames={:?} film_clock_advance_s={:?}",
        evidence.hand_off,
        evidence.host.session_grants,
        evidence.max_loads,
        metric_of("film_real_frames"),
        metric_of("ad_real_frames"),
        metric_of("film_clock_advance_s"),
    )
}

#[cfg(test)]
mod tests {
    use super::super::AD_HOP_PAGES;
    use super::*;
    use crate::observe::FrameEvidence;
    use crate::scenarios::capture::judge::tests::report_with;
    use crate::scenarios::outcome::Outcome;
    use crate::site::{PageStatus, RunId};
    use std::time::Duration;

    fn growth(before: u64, after: u64, regressions: u64) -> FrameGrowth {
        let evidence = |frames, position_regressions| FrameEvidence {
            presented_frames: frames,
            position_regressions,
            summary_line: format!("summary frames={frames}"),
        };
        FrameGrowth {
            before: evidence(before, 0),
            after: evidence(after, regressions),
        }
    }

    /// Метрики здоровой страницы: фильм идёт 12 с под тенью, браузер почти не декодирует.
    fn healthy_metrics(expectation: &AdHopExpectation) -> Vec<(&'static str, f64)> {
        let mut metrics = vec![
            ("film_window_s", 12.0),
            ("film_clock_advance_s", 11.0),
            ("film_real_frames", 4.0),
            ("film_paused", 0.0),
            ("film_play_rejected", 0.0),
            ("ad_ended_total", f64::from(expectation.ads_ended)),
            ("ad_shadow_window_s", 6.0),
            ("ad_real_frames", 6.0),
        ];
        if expectation.banner_file.is_some() {
            metrics.push(("banner_real_frames", 240.0));
        }
        metrics
    }

    fn healthy_fetches(expectation: &AdHopExpectation) -> Vec<FetchCounts> {
        let mut fetches: Vec<FetchCounts> = expectation
            .background_files
            .iter()
            .map(|path| FetchCounts {
                path: (*path).to_owned(),
                element_requests: 1,
                background_requests: 3,
            })
            .collect();
        if let Some(path) = expectation.banner_file {
            fetches.push(FetchCounts {
                path: path.to_owned(),
                element_requests: 2,
                background_requests: 0,
            });
        }
        fetches
    }

    fn healthy(page_id: &'static str, expectation: &AdHopExpectation) -> AdHopEvidence {
        AdHopEvidence {
            page_id,
            hand_off: Ok(HandOffOutcome::Started),
            host: HostLogEvidence {
                caller_accepted: true,
                hello_accepted: true,
                session_granted: true,
                session_grants: 1,
            },
            film_growth: Ok(growth(100, 400, 0)),
            max_loads: Some(expectation.min_loads),
            fetches: healthy_fetches(expectation),
            end_notice_lines: Vec::new(),
            page_report: Ok(PageReport {
                scenario: page_id.into(),
                ..report_with(&healthy_metrics(expectation))
            }),
            forbidden_player_lines: Vec::new(),
            page_log: "page log".into(),
        }
    }

    fn with_metric(evidence: &mut AdHopEvidence, name: &str, value: f64) {
        let report = evidence.page_report.as_mut().expect("a report");
        report.metrics.insert(name.to_owned(), value);
    }

    #[test]
    fn every_ad_page_passes_with_healthy_evidence() {
        for page in AD_HOP_PAGES {
            let verdict =
                judge_ad_hop(&page.expectation, &healthy(page.page_id, &page.expectation));
            assert_eq!(
                verdict.outcome,
                Outcome::Pass,
                "{}: {}",
                page.step_id,
                verdict.detail
            );
        }
    }

    /// Каждое нарушение даёт провал и называет себя в тексте (иначе судья «зелёный» от любой беды).
    #[test]
    fn every_broken_link_fails_and_names_itself() {
        let chain = AD_HOP_PAGES
            .iter()
            .find(|page| page.page_id == "ad-chain")
            .expect("the chain page");
        let banner = AD_HOP_PAGES
            .iter()
            .find(|page| page.page_id == "ad-then-banner")
            .expect("the banner page");
        type Change = Box<dyn Fn(&mut AdHopEvidence)>;
        let cases: Vec<(&str, &str, Change)> = vec![
            (
                "not started",
                "expected started",
                Box::new(|e| e.hand_off = Ok(HandOffOutcome::NoVideo)),
            ),
            (
                "hand-off error",
                "hand_off_tab during the ad",
                Box::new(|e| e.hand_off = Err("no reply".into())),
            ),
            (
                "two windows",
                "expected exactly one",
                Box::new(|e| e.host.session_grants = 2),
            ),
            (
                "no window",
                "expected exactly one",
                Box::new(|e| e.host.session_grants = 0),
            ),
            (
                "end notice",
                "session end notice",
                Box::new(|e| {
                    e.end_notice_lines =
                        vec!["Browser session end notice shown notice=page_took_video_back".into()];
                }),
            ),
            (
                "one load only",
                "loads=1",
                Box::new(|e| e.max_loads = Some(1)),
            ),
            (
                "no loads field",
                "no 'loads='",
                Box::new(|e| e.max_loads = None),
            ),
            (
                "film frames stall",
                "only 59 more frames",
                Box::new(|e| e.film_growth = Ok(growth(100, 159, 0))),
            ),
            (
                "no film summaries",
                "no summary",
                Box::new(|e| e.film_growth = Err("no summary".into())),
            ),
            (
                "position regressions",
                "went backwards",
                Box::new(|e| e.film_growth = Ok(growth(100, 400, 2))),
            ),
            (
                "short film window",
                "too short",
                Box::new(|e| with_metric(e, "film_window_s", 3.0)),
            ),
            (
                "film clock frozen",
                "film clock on the page advanced only",
                Box::new(|e| with_metric(e, "film_clock_advance_s", 0.2)),
            ),
            (
                "browser decodes the film",
                "real frames of the film",
                Box::new(|e| with_metric(e, "film_real_frames", 16.0)),
            ),
            (
                "film paused for the page",
                "sees the film paused",
                Box::new(|e| with_metric(e, "film_paused", 1.0)),
            ),
            (
                "film play() rejected",
                "promise was rejected",
                Box::new(|e| with_metric(e, "film_play_rejected", 1.0)),
            ),
            (
                "second ended",
                "fired 2 time(s)",
                Box::new(|e| with_metric(e, "ad_ended_total", 2.0)),
            ),
            (
                "no ended",
                "fired 0 time(s)",
                Box::new(|e| with_metric(e, "ad_ended_total", 0.0)),
            ),
            (
                "browser decodes the ad",
                "real frames of the ad",
                Box::new(|e| with_metric(e, "ad_real_frames", 400.0)),
            ),
            (
                "metric missing",
                "no metric 'film_real_frames'",
                Box::new(|e| {
                    e.page_report
                        .as_mut()
                        .expect("a report")
                        .metrics
                        .remove("film_real_frames");
                }),
            ),
            (
                "background never fetched the ad",
                "never downloaded",
                Box::new(|e| {
                    e.fetches[0].background_requests = 0;
                }),
            ),
            (
                "no fetch evidence",
                "no fetch evidence",
                Box::new(|e| e.fetches.clear()),
            ),
            (
                "page failed",
                "page report is",
                Box::new(|e| {
                    e.page_report.as_mut().expect("a report").status = PageStatus::Fail;
                }),
            ),
            (
                "page report missing",
                "page:",
                Box::new(|e| {
                    e.page_report = Err(SiteError::ReportTimeout {
                        run: RunId::new("r-1").expect("run id"),
                        waited: Duration::from_secs(1),
                    });
                }),
            ),
            (
                "forbidden line",
                "forbidden player line",
                Box::new(|e| {
                    e.forbidden_player_lines = vec!["ERROR renderer lost".into()];
                }),
            ),
        ];
        for (what, expected_text, change) in cases {
            let mut evidence = healthy(chain.page_id, &chain.expectation);
            change(&mut evidence);
            let verdict = judge_ad_hop(&chain.expectation, &evidence);
            assert_eq!(verdict.outcome, Outcome::Fail, "{what} must fail");
            assert!(
                verdict.detail.contains(expected_text),
                "{what}: '{expected_text}' not in: {}",
                verdict.detail
            );
        }

        // Страница-«баннер»: баннер обязан остаться у браузера и декодироваться.
        let banner_cases: Vec<(&str, &str, Change)> = vec![
            (
                "banner taken by the player",
                "banner decoded only",
                Box::new(|e| with_metric(e, "banner_real_frames", 5.0)),
            ),
            (
                "banner downloaded by the background",
                "downloaded the banner",
                Box::new(|e| {
                    e.fetches
                        .last_mut()
                        .expect("banner fetches")
                        .background_requests = 2;
                }),
            ),
            (
                "banner never loaded by the page",
                "ruler is blind",
                Box::new(|e| {
                    e.fetches
                        .last_mut()
                        .expect("banner fetches")
                        .element_requests = 0;
                }),
            ),
            (
                "banner metric missing",
                "no metric 'banner_real_frames'",
                Box::new(|e| {
                    e.page_report
                        .as_mut()
                        .expect("a report")
                        .metrics
                        .remove("banner_real_frames");
                }),
            ),
        ];
        for (what, expected_text, change) in banner_cases {
            let mut evidence = healthy(banner.page_id, &banner.expectation);
            change(&mut evidence);
            let verdict = judge_ad_hop(&banner.expectation, &evidence);
            assert_eq!(verdict.outcome, Outcome::Fail, "{what} must fail");
            assert!(
                verdict.detail.contains(expected_text),
                "{what}: {}",
                verdict.detail
            );
        }
    }

    #[test]
    fn a_report_from_another_page_fails_even_when_it_passed() {
        let page = &AD_HOP_PAGES[0];
        let mut evidence = healthy(page.page_id, &page.expectation);
        evidence.page_report = Ok(PageReport {
            scenario: "mse-fmp4".into(),
            ..report_with(&healthy_metrics(&page.expectation))
        });
        let verdict = judge_ad_hop(&page.expectation, &evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict.detail.contains("expected 'ad-preroll'"),
            "{}",
            verdict.detail
        );
    }

    #[test]
    fn the_skipped_ad_page_expects_no_ended_and_the_chain_four_loads() {
        let by_step = |step: &str| {
            AD_HOP_PAGES
                .iter()
                .find(|page| page.step_id == step)
                .expect("a known step")
        };
        assert_eq!(by_step("ad-skipped-midway").expectation.ads_ended, 0);
        assert_eq!(by_step("ad-chain").expectation.ads_ended, 3);
        assert_eq!(by_step("ad-chain").expectation.min_loads, 4);
        assert!(by_step("ad-preroll").expectation.min_loads >= 2);
        // Пропущенная реклама, у которой всё-таки сработал `ended`, — не пропуск.
        let skipped = by_step("ad-skipped-midway");
        let mut evidence = healthy(skipped.page_id, &skipped.expectation);
        with_metric(&mut evidence, "ad_ended_total", 1.0);
        assert_eq!(
            judge_ad_hop(&skipped.expectation, &evidence).outcome,
            Outcome::Fail
        );
    }
}
