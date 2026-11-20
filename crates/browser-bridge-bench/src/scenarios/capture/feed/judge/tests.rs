//! Тесты приговора ленты: здоровые улики дают «пройдено», а каждое нарушение — свой провал.

use super::super::switch_judge::{healthy_switch_lines, switch_line};
use super::*;
use crate::observe::FrameEvidence;
use crate::scenarios::outcome::Outcome;
use crate::site::{PageStatus, RunId};

const REEL_WIDTHS: &[u64] = &[640, 512, 384];
const FOLLOWS: ExpectedWindow = ExpectedWindow::FollowsFeed {
    reel_stream_widths: REEL_WIDTHS,
};
const STAYS: ExpectedWindow = ExpectedWindow::StaysOnFirstReel { stream_width: 640 };

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
        scenario: "feed".into(),
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

fn good_scroll() -> ScrollRecord {
    ScrollRecord {
        trusted_wheel_events: 5,
        page_started_next_reel: Ok(()),
        player_loaded_new_generation: Ok(()),
        frame_growth: Ok(growth(200)),
        frame_growth_check: FrameGrowthCheck::Required,
    }
}

/// Здоровая лента из трёх роликов: две прокрутки, окно прошло ширины 640 -> 512 -> 384.
fn following() -> FeedEvidence {
    FeedEvidence {
        page_id: "feed",
        hand_off: Ok(HandOffOutcome::Started),
        host: HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            session_grants: 1,
        },
        first_reel_frames: Ok(()),
        scrolls: vec![good_scroll(), good_scroll()],
        stay: None,
        page_download: None,
        edge: None,
        back: None,
        history_back: None,
        target_wait_lines: Vec::new(),
        media_switch_lines: healthy_switch_lines(),
        generations: vec![1, 2, 3],
        // Плеер может написать строку про один ролик дважды: подряд идущие повторы не в счёт.
        shown_stream_widths: vec![640, 640, 512, 512, 384],
        end_notice_lines: Vec::new(),
        forbidden_player_lines: Vec::new(),
        page_report: Ok(report(&[
            ("reels_started", 3.0),
            ("wheel_events_trusted", 10.0),
        ])),
        page_log: "page log".into(),
    }
}

/// Здоровая отрицательная строка: раздражитель был, поколение одно.
fn staying() -> FeedEvidence {
    FeedEvidence {
        scrolls: Vec::new(),
        stay: Some(StayRecord {
            stimulus: Ok(()),
            frame_growth: Ok(growth(250)),
        }),
        generations: vec![1],
        shown_stream_widths: vec![640, 640],
        page_report: Ok(report(&[
            ("main_paused", 0.0),
            ("preview_playing", 1.0),
            ("preview_started", 1.0),
        ])),
        ..following()
    }
}

fn failure_of(expected: ExpectedWindow, evidence: &FeedEvidence) -> String {
    let verdict = judge_feed(expected, evidence);
    assert_eq!(verdict.outcome, Outcome::Fail, "{}", verdict.detail);
    verdict.detail
}

#[test]
fn a_window_that_follows_the_feed_through_every_reel_passes() {
    let verdict = judge_feed(FOLLOWS, &following());
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    // В отчёт идёт то, что видело окно, без дубля строки качества.
    assert!(
        verdict.detail.contains("[640, 512, 384]"),
        "{}",
        verdict.detail
    );
}

#[test]
fn a_hand_off_that_did_not_start_is_a_failure() {
    let mut evidence = following();
    evidence.hand_off = Ok(HandOffOutcome::NoVideo);
    assert!(failure_of(FOLLOWS, &evidence).contains("hand_off_tab of reel 1"));
}

#[test]
fn a_second_session_means_the_window_was_not_the_same() {
    let mut evidence = following();
    evidence.host.session_grants = 2;
    assert!(failure_of(FOLLOWS, &evidence).contains("expected exactly one"));
}

#[test]
fn a_session_end_notice_is_a_failure() {
    let mut evidence = following();
    evidence.end_notice_lines = vec!["Browser session end notice shown notice=x".into()];
    assert!(failure_of(FOLLOWS, &evidence).contains("session end notice"));
}

#[test]
fn a_window_that_waited_out_the_deadline_for_the_start_of_a_reel_is_a_failure() {
    // Поколения, ширины и кадры в порядке: окно в итоге переехало, но зритель 10 с ждал и ролик встал с середины.
    let mut evidence = following();
    evidence.target_wait_lines =
        vec!["The new stream generation got no data at its target; it starts where data exists waited_ms=10000".into()];
    let detail = failure_of(FOLLOWS, &evidence);
    assert!(detail.contains("waited out the whole deadline"), "{detail}");
    assert!(detail.contains("waited_ms=10000"), "{detail}");
}

#[test]
fn a_long_download_row_passes_only_when_the_page_did_download() {
    let mut evidence = following();
    evidence.page_download = Some(Ok(()));
    let verdict = judge_feed(FOLLOWS, &evidence);
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);

    evidence.page_download = Some(Err("the page never logged 'earlier-reel-downloaded'".into()));
    let detail = failure_of(FOLLOWS, &evidence);
    assert!(
        detail.contains("did not download past the bridge memory"),
        "{detail}"
    );
}

#[test]
fn a_wheel_that_never_reached_the_page_is_reported_apart_from_a_missed_relocation() {
    let mut evidence = following();
    evidence.scrolls[0].trusted_wheel_events = 0;
    let detail = failure_of(FOLLOWS, &evidence);
    assert!(
        detail.contains("real mouse wheel never reached the page"),
        "{detail}"
    );
    assert!(!detail.contains("did not relocate"), "{detail}");
}

