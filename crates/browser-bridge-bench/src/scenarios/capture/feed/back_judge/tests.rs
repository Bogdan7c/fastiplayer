//! Тесты приговора строк «назад»: здоровые улики дают «пройдено», а каждое нарушение — свой провал. Каждый
//! тест ломает ОДНО свидетельство здорового прогона и требует, чтобы приговор назвал именно его.

use super::super::judge::{ExpectedWindow, FrameGrowthCheck, judge_feed};
use super::*;
use crate::browsers::HandOffOutcome;
use crate::observe::{FrameEvidence, FrameGrowth, HostLogEvidence};
use crate::scenarios::outcome::Outcome;
use crate::site::{PageReport, PageStatus, RunId};

const WIDTHS: &[u64] = &[640, 512, 640];
const LIVE: ExpectedWindow = ExpectedWindow::ReturnsToEarlierReel {
    reel_stream_widths: WIDTHS,
    start: BackStart::NearPausedSecond,
};
const RECREATED: ExpectedWindow = ExpectedWindow::ReturnsToEarlierReel {
    reel_stream_widths: WIDTHS,
    start: BackStart::FromTheBeginning,
};
const FAR_WIDTHS: &[u64] = &[640, 512, 384, 640];
const MEASURED_FAR: ExpectedWindow = ExpectedWindow::ReturnsToEarlierReel {
    reel_stream_widths: FAR_WIDTHS,
    start: BackStart::MeasuredFar,
};
const MEASURED: ExpectedWindow = ExpectedWindow::ReturnsToEarlierReel {
    reel_stream_widths: WIDTHS,
    start: BackStart::Measured,
};

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

fn good_scroll() -> ScrollRecord {
    ScrollRecord {
        trusted_wheel_events: 5,
        page_started_next_reel: Ok(()),
        player_loaded_new_generation: Ok(()),
        frame_growth: Ok(growth(200)),
        frame_growth_check: FrameGrowthCheck::Required,
    }
}

fn shown(generation: u64, start_ms: u64) -> ShownGeneration {
    ShownGeneration {
        generation,
        start_ms,
    }
}

/// Здоровый возврат живого элемента: страница остановила ролик 1 на 3900 мс, поколение 3 идёт с 4100 мс.
fn live() -> FeedEvidence {
    FeedEvidence {
        page_id: "feed-edges",
        hand_off: Ok(HandOffOutcome::Started),
        host: HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            session_grants: 1,
        },
        first_reel_frames: Ok(()),
        scrolls: vec![good_scroll()],
        stay: None,
        page_download: None,
        edge: None,
        back: Some(BackRecord {
            stimulus: Ok(()),
            forward_reels: 2,
            paused_at_ms: Some(3900),
            scroll_back: good_scroll(),
            shown: vec![shown(1, 0), shown(2, 0), shown(3, 4100)],
            page_saw_playing: true,
            window_responds: Ok(()),
            play_rejections: Vec::new(),
        }),
        history_back: None,
        target_wait_lines: Vec::new(),
        generations: vec![1, 2, 3],
        shown_stream_widths: vec![640, 640, 512, 640],
        end_notice_lines: Vec::new(),
        forbidden_player_lines: Vec::new(),
        page_report: Ok(report(&[
            ("reels_started", 2.0),
            ("reel_back_started", 1.0),
            ("wheel_events_trusted", 10.0),
        ])),
        page_log: "page log".into(),
    }
}

/// Здоровый возврат с новым MediaSource: поколение 3 идёт с нуля.
fn recreated() -> FeedEvidence {
    let mut evidence = live();
    let back = evidence.back.as_mut().expect("back record");
    back.shown = vec![shown(1, 0), shown(2, 0), shown(3, 0)];
    evidence
}

fn back_mut(evidence: &mut FeedEvidence) -> &mut BackRecord {
    evidence
        .back
        .as_mut()
        .expect("the fixture has a back record")
}

fn failure_of(expected: ExpectedWindow, evidence: &FeedEvidence) -> String {
    let verdict = judge_feed(expected, evidence);
    assert_eq!(verdict.outcome, Outcome::Fail, "{}", verdict.detail);
    verdict.detail
}

fn wait_line(waited_ms: u64) -> String {
    format!(
        "WARN The new stream generation got no data at its target; it starts where data exists waited_ms={waited_ms}"
    )
}

