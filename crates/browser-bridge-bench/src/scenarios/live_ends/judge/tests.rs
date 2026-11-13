//! Тесты приговора `live-ends` на синтетических журналах плеера и отчётах страницы.

use super::*;
use crate::scenarios::outcome::Outcome;
use crate::site::{PageStatus, ReportCheck, RunId};

fn summary(frames: u64) -> String {
    format!(
        "INFO fastiplayer: Browser playback summary kind=periodic presented_frames={frames} position_regressions=0"
    )
}

fn ended(frames: u64) -> String {
    format!("INFO fastiplayer: Browser playback ended presented_frames={frames}")
}

const RECORDING_LINE: &str =
    "INFO fastiplayer: Live timeline became a recording duration_ms=19000 available_from_ms=0";

/// Здоровый журнал: кадры шли, эфир стал записью, ещё ~100 кадров и конец.
fn healthy_journal() -> Vec<String> {
    vec![
        summary(120),
        summary(380),
        RECORDING_LINE.to_owned(),
        ended(490),
    ]
}

fn report(row: LiveEndsRow, metrics: &[(&str, f64)]) -> PageReport {
    PageReport {
        run: RunId::new("r-1").expect("run id"),
        scenario: row.page_id().into(),
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

/// Метрики здоровой страницы: эфир виден, одно изменение длины, длина конечна, `ended` пришёл.
fn healthy_metrics() -> Vec<(&'static str, f64)> {
    vec![
        ("on_air_duration_infinite", 1.0),
        ("duration_changes_after_on_air", 1.0),
        ("final_duration_finite", 1.0),
        ("final_duration_s", 19.0),
        ("ended_seen", 1.0),
    ]
}

fn healthy(row: LiveEndsRow) -> LiveEndsEvidence {
    LiveEndsEvidence {
        row,
        steps: vec![StepEvidence {
            name: "session-granted",
            result: Ok(()),
        }],
        journal: healthy_journal(),
        host: HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            session_grants: 1,
        },
        page_report: Ok(report(row, &healthy_metrics())),
        forbidden_player_lines: Vec::new(),
        page_log: "page log".into(),
    }
}

fn with_metrics(row: LiveEndsRow, metrics: &[(&str, f64)]) -> LiveEndsEvidence {
    let mut evidence = healthy(row);
    evidence.page_report = Ok(report(row, metrics));
    evidence
}

fn failed_with(evidence: &LiveEndsEvidence, needle: &str) {
    let verdict = judge_live_ends(evidence);
    assert_eq!(verdict.outcome, Outcome::Fail, "{}", verdict.detail);
    assert!(
        verdict.detail.contains(needle),
        "expected '{needle}' in: {}",
        verdict.detail
    );
}

#[test]
fn both_rows_pass_on_a_healthy_run_and_show_the_evidence() {
    for row in [LiveEndsRow::MseLiveEnds, LiveEndsRow::HlsLiveEnds] {
        let verdict = judge_live_ends(&healthy(row));
        assert_eq!(
            verdict.outcome,
            Outcome::Pass,
            "{row:?}: {}",
            verdict.detail
        );
        // Кадры вокруг строки перехода и цифры страницы видны в итоге: 380 -> 490 = +110.
        assert!(
            verdict.detail.contains("380 -> Some(490) (+110"),
            "{}",
            verdict.detail
        );
        assert!(
            verdict.detail.contains("final_duration_s=19.00"),
            "{}",
            verdict.detail
        );
    }
}

#[test]
fn a_timeline_that_stayed_live_fails() {
    let mut evidence = healthy(LiveEndsRow::MseLiveEnds);
    evidence.journal = vec![summary(120), summary(380), ended(490)];
    failed_with(&evidence, "no 'Live timeline became a recording' line");
}

#[test]
fn frames_that_did_not_grow_after_the_transition_fail() {
    let mut evidence = healthy(LiveEndsRow::MseLiveEnds);
    // После перехода всего 20 кадров: ниже порога (кадры стоят).
    evidence.journal = vec![summary(380), RECORDING_LINE.to_owned(), ended(400)];
    failed_with(&evidence, "presented only 20 frame(s)");
}

#[test]
fn frames_before_the_transition_do_not_count_as_growth() {
    let mut evidence = healthy(LiveEndsRow::HlsLiveEnds);
    // Много кадров ДО строки, после неё счётчик не вырос.
    evidence.journal = vec![summary(900), RECORDING_LINE.to_owned(), ended(900)];
    failed_with(&evidence, "presented only 0 frame(s)");
}

#[test]
fn a_player_that_never_reached_the_end_fails() {
    let mut evidence = healthy(LiveEndsRow::MseLiveEnds);
    // Кадры выросли (сводка), но строки «доиграло до конца» нет.
    evidence.journal = vec![summary(120), RECORDING_LINE.to_owned(), summary(380)];
    failed_with(&evidence, "never reached the end of the recording");
}

#[test]
fn a_stream_rejection_line_fails_even_when_the_rest_is_healthy() {
    let mut evidence = healthy(LiveEndsRow::HlsLiveEnds);
    evidence.journal.insert(
        1,
        "WARN fastiplayer: Stream generation rejected reason=bad".to_owned(),
    );
    failed_with(&evidence, "the player rejected the stream");
}

#[test]
fn a_failed_robot_step_fails_with_its_name_and_message() {
    let mut evidence = healthy(LiveEndsRow::MseLiveEnds);
    evidence.steps.push(StepEvidence {
        name: "page-sees-the-recording",
        result: Err("no ack".into()),
    });
    failed_with(&evidence, "step 'page-sees-the-recording': no ack");
}

#[test]
fn a_host_without_a_session_and_forbidden_player_lines_fail() {
    let mut evidence = healthy(LiveEndsRow::MseLiveEnds);
    evidence.host.session_granted = false;
    failed_with(&evidence, "host did not grant the session");
    let mut evidence = healthy(LiveEndsRow::MseLiveEnds);
    evidence.forbidden_player_lines = vec!["ERROR something broke".into()];
    failed_with(&evidence, "forbidden player line: ERROR something broke");
}

#[test]
fn a_page_that_never_saw_the_stream_on_air_fails() {
    let mut metrics = healthy_metrics();
    metrics.retain(|(name, _)| *name != "on_air_duration_infinite");
    metrics.push(("on_air_duration_infinite", 0.0));
    failed_with(
        &with_metrics(LiveEndsRow::HlsLiveEnds, &metrics),
        "did not see duration = Infinity while the stream was on air",
    );
}

#[test]
fn a_page_whose_duration_never_became_finite_fails() {
    let metrics = [
        ("on_air_duration_infinite", 1.0),
        ("duration_changes_after_on_air", 1.0),
        ("final_duration_finite", 0.0),
        ("ended_seen", 1.0),
    ];
    failed_with(
        &with_metrics(LiveEndsRow::MseLiveEnds, &metrics),
        "never saw a finite duration",
    );
}

#[test]
fn a_page_that_never_saw_ended_fails() {
    let metrics = [
        ("on_air_duration_infinite", 1.0),
        ("duration_changes_after_on_air", 1.0),
        ("final_duration_finite", 1.0),
        ("ended_seen", 0.0),
    ];
    failed_with(
        &with_metrics(LiveEndsRow::HlsLiveEnds, &metrics),
        "never saw 'ended'",
    );
}

#[test]
fn the_hls_row_wants_exactly_one_duration_change_under_the_shadow() {
    for changes in [0.0, 2.0, 3.0] {
        let mut metrics = healthy_metrics();
        metrics.retain(|(name, _)| *name != "duration_changes_after_on_air");
        metrics.push(("duration_changes_after_on_air", changes));
        failed_with(
            &with_metrics(LiveEndsRow::HlsLiveEnds, &metrics),
            "expected exactly 1",
        );
    }
}

#[test]
fn the_mse_row_accepts_two_duration_changes_but_not_none() {
    let mut two = healthy_metrics();
    two.retain(|(name, _)| *name != "duration_changes_after_on_air");
    two.push(("duration_changes_after_on_air", 2.0));
    let verdict = judge_live_ends(&with_metrics(LiveEndsRow::MseLiveEnds, &two));
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);

    let mut none = healthy_metrics();
    none.retain(|(name, _)| *name != "duration_changes_after_on_air");
    none.push(("duration_changes_after_on_air", 0.0));
    failed_with(
        &with_metrics(LiveEndsRow::MseLiveEnds, &none),
        "saw no durationchange",
    );
}

