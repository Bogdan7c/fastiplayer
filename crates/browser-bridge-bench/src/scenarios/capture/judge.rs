//! Чистый вердикт `capture`: по собранным уликам решает, дошла ли страница до кадра в рендере
//! плеера без декодирования браузером. Ввода-вывода здесь нет — только вход (`CaptureEvidence`)
//! и решение (`judge_capture`), поэтому всё проверяется юнит-тестами без браузера.

use super::super::evidence::{host_failures, page_failures, unexpected_player_lines};
use super::super::outcome::Verdict;
use crate::observe::{FrameEvidence, FrameGrowth, HostLogEvidence};
use crate::site::{PageReport, SiteError};

/// Самое короткое окно «под тенью», на котором счётчик браузера что-то доказывает, с.
const MIN_SHADOW_WINDOW_S: f64 = 6.0;
/// Потолок настоящих кадров браузера под тенью, кадров в секунду. В 00c браузер под тенью
/// декодировал 0–116 кадров за 60 с (до ~2 к/с из-за перестановки кадров), обычное
/// воспроизведение роликов стенда — 25 к/с. 5 к/с с запасом над худшим замеренным и в пять
/// раз ниже обычного воспроизведения.
const MAX_REAL_FRAMES_PER_SECOND: f64 = 5.0;
/// Запас кадров на момент перехода: команда вернула `started` чуть раньше, чем тень
/// полностью заняла элемент, и пара кадров браузера успевает проскочить.
const HANDOVER_SLACK_FRAMES: f64 = 10.0;

/// Результат действия страницы под тенью.
pub(super) struct HookEvidence {
    pub(super) name: String,
    /// Рост показанных кадров плеера между двумя сводками после действия.
    pub(super) growth: Result<FrameGrowth, String>,
}

/// Всё, что собрал прогон, без привязки к браузеру и файлам: вход чистого вердикта.
pub(super) struct CaptureEvidence {
    pub(super) page_id: &'static str,
    /// Сводка показанных кадров или причина, почему её нет.
    pub(super) frames: Result<FrameEvidence, String>,
    /// Действие под тенью, если у страницы оно есть.
    pub(super) hook: Option<HookEvidence>,
    /// Отметки посредника.
    pub(super) host: HostLogEvidence,
    /// Отчёт страницы или ошибка ожидания.
    pub(super) page_report: Result<PageReport, SiteError>,
    /// Запрещённые строки журнала плеера (с шумом).
    pub(super) forbidden_player_lines: Vec<String>,
    /// Хвост журнала страницы (прикладывается при провале).
    pub(super) page_log: String,
    /// `<video src>`: провалы и сводка запросов файла со стороны сервера (`None` — не файл).
    pub(super) file: Option<(Vec<String>, String)>,
}

/// Сколько настоящих кадров браузера допустимо под тенью за окно `window_s` секунд.
fn max_real_frames(window_s: f64) -> f64 {
    window_s * MAX_REAL_FRAMES_PER_SECOND + HANDOVER_SLACK_FRAMES
}

/// Проверки «браузер не декодирует» по метрикам страницы.
pub(super) fn browser_does_not_decode(report: &PageReport) -> Vec<String> {
    let metric = |name: &str| report.metrics.get(name).copied();
    let (Some(available), Some(window_s), Some(real_frames)) = (
        metric("real_probe_available"),
        metric("shadow_window_s"),
        metric("real_frames_in_window"),
    ) else {
        return vec!["the page report has no real-frame metrics".to_owned()];
    };
    if available < 1.0 {
        return vec!["the page could not read the real frame counter".to_owned()];
    }
    if window_s < MIN_SHADOW_WINDOW_S {
        return vec![format!(
            "the shadow window was {window_s:.1} s, too short to judge the browser decoder (need {MIN_SHADOW_WINDOW_S} s)"
        )];
    }
    let allowed = max_real_frames(window_s);
    if real_frames > allowed {
        return vec![format!(
            "the browser decoded {real_frames} frames in {window_s:.1} s under the shadow (allowed {allowed:.0}): it still plays by itself"
        )];
    }
    Vec::new()
}