#[test]
fn a_live_element_that_continues_from_its_paused_second_passes_and_reports_the_start() {
    let verdict = judge_feed(LIVE, &live());
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    assert!(
        verdict
            .detail
            .contains("back_start_ms=4100 back_waited_ms=0 outcome=immediate"),
        "{}",
        verdict.detail
    );
}

#[test]
fn a_live_element_that_restarts_from_zero_fails() {
    let mut evidence = live();
    back_mut(&mut evidence).shown = vec![shown(1, 0), shown(2, 0), shown(3, 0)];
    assert!(failure_of(LIVE, &evidence).contains("must continue from where the page paused it"));
}

#[test]
fn a_live_element_that_starts_far_from_the_paused_second_fails() {
    let mut evidence = live();
    back_mut(&mut evidence).shown = vec![shown(1, 0), shown(2, 0), shown(3, 9000)];
    assert!(failure_of(LIVE, &evidence).contains("far from the 3900 ms"));
}

#[test]
fn a_live_row_without_the_paused_second_proves_nothing() {
    let mut evidence = live();
    back_mut(&mut evidence).paused_at_ms = None;
    assert!(failure_of(LIVE, &evidence).contains("never logged the second it paused reel 1"));
}

#[test]
fn a_live_row_where_reel_one_barely_played_cannot_tell_resume_from_restart() {
    let mut evidence = live();
    back_mut(&mut evidence).paused_at_ms = Some(400);
    back_mut(&mut evidence).shown = vec![shown(1, 0), shown(2, 0), shown(3, 400)];
    assert!(failure_of(LIVE, &evidence).contains("played only 400 ms"));
}

#[test]
fn a_live_row_fails_when_the_window_waited_for_data_at_the_target() {
    let mut evidence = live();
    evidence.target_wait_lines = vec![wait_line(10003)];
    assert!(failure_of(LIVE, &evidence).contains("waited out the whole deadline"));
}

#[test]
fn a_recreated_reel_that_starts_from_the_beginning_passes() {
    let verdict = judge_feed(RECREATED, &recreated());
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
}

#[test]
fn a_recreated_reel_that_starts_in_the_middle_fails() {
    let mut evidence = recreated();
    back_mut(&mut evidence).shown = vec![shown(1, 0), shown(2, 0), shown(3, 3900)];
    assert!(failure_of(RECREATED, &evidence).contains("must start from the beginning"));
}

#[test]
fn a_return_where_the_window_never_showed_the_new_generation_fails() {
    let mut evidence = recreated();
    back_mut(&mut evidence).shown = vec![shown(1, 0), shown(2, 0)];
    assert!(failure_of(RECREATED, &evidence).contains("never showed generation 3"));
}

#[test]
fn a_return_with_wrong_generations_fails() {
    let mut evidence = recreated();
    evidence.generations = vec![1, 2];
    assert!(failure_of(RECREATED, &evidence).contains("expected [1, 2, 3]"));
}

#[test]
fn a_return_that_shows_the_wrong_widths_fails() {
    let mut evidence = recreated();
    evidence.shown_stream_widths = vec![640, 512, 512];
    assert!(failure_of(RECREATED, &evidence).contains("expected [640, 512, 640]"));
}

#[test]
fn a_return_that_ends_the_session_fails() {
    let mut evidence = recreated();
    evidence.end_notice_lines = vec!["Browser session end notice shown notice=error".into()];
    assert!(failure_of(RECREATED, &evidence).contains("session end notice"));
}

#[test]
fn a_return_where_the_page_did_not_come_back_the_planned_way_fails() {
    let mut evidence = recreated();
    back_mut(&mut evidence).stimulus = Err("never logged 'element=new'".into());
    assert!(failure_of(RECREATED, &evidence).contains("did not call play() of the earlier reel"));
}

#[test]
fn a_return_where_the_wheel_never_reached_the_page_fails() {
    let mut evidence = recreated();
    back_mut(&mut evidence).scroll_back.trusted_wheel_events = 0;
    assert!(failure_of(RECREATED, &evidence).contains("scroll back (reel 2 -> 1)"));
}

#[test]
fn a_return_where_frames_do_not_grow_fails() {
    let mut evidence = recreated();
    back_mut(&mut evidence).scroll_back.frame_growth = Ok(growth(3));
    assert!(
        failure_of(RECREATED, &evidence)
            .contains("scroll back (reel 2 -> 1): player frames grew by only 3")
    );
}

#[test]
fn a_return_without_the_forward_scroll_record_fails() {
    let mut evidence = recreated();
    evidence.scrolls.clear();
    assert!(failure_of(RECREATED, &evidence).contains("0 forward scroll(s)"));
}