#[test]
fn a_page_that_did_not_start_the_next_reel_is_a_failure() {
    let mut evidence = following();
    evidence.scrolls[1].page_started_next_reel = Err("timed out".into());
    assert!(failure_of(FOLLOWS, &evidence).contains("the page did not start the next reel"));
}

#[test]
fn a_window_that_did_not_relocate_is_a_failure() {
    let mut evidence = following();
    evidence.scrolls[0].player_loaded_new_generation = Err("no new generation".into());
    evidence.generations = vec![1];
    evidence.shown_stream_widths = vec![640];
    let detail = failure_of(FOLLOWS, &evidence);
    assert!(
        detail.contains("did not relocate to the next reel"),
        "{detail}"
    );
    assert!(detail.contains("expected [1, 2, 3]"), "{detail}");
}

#[test]
fn an_extra_or_missing_load_is_a_failure() {
    let mut evidence = following();
    evidence.generations = vec![1, 2, 3, 4];
    assert!(failure_of(FOLLOWS, &evidence).contains("no extra and none missing"));
}

#[test]
fn the_wrong_reel_in_the_window_is_a_failure() {
    // Загрузок три, но окно второй раз показало первый ролик (640), а не второй.
    let mut evidence = following();
    evidence.shown_stream_widths = vec![640, 640, 384];
    let detail = failure_of(FOLLOWS, &evidence);
    assert!(
        detail.contains("showed stream widths [640, 384]"),
        "{detail}"
    );
}

#[test]
fn frames_that_stopped_growing_after_a_move_are_a_failure() {
    let mut evidence = following();
    evidence.scrolls[0].frame_growth = Ok(growth(10));
    assert!(failure_of(FOLLOWS, &evidence).contains("grew by only 10"));
    evidence.scrolls[0].frame_growth = Err("timed out".into());
    assert!(failure_of(FOLLOWS, &evidence).contains("did not grow"));
}

#[test]
fn a_page_that_started_fewer_reels_than_the_robot_scrolled_is_a_failure() {
    let mut evidence = following();
    evidence.page_report = Ok(report(&[
        ("reels_started", 2.0),
        ("wheel_events_trusted", 10.0),
    ]));
    assert!(failure_of(FOLLOWS, &evidence).contains("started 2 reel(s), expected 3"));
}

#[test]
fn a_failed_page_report_is_a_failure() {
    let mut evidence = following();
    evidence.page_report = Err(SiteError::ReportTimeout {
        run: RunId::new("r-1").expect("run id"),
        waited: std::time::Duration::from_secs(1),
    });
    assert!(failure_of(FOLLOWS, &evidence).contains("page:"));
}

#[test]
fn a_window_that_stays_on_its_reel_after_the_stimulus_passes() {
    let verdict = judge_feed(STAYS, &staying());
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
}

#[test]
fn a_relocation_after_a_stimulus_that_must_not_move_the_window_is_a_failure() {
    let mut evidence = staying();
    evidence.generations = vec![1, 2];
    evidence.shown_stream_widths = vec![640, 512];
    let detail = failure_of(STAYS, &evidence);
    assert!(detail.contains("the window relocated"), "{detail}");
    assert!(
        detail.contains("showed stream widths [640, 512]"),
        "{detail}"
    );
}

#[test]
fn a_row_whose_stimulus_never_happened_proves_nothing() {
    let mut evidence = staying();
    evidence.stay.as_mut().expect("stay").stimulus = Err("the preview never played".into());
    assert!(failure_of(STAYS, &evidence).contains("proves nothing"));
}

#[test]
fn a_negative_row_without_any_record_is_a_failure() {
    let mut evidence = staying();
    evidence.stay = None;
    assert!(failure_of(STAYS, &evidence).contains("recorded no stimulus"));
}

#[test]
fn a_stay_row_where_the_page_paused_our_video_is_not_the_case_under_test() {
    let mut evidence = staying();
    evidence.page_report = Ok(report(&[("main_paused", 1.0), ("preview_playing", 1.0)]));
    assert!(failure_of(STAYS, &evidence).contains("sees our video paused"));
}

#[test]
fn a_stay_row_whose_frames_froze_is_a_failure() {
    let mut evidence = staying();
    evidence.stay.as_mut().expect("stay").frame_growth = Ok(growth(0));
    assert!(failure_of(STAYS, &evidence).contains("grew by only 0"));
}

#[test]
fn a_feed_whose_new_reels_came_with_an_empty_frame_fails() {
    // Три ролика, смены 2 и 3; на смене 3 на экран ушёл кадр без видео.
    let mut evidence = following();
    evidence.media_switch_lines = vec![
        switch_line(1, "first_frame", "no_previous_frame", 0, Some(90)),
        switch_line(2, "first_frame", "held", 0, Some(120)),
        switch_line(3, "first_frame", "disabled", 1, Some(130)),
    ];
    let detail = failure_of(FOLLOWS, &evidence);
    assert!(detail.contains("media switch 3: 1 empty frame"), "{detail}");
}

#[test]
fn an_old_player_without_switch_lines_fails_a_row_that_relocates() {
    let mut evidence = following();
    evidence.media_switch_lines = Vec::new();
    let detail = failure_of(FOLLOWS, &evidence);
    assert!(detail.contains("needs 2"), "{detail}");
}

#[test]
fn a_row_that_stays_on_the_first_reel_is_not_judged_for_switches() {
    // Окно осталось на первом ролике: смен нет, строк журнала смен может не быть вовсе.
    let mut evidence = staying();
    evidence.media_switch_lines = Vec::new();
    let verdict = judge_feed(STAYS, &evidence);
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
}