#[test]
fn a_report_without_the_metrics_fails_naming_each_missing_one() {
    let evidence = with_metrics(LiveEndsRow::HlsLiveEnds, &[]);
    for name in [
        "on_air_duration_infinite",
        "final_duration_finite",
        "ended_seen",
        "duration_changes_after_on_air",
    ] {
        failed_with(&evidence, &format!("no metric '{name}'"));
    }
}

#[test]
fn a_failing_page_report_and_a_report_of_another_page_fail() {
    let mut evidence = healthy(LiveEndsRow::MseLiveEnds);
    if let Ok(page) = evidence.page_report.as_mut() {
        page.status = PageStatus::Fail;
        page.checks.push(ReportCheck {
            name: "recording-reaches-ended".into(),
            ok: false,
            detail: "duration=19 ended=false".into(),
        });
    }
    failed_with(
        &evidence,
        "recording-reaches-ended (duration=19 ended=false)",
    );

    let mut evidence = healthy(LiveEndsRow::MseLiveEnds);
    evidence.page_report = Ok(report(LiveEndsRow::HlsLiveEnds, &healthy_metrics()));
    failed_with(&evidence, "expected 'mse-live-ends'");
}

#[test]
fn a_missing_page_report_fails_and_the_summary_says_so() {
    let mut evidence = healthy(LiveEndsRow::MseLiveEnds);
    evidence.page_report = Err(SiteError::ReportTimeout {
        run: RunId::new("r-1").expect("run id"),
        waited: std::time::Duration::from_secs(30),
    });
    failed_with(&evidence, "page:");
    failed_with(&evidence, "page metrics: no report");
}

#[test]
fn a_failure_carries_the_page_log_and_a_pass_does_not() {
    let pass = judge_live_ends(&healthy(LiveEndsRow::MseLiveEnds));
    assert!(!pass.detail.contains("page log"), "{}", pass.detail);
    let mut evidence = healthy(LiveEndsRow::MseLiveEnds);
    evidence.journal.clear();
    failed_with(&evidence, "page log");
}