#[test]
fn a_page_that_reports_no_return_fails() {
    let mut evidence = recreated();
    evidence.page_report = Ok(report(&[
        ("reels_started", 2.0),
        ("reel_back_started", 0.0),
        ("wheel_events_trusted", 10.0),
    ]));
    assert!(failure_of(RECREATED, &evidence).contains("returned to a reel 0 time(s)"));
}

#[test]
fn a_row_without_back_evidence_fails() {
    let mut evidence = recreated();
    evidence.back = None;
    assert!(failure_of(RECREATED, &evidence).contains("no back-scroll evidence"));
}

#[test]
fn the_measuring_row_passes_when_the_window_waited_and_records_the_numbers() {
    let mut evidence = live();
    evidence.target_wait_lines = vec![wait_line(10003)];
    back_mut(&mut evidence).shown = vec![shown(1, 0), shown(2, 0), shown(3, 0)];
    let verdict = judge_feed(MEASURED, &evidence);
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    assert!(
        verdict
            .detail
            .contains("back_start_ms=0 back_waited_ms=10003 outcome=waited"),
        "{}",
        verdict.detail
    );
}

#[test]
fn the_measuring_row_passes_when_the_reel_played_at_once() {
    let verdict = judge_feed(MEASURED, &live());
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    assert!(
        verdict.detail.contains("outcome=immediate"),
        "{}",
        verdict.detail
    );
}

#[test]
fn the_measuring_row_fails_with_outcome_never_when_the_window_never_showed_the_reel() {
    let mut evidence = live();
    back_mut(&mut evidence).shown = vec![shown(1, 0), shown(2, 0)];
    back_mut(&mut evidence)
        .scroll_back
        .player_loaded_new_generation = Err("no".into());
    back_mut(&mut evidence).scroll_back.frame_growth = Err("not awaited".into());
    let detail = failure_of(MEASURED, &evidence);
    assert!(detail.contains("outcome never"), "{detail}");
    assert!(detail.contains("outcome=never"), "{detail}");
}

#[test]
fn the_measuring_row_fails_with_outcome_never_when_frames_stay_frozen() {
    let mut evidence = live();
    back_mut(&mut evidence).scroll_back.frame_growth = Ok(growth(0));
    let detail = failure_of(MEASURED, &evidence);
    assert!(detail.contains("outcome never"), "{detail}");
}

#[test]
fn the_measuring_row_still_fails_on_the_general_failures() {
    let mut evidence = live();
    evidence.end_notice_lines = vec!["Browser session end notice shown".into()];
    assert!(failure_of(MEASURED, &evidence).contains("session end notice"));
    let mut evidence = live();
    evidence.host.session_grants = 2;
    assert!(failure_of(MEASURED, &evidence).contains("expected exactly one"));
}

#[test]
fn the_paused_second_is_read_from_the_last_page_log_line() {
    let lines = vec![
        "12 reel-paused n=1 at_ms=3100".to_owned(),
        "40 reel-paused n=1 at_ms=3921".to_owned(),
    ];
    assert_eq!(parse_paused_at_ms(&lines), Some(3921));
    assert_eq!(parse_paused_at_ms(&[]), None);
    assert_eq!(
        parse_paused_at_ms(&["reel-paused n=1 at_ms=".to_owned()]),
        None
    );
}

/// Здоровый дальний возврат: окно взяло ролик 1 сразу (ролики 1, 2, 3 вперёд, затем ролик 1).
fn far_immediate() -> FeedEvidence {
    let mut evidence = live();
    evidence.scrolls = vec![good_scroll(), good_scroll()];
    evidence.generations = vec![1, 2, 3, 4];
    evidence.shown_stream_widths = vec![640, 512, 384, 640];
    let back = back_mut(&mut evidence);
    back.forward_reels = 3;
    back.shown = vec![shown(1, 0), shown(2, 0), shown(3, 0), shown(4, 14000)];
    evidence.page_report = Ok(report(&[
        ("reels_started", 3.0),
        ("reel_back_started", 1.0),
        ("wheel_events_trusted", 12.0),
    ]));
    evidence
}

/// Решение 378: окно ролик не взяло (новое поколение не загружено), сеанс жив, страница играет ролик сама.
fn far_played_in_browser() -> FeedEvidence {
    let mut evidence = far_immediate();
    evidence.generations = vec![1, 2, 3];
    evidence.shown_stream_widths = vec![640, 512, 384];
    let back = back_mut(&mut evidence);
    back.shown = vec![shown(1, 0), shown(2, 0), shown(3, 0)];
    back.scroll_back.player_loaded_new_generation = Err("no new generation".into());
    back.scroll_back.frame_growth = Err("not awaited".into());
    evidence
}