/// Чистое решение «страница дошла до кадра в рендере без декодирования браузером».
pub(super) fn judge_capture(evidence: &CaptureEvidence) -> Verdict {
    let mut failures: Vec<String> = Vec::new();
    match &evidence.frames {
        Ok(found) if found.position_regressions > 0 => failures.push(format!(
            "playback position went backwards {} time(s): {}",
            found.position_regressions, found.summary_line
        )),
        Ok(_) => {}
        Err(message) => failures.push(message.clone()),
    }
    failures.extend(host_failures(&evidence.host));
    if let Some(hook) = &evidence.hook
        && let Err(message) = &hook.growth
    {
        failures.push(format!("after '{}': {message}", hook.name));
    }
    failures.extend(page_failures(&evidence.page_report, evidence.page_id));
    if let Ok(report) = &evidence.page_report {
        failures.extend(browser_does_not_decode(report));
    }
    failures.extend(
        unexpected_player_lines(&evidence.forbidden_player_lines)
            .map(|line| format!("forbidden player line: {line}")),
    );
    if let Some((file_failures, _)) = &evidence.file {
        failures.extend(file_failures.iter().cloned());
    }

    let presented = evidence
        .frames
        .as_ref()
        .map_or(0, |found| found.presented_frames);
    let gained = evidence
        .hook
        .as_ref()
        .and_then(|hook| hook.growth.as_ref().ok())
        .map(FrameGrowth::frames_gained);
    let real_frames = evidence
        .page_report
        .as_ref()
        .ok()
        .and_then(|report| report.metrics.get("real_frames_in_window").copied());
    let summary = format!(
        "presented_frames={presented} frames_gained_after_hook={gained:?} browser_real_frames={real_frames:?}; host: caller_accepted={} hello_accepted={} session_granted={}",
        evidence.host.caller_accepted, evidence.host.hello_accepted, evidence.host.session_granted,
    );
    let summary = match &evidence.file {
        Some((_, file_summary)) => format!("{summary}; {file_summary}"),
        None => summary,
    };
    // При провале журнал страницы — главная улика.
    let summary = if failures.is_empty() {
        summary
    } else {
        format!("{summary}\n{}", evidence.page_log)
    };
    Verdict::from_failures(&failures, summary)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::scenarios::outcome::Outcome;
    use crate::site::{PageStatus, ReportCheck, RunId};

    pub(in crate::scenarios::capture) fn report_with(metrics: &[(&str, f64)]) -> PageReport {
        PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: "mse-fmp4".into(),
            status: PageStatus::Pass,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: metrics
                .iter()
                .map(|(name, value)| ((*name).to_owned(), *value))
                .collect(),
        }
    }

    fn healthy_metrics() -> Vec<(&'static str, f64)> {
        vec![
            ("real_probe_available", 1.0),
            ("shadow_window_s", 12.0),
            ("real_frames_in_window", 8.0),
        ]
    }

    fn full_host() -> HostLogEvidence {
        HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            session_grants: 1,
        }
    }

    fn healthy_evidence() -> CaptureEvidence {
        CaptureEvidence {
            page_id: "mse-fmp4",
            frames: Ok(FrameEvidence {
                presented_frames: 240,
                position_regressions: 0,
                summary_line: "summary".into(),
            }),
            hook: None,
            host: full_host(),
            page_report: Ok(report_with(&healthy_metrics())),
            forbidden_player_lines: Vec::new(),
            page_log: "page log".into(),
            file: None,
        }
    }

    fn growth(before: u64, after: u64) -> FrameGrowth {
        let evidence = |frames| FrameEvidence {
            presented_frames: frames,
            position_regressions: 0,
            summary_line: String::new(),
        };
        FrameGrowth {
            before: evidence(before),
            after: evidence(after),
        }
    }

    #[test]
    fn healthy_capture_passes_and_reports_the_numbers() {
        let verdict = judge_capture(&healthy_evidence());
        assert_eq!(verdict.outcome, Outcome::Pass);
        assert!(verdict.detail.contains("presented_frames=240"));
        assert!(verdict.detail.contains("browser_real_frames=Some(8.0)"));
    }

    #[test]
    fn a_file_page_fails_on_server_side_file_evidence_and_reports_its_requests() {
        let mut evidence = healthy_evidence();
        evidence.file = Some((Vec::new(), "file: background requests=3".into()));
        let verdict = judge_capture(&evidence);
        assert_eq!(verdict.outcome, Outcome::Pass);
        assert!(verdict.detail.contains("file: background requests=3"));
        evidence.file = Some((
            vec!["the page's own <video> kept loading".into()],
            String::new(),
        ));
        assert_eq!(judge_capture(&evidence).outcome, Outcome::Fail);
    }

    #[test]
    fn a_browser_that_keeps_decoding_under_the_shadow_fails() {
        let mut evidence = healthy_evidence();
        evidence.page_report = Ok(report_with(&[
            ("real_probe_available", 1.0),
            ("shadow_window_s", 12.0),
            ("real_frames_in_window", 290.0),
        ]));
        let verdict = judge_capture(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("still plays by itself"));
    }

    #[test]
    fn the_decode_threshold_is_exact_at_the_edge() {
        let window_s = 10.0;
        let edge = max_real_frames(window_s);
        let metrics = |frames: f64| {
            report_with(&[
                ("real_probe_available", 1.0),
                ("shadow_window_s", window_s),
                ("real_frames_in_window", frames),
            ])
        };
        assert!(browser_does_not_decode(&metrics(edge)).is_empty());
        assert_eq!(browser_does_not_decode(&metrics(edge + 1.0)).len(), 1);
    }

    #[test]
    fn missing_metrics_unavailable_probe_and_short_window_are_failures_not_passes() {
        assert_eq!(browser_does_not_decode(&report_with(&[])).len(), 1);
        let unavailable = report_with(&[
            ("real_probe_available", 0.0),
            ("shadow_window_s", 12.0),
            ("real_frames_in_window", 0.0),
        ]);
        assert!(browser_does_not_decode(&unavailable)[0].contains("could not read"));
        let short = report_with(&[
            ("real_probe_available", 1.0),
            ("shadow_window_s", 2.0),
            ("real_frames_in_window", 0.0),
        ]);
        assert!(browser_does_not_decode(&short)[0].contains("too short"));
    }

    #[test]
    fn no_frames_in_the_player_fails_with_the_reason() {
        let mut evidence = healthy_evidence();
        evidence.frames = Err("player did not present frames: timeout".into());
        let verdict = judge_capture(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("did not present frames"));
        assert!(
            verdict.detail.contains("page log"),
            "the failure carries the page log"
        );
    }

    #[test]
    fn position_regressions_fail() {
        let mut evidence = healthy_evidence();
        evidence.frames = Ok(FrameEvidence {
            presented_frames: 240,
            position_regressions: 2,
            summary_line: "summary".into(),
        });
        let verdict = judge_capture(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("went backwards 2 time(s)"));
    }

    #[test]
    fn missing_host_marks_fail() {
        let mut evidence = healthy_evidence();
        evidence.host = HostLogEvidence::default();
        let verdict = judge_capture(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("host did not grant the session"));
    }

    #[test]
    fn a_failed_page_report_fails_even_when_the_player_shows_frames() {
        let mut evidence = healthy_evidence();
        let mut report = report_with(&healthy_metrics());
        report.status = PageStatus::Fail;
        report.checks = vec![ReportCheck {
            name: "page-clock-advances".into(),
            ok: false,
            detail: "currentTime 3 -> 3".into(),
        }];
        evidence.page_report = Ok(report);
        let verdict = judge_capture(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(
            verdict
                .detail
                .contains("page-clock-advances (currentTime 3 -> 3)")
        );
    }

    #[test]
    fn hook_needs_the_player_frames_to_keep_growing() {
        let mut evidence = healthy_evidence();
        evidence.hook = Some(HookEvidence {
            name: "switch-quality".into(),
            growth: Ok(growth(300, 560)),
        });
        assert_eq!(judge_capture(&evidence).outcome, Outcome::Pass);
        evidence.hook = Some(HookEvidence {
            name: "switch-quality".into(),
            growth: Err("player frames did not keep growing".into()),
        });
        let verdict = judge_capture(&evidence);
        assert_eq!(verdict.outcome, Outcome::Fail);
        assert!(verdict.detail.contains("after 'switch-quality'"));
    }

    #[test]
    fn forbidden_player_lines_fail_but_the_known_portal_noise_does_not() {
        let mut evidence = healthy_evidence();
        evidence.forbidden_player_lines =
            vec!["T ERROR XDG Settings Portal did not return response in time".into()];
        assert_eq!(judge_capture(&evidence).outcome, Outcome::Pass);
        evidence
            .forbidden_player_lines
            .push("T ERROR decoder exploded".into());
        assert_eq!(judge_capture(&evidence).outcome, Outcome::Fail);
    }
}
