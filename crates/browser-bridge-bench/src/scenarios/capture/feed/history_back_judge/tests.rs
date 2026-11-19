//! Тесты приговора «Назад» в истории: здоровые улики дают «пройдено», а каждое нарушение — свой провал. Каждый
//! тест ломает ОДНО свидетельство здорового прогона и требует, чтобы приговор назвал именно его.

use super::super::judge::{ExpectedWindow, judge_feed};
use super::*;
use crate::browsers::HandOffOutcome;
use crate::observe::{EndNotice, FrameEvidence, HostLogEvidence};
use crate::scenarios::outcome::Outcome;
use crate::site::{PageReport, PageStatus, RunId};

const PICKUP: ExpectedWindow = ExpectedWindow::PicksUpAfterHistoryBack;

fn growth(gained: u64) -> FrameGrowth {
    let evidence = |frames| FrameEvidence {
        presented_frames: frames,
        position_regressions: 0,
        summary_line: format!("summary frames={frames}"),
    };
    FrameGrowth {
        before: evidence(100),
        after: evidence(100 + gained),
    }
}

fn report(metrics: &[(&str, f64)]) -> PageReport {
    PageReport {
        run: RunId::new("r-1").expect("run id"),
        scenario: "feed-edges".into(),
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

fn paused_end_notice() -> EndNotice {
    EndNotice {
        notice: "waiting_for_viewer".into(),
        cause: "tab_gone".into(),
        end: "none".into(),
        line: "Browser session end notice shown notice=waiting_for_viewer cause=tab_gone end=none"
            .into(),
    }
}

/// Журнал плеера здорового подхвата: одно окно, сеанс 1 (ролики 1 и 2), сеанс 2 (ролик 2 с начала).
fn healthy_lines() -> Vec<String> {
    [
        "INFO Browser window created",
        "INFO Browser session opened intent=OpenNew grant=Started",
        "INFO Browser stream generation loaded generation=1",
        "INFO Browser stream generation shown generation=1 start_ms=0",
        "INFO Output surface pool reconfigured for stream width=640 height=368 rt_format=1",
        "INFO Browser stream generation loaded generation=2",
        "INFO Browser stream generation shown generation=2 start_ms=0",
        "INFO Output surface pool reconfigured for stream width=512 height=288 rt_format=1",
        "INFO Browser session opened intent=OpenNew grant=Started",
        "INFO Browser stream generation loaded generation=1",
        "INFO Browser stream generation shown generation=1 start_ms=0",
        "INFO Output surface pool reconfigured for stream width=512 height=288 rt_format=1",
    ]
    .iter()
    .map(|line| (*line).to_owned())
    .collect()
}

fn healthy() -> FeedEvidence {
    FeedEvidence {
        page_id: "feed-edges",
        hand_off: Ok(HandOffOutcome::Started),
        host: HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            session_grants: 2,
        },
        first_reel_frames: Ok(()),
        scrolls: Vec::new(),
        stay: None,
        page_download: None,
        edge: None,
        back: None,
        history_back: Some(HistoryBackRecord {
            stimulus: Ok(()),
            end: EndRecord {
                notice: Ok(paused_end_notice()),
                window_after_end: Ok(()),
            },
            player_lines: healthy_lines(),
            frame_growth: Ok(growth(200)),
            play_rejections: Vec::new(),
        }),
        target_wait_lines: Vec::new(),
        generations: vec![1, 2, 1],
        shown_stream_widths: vec![640, 512],
        end_notice_lines: vec![paused_end_notice().line],
        forbidden_player_lines: Vec::new(),
        page_report: Ok(report(&[("reel_back_started", 1.0)])),
        page_log: "page log".into(),
    }
}

fn record_mut(evidence: &mut FeedEvidence) -> &mut HistoryBackRecord {
    evidence
        .history_back
        .as_mut()
        .expect("the fixture has a history-back record")
}

fn failure_of(evidence: &FeedEvidence) -> String {
    let verdict = judge_feed(PICKUP, evidence);
    assert_eq!(verdict.outcome, Outcome::Fail, "{}", verdict.detail);
    verdict.detail
}

#[test]
fn a_pickup_in_the_same_window_from_the_beginning_passes() {
    let verdict = judge_feed(PICKUP, &healthy());
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
}

#[test]
fn a_second_window_fails() {
    let mut evidence = healthy();
    let lines = &mut record_mut(&mut evidence).player_lines;
    lines.insert(8, "INFO Browser window created".into());
    assert!(failure_of(&evidence).contains("created 2 window(s)"));
}

#[test]
fn a_run_without_the_pickup_session_fails() {
    // Старый экстеншен: второго сеанса нет, ролик играет в браузере.
    let mut evidence = healthy();
    record_mut(&mut evidence).player_lines.truncate(8);
    record_mut(&mut evidence).frame_growth = Err("not awaited: no second session".into());
    evidence.host.session_grants = 1;
    let detail = failure_of(&evidence);
    assert!(detail.contains("opened 1 session(s)"), "{detail}");
    assert!(
        detail.contains("showed nothing after the pickup"),
        "{detail}"
    );
    assert!(detail.contains("granted 1 session(s)"), "{detail}");
}

#[test]
fn a_pickup_session_with_another_grant_fails() {
    let mut evidence = healthy();
    record_mut(&mut evidence).player_lines[8] =
        "INFO Browser session opened intent=OpenNew grant=ReplacedPrevious".into();
    assert!(failure_of(&evidence).contains("not opened with grant=Started"));
}

#[test]
fn a_preview_taken_for_a_reel_before_the_pickup_fails() {
    let mut evidence = healthy();
    record_mut(&mut evidence).player_lines.insert(
        8,
        "INFO Browser stream generation loaded generation=3".into(),
    );
    assert!(failure_of(&evidence).contains("expected [1, 2]"));
}

#[test]
fn a_pickup_that_shows_nothing_fails() {
    let mut evidence = healthy();
    record_mut(&mut evidence).player_lines.truncate(9);
    assert!(failure_of(&evidence).contains("showed nothing after the pickup"));
}

#[test]
fn a_pickup_that_does_not_start_from_the_beginning_fails() {
    let mut evidence = healthy();
    record_mut(&mut evidence).player_lines[10] =
        "INFO Browser stream generation shown generation=1 start_ms=7300".into();
    assert!(failure_of(&evidence).contains("must play from the beginning"));
}

#[test]
fn a_preview_taken_after_the_pickup_fails() {
    let mut evidence = healthy();
    let lines = &mut record_mut(&mut evidence).player_lines;
    lines.push("INFO Browser stream generation loaded generation=2".into());
    lines.push("INFO Browser stream generation shown generation=2 start_ms=0".into());
    lines.push(
        "INFO Output surface pool reconfigured for stream width=320 height=192 rt_format=1".into(),
    );
    let detail = failure_of(&evidence);
    assert!(
        detail.contains("showed 2 generations after the pickup"),
        "{detail}"
    );
    assert!(detail.contains("expected only 512"), "{detail}");
}

#[test]
fn a_pickup_with_the_wrong_reel_width_fails() {
    let mut evidence = healthy();
    record_mut(&mut evidence).player_lines[11] =
        "INFO Output surface pool reconfigured for stream width=640 height=368 rt_format=1".into();
    assert!(failure_of(&evidence).contains("expected only 512"));
}

#[test]
fn frozen_frames_after_the_pickup_fail() {
    let mut evidence = healthy();
    record_mut(&mut evidence).frame_growth = Ok(growth(2));
    assert!(failure_of(&evidence).contains("after the pickup: player frames grew by only 2"));
}

#[test]
fn a_first_session_that_did_not_end_honestly_fails() {
    let mut evidence = healthy();
    record_mut(&mut evidence).end.notice = Err("no notice".into());
    assert!(failure_of(&evidence).contains("did not end honestly"));
    let mut evidence = healthy();
    record_mut(&mut evidence).end.window_after_end = Err("window gone".into());
    assert!(failure_of(&evidence).contains("the player window after the end: window gone"));
}

#[test]
fn a_row_where_the_page_did_not_come_back_proves_nothing() {
    let mut evidence = healthy();
    record_mut(&mut evidence).stimulus = Err("never logged 'history-back-popstate'".into());
    assert!(failure_of(&evidence).contains("so the row proves nothing"));
}

#[test]
fn an_abort_error_from_play_fails() {
    let mut evidence = healthy();
    record_mut(&mut evidence).play_rejections =
        vec!["9 play-rejected back reel 2: AbortError".into()];
    assert!(failure_of(&evidence).contains("AbortError from play()"));
}

#[test]
fn a_page_that_reports_no_return_fails() {
    let mut evidence = healthy();
    evidence.page_report = Ok(report(&[("reel_back_started", 0.0)]));
    assert!(failure_of(&evidence).contains("never saw the returned reel playing"));
}

#[test]
fn a_row_without_history_back_evidence_fails() {
    let mut evidence = healthy();
    evidence.history_back = None;
    assert!(failure_of(&evidence).contains("no history-back evidence"));
}