#[test]
fn the_far_measuring_row_passes_when_the_window_plays_reel_one_at_once() {
    let verdict = judge_feed(MEASURED_FAR, &far_immediate());
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    assert!(
        verdict.detail.contains("outcome=immediate"),
        "{}",
        verdict.detail
    );
}

#[test]
fn the_far_measuring_row_passes_when_the_page_plays_the_reel_in_the_browser() {
    let verdict = judge_feed(MEASURED_FAR, &far_played_in_browser());
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    assert!(
        verdict.detail.contains("outcome=played_in_browser"),
        "{}",
        verdict.detail
    );
}

#[test]
fn played_in_browser_fails_when_the_page_does_not_see_the_reel_playing() {
    // Окно ролик не взяло и страница его не играет: это «never», а не решение 378.
    let mut evidence = far_played_in_browser();
    back_mut(&mut evidence).page_saw_playing = false;
    let detail = failure_of(MEASURED_FAR, &evidence);
    assert!(detail.contains("outcome never"), "{detail}");
}

#[test]
fn played_in_browser_fails_when_the_window_does_not_respond() {
    let mut evidence = far_played_in_browser();
    back_mut(&mut evidence).window_responds = Err("window gone".into());
    let detail = failure_of(MEASURED_FAR, &evidence);
    assert!(detail.contains("outcome never"), "{detail}");
}

#[test]
fn played_in_browser_fails_when_the_session_ended() {
    let mut evidence = far_played_in_browser();
    evidence.end_notice_lines = vec!["Browser session end notice shown notice=stream_error".into()];
    assert!(failure_of(MEASURED_FAR, &evidence).contains("session end notice"));
}

#[test]
fn the_far_measuring_row_fails_when_the_window_waited_for_data() {
    let mut evidence = far_immediate();
    evidence.target_wait_lines = vec![wait_line(10003)];
    let detail = failure_of(MEASURED_FAR, &evidence);
    assert!(detail.contains("outcome waited"), "{detail}");
    assert!(detail.contains("outcome=waited"), "{detail}");
}

#[test]
fn the_far_measuring_row_fails_with_never_when_nothing_plays() {
    let mut evidence = far_played_in_browser();
    let back = back_mut(&mut evidence);
    back.page_saw_playing = false;
    back.window_responds = Err("gone".into());
    assert!(failure_of(MEASURED_FAR, &evidence).contains("outcome never"));
}

#[test]
fn the_pressure_row_accepts_played_in_browser_and_names_it() {
    let mut evidence = live();
    evidence.generations = vec![1, 2];
    evidence.shown_stream_widths = vec![640, 512];
    let back = back_mut(&mut evidence);
    back.shown = vec![shown(1, 0), shown(2, 0)];
    back.scroll_back.player_loaded_new_generation = Err("no new generation".into());
    back.scroll_back.frame_growth = Err("not awaited".into());
    let verdict = judge_feed(MEASURED, &evidence);
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    assert!(
        verdict.detail.contains("outcome=played_in_browser"),
        "{}",
        verdict.detail
    );
}

#[test]
fn an_abort_error_from_play_fails_every_back_row() {
    for (expected, mut evidence) in [(LIVE, live()), (RECREATED, recreated()), (MEASURED, live())] {
        back_mut(&mut evidence).play_rejections =
            vec!["12 play-rejected back reel 1: AbortError".into()];
        let detail = failure_of(expected, &evidence);
        assert!(detail.contains("AbortError from play()"), "{detail}");
    }
}

#[test]
fn another_rejection_of_play_is_logged_but_not_an_abort() {
    let mut evidence = recreated();
    back_mut(&mut evidence).play_rejections = vec!["play-rejected x: NotAllowedError".into()];
    assert_eq!(judge_feed(RECREATED, &evidence).outcome, Outcome::Pass);
}

#[test]
fn a_live_row_where_the_page_called_play_but_the_reel_never_started_playing_still_judges_the_window()
 {
    // Раздражитель — вызов play(): исход «ролик не заиграл у страницы» строку не делает «вхолостую».
    let mut evidence = live();
    back_mut(&mut evidence).page_saw_playing = false;
    assert_eq!(judge_feed(LIVE, &evidence).outcome, Outcome::Pass);
}
