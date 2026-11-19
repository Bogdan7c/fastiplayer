//! Тесты приговора краёв: здоровые улики дают «пройдено», а каждое нарушение — свой провал. Каждый тест
//! ломает ОДНО свидетельство здорового прогона и требует, чтобы приговор назвал именно его.

use super::super::judge::ScrollRecord;
use super::super::judge::{EndNoticePolicy, ExpectedWindow, FrameGrowthCheck, judge_feed};
use super::*;
use crate::browsers::HandOffOutcome;
use crate::observe::{FrameEvidence, HostLogEvidence};
use crate::scenarios::outcome::Outcome;
use crate::site::{PageReport, PageStatus, RunId};

const WIDTHS: &[u64] = &[640, 512, 384, 320, 448];
const BURST: ExpectedWindow = ExpectedWindow::EndsOnLastReel {
    reel_stream_widths: WIDTHS,
};
const HONEST_END: ExpectedWindow = ExpectedWindow::EndsHonestly {
    reel_stream_widths: &[640, 512],
    window: WindowAfterLeave::StandsPaused,
};
/// Уход Shorts (S4): прежний конец 306, окно доигрывает и закрывается.
const PLAYED_OUT_END: ExpectedWindow = ExpectedWindow::EndsHonestly {
    reel_stream_widths: &[640, 512],
    window: WindowAfterLeave::PlaysOut,
};
const WAITS: ExpectedWindow = ExpectedWindow::WaitsAtFeedEnd {
    reel_stream_widths: &[640, 512, 384],
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

/// Сообщение о конце: окно на паузе ждёт зрителя (решение 370) — так кончается здоровый уход из ленты.
fn end_notice(cause: &str) -> EndNotice {
    notice_line("waiting_for_viewer", cause, "none")
}

/// Сообщение о конце: окно доиграло скачанное (конец 306).
fn played_out_notice(cause: &str) -> EndNotice {
    notice_line("finished", cause, "drained")
}

fn notice_line(notice: &str, cause: &str, end: &str) -> EndNotice {
    EndNotice {
        notice: notice.into(),
        cause: cause.into(),
        end: end.into(),
        line: format!("Browser session end notice shown notice={notice} cause={cause} end={end}"),
    }
}

/// Общая здоровая основа: передача, один пропуск, отчёт страницы; края добавляют тесты ниже.
fn base() -> FeedEvidence {
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
        target_wait_lines: Vec::new(),
        generations: Vec::new(),
        shown_stream_widths: Vec::new(),
        end_notice_lines: Vec::new(),
        forbidden_player_lines: Vec::new(),
        page_report: Ok(report(&[])),
        page_log: "page log".into(),
    }
}

/// Здоровая быстрая серия: ролик 1, 2, затем итоговый 5 (промежуточные 3 и 4 окно пропустило).
fn burst() -> FeedEvidence {
    FeedEvidence {
        edge: Some(EdgeRecord {
            stimulus: Ok(()),
            window_reached_last_reel: Some(Ok(())),
            frame_growth: Some(Ok(growth(250))),
            generations_before_edge: None,
            end: None,
            window_still_present: None,
        }),
        generations: vec![1, 2, 3],
        shown_stream_widths: vec![640, 640, 512, 448, 448],
        page_report: Ok(report(&[("final_reel", 5.0), ("burst_intermediates", 2.0)])),
        ..base()
    }
}

/// Здоровый честный конец после спокойной прокрутки: уведомление «вкладка закрыта», окно закрылось.
fn honest_end() -> FeedEvidence {
    FeedEvidence {
        edge: Some(EdgeRecord {
            stimulus: Ok(()),
            window_reached_last_reel: None,
            frame_growth: None,
            generations_before_edge: Some(vec![1, 2]),
            end: Some(EndRecord {
                notice: Ok(end_notice("tab_gone")),
                window_after_end: Ok(()),
            }),
            window_still_present: None,
        }),
        generations: vec![1, 2],
        shown_stream_widths: vec![640, 640, 512],
        end_notice_lines: vec![end_notice("tab_gone").line],
        page_report: Ok(report(&[])),
        ..base()
    }
}

/// Здоровый конец ленты: окно ждёт, поколений три (ролики 1-3), сообщения о конце нет.
fn waiting() -> FeedEvidence {
    FeedEvidence {
        scrolls: vec![good_scroll(), good_scroll()],
        edge: Some(EdgeRecord {
            stimulus: Ok(()),
            window_reached_last_reel: None,
            frame_growth: None,
            generations_before_edge: Some(vec![1, 2, 3]),
            end: None,
            window_still_present: Some(Ok(())),
        }),
        generations: vec![1, 2, 3],
        shown_stream_widths: vec![640, 512, 384],
        page_report: Ok(report(&[])),
        ..base()
    }
}

fn failure_of(expected: ExpectedWindow, evidence: &FeedEvidence) -> String {
    let verdict = judge_feed(expected, evidence);
    assert_eq!(verdict.outcome, Outcome::Fail, "{}", verdict.detail);
    verdict.detail
}

fn edge_mut(evidence: &mut FeedEvidence) -> &mut EdgeRecord {
    evidence
        .edge
        .as_mut()
        .expect("the fixture has an edge record")
}

#[test]
fn a_burst_that_ends_on_the_last_reel_passes_and_reports_what_the_window_showed() {
    let verdict = judge_feed(BURST, &burst());
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    assert!(
        verdict.detail.contains("[640, 512, 448]"),
        "{}",
        verdict.detail
    );
}

#[test]
fn a_burst_where_the_window_never_reached_the_last_reel_fails() {
    let mut evidence = burst();
    edge_mut(&mut evidence).window_reached_last_reel = Some(Err("stuck on 384".into()));
    assert!(failure_of(BURST, &evidence).contains("never ended on the last reel"));
}

#[test]
fn a_window_that_ended_on_an_intermediate_reel_fails() {
    let mut evidence = burst();
    evidence.shown_stream_widths = vec![640, 512, 384];
    assert!(failure_of(BURST, &evidence).contains("ended on stream width"));
}

#[test]
fn a_window_that_went_back_to_an_earlier_reel_fails() {
    let mut evidence = burst();
    evidence.shown_stream_widths = vec![640, 512, 384, 512, 448];
    assert!(failure_of(BURST, &evidence).contains("went back or repeated"));
}

#[test]
fn a_window_that_showed_a_stream_outside_the_feed_fails() {
    let mut evidence = burst();
    evidence.shown_stream_widths = vec![640, 512, 999, 448];
    assert!(failure_of(BURST, &evidence).contains("no reel of the feed"));
}

#[test]
fn generations_that_repeat_or_go_back_in_a_burst_fail_but_numbering_gaps_do_not() {
    let mut evidence = burst();
    evidence.generations = vec![1, 3, 3];
    assert!(failure_of(BURST, &evidence).contains("strictly increasing"));
    evidence.generations = vec![1, 3, 2];
    assert!(failure_of(BURST, &evidence).contains("strictly increasing"));
    evidence.generations = vec![2, 3, 4];
    assert!(failure_of(BURST, &evidence).contains("strictly increasing"));
    // Страница с двумя MediaSource на шаг даёт мосту номера через один: это не нарушение.
    evidence.generations = vec![1, 3, 5, 7, 9];
    let verdict = judge_feed(BURST, &evidence);
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
}

#[test]
fn a_burst_with_too_few_generations_fails() {
    let mut evidence = burst();
    evidence.generations = vec![1, 2];
    assert!(failure_of(BURST, &evidence).contains("at least 3"));
}

#[test]
fn a_burst_whose_last_reel_does_not_play_fails() {
    let mut evidence = burst();
    edge_mut(&mut evidence).frame_growth = Some(Ok(growth(3)));
    assert!(failure_of(BURST, &evidence).contains("on the last reel"));
    edge_mut(&mut evidence).frame_growth = None;
    assert!(failure_of(BURST, &evidence).contains("no frame growth was measured"));
}

#[test]
fn a_burst_the_page_did_not_finish_on_the_last_reel_proves_nothing() {
    let mut evidence = burst();
    evidence.page_report = Ok(report(&[("final_reel", 4.0), ("burst_intermediates", 2.0)]));
    assert!(failure_of(BURST, &evidence).contains("proves nothing"));
}

#[test]
fn a_burst_without_intermediate_reels_proves_nothing() {
    let mut evidence = burst();
    evidence.page_report = Ok(report(&[("final_reel", 5.0), ("burst_intermediates", 1.0)]));
    assert!(failure_of(BURST, &evidence).contains("intermediate reels"));
}

#[test]
fn a_burst_whose_stimulus_never_happened_fails() {
    let mut evidence = burst();
    edge_mut(&mut evidence).stimulus = Err("no wheel".into());
    assert!(failure_of(BURST, &evidence).contains("stimulus did not happen"));
}

#[test]
fn a_session_end_notice_during_a_burst_fails() {
    let mut evidence = burst();
    evidence.end_notice_lines = vec![end_notice("page_took_video_back").line];
    assert!(failure_of(BURST, &evidence).contains("session end notice"));
}

#[test]
fn a_burst_row_without_edge_evidence_fails() {
    let mut evidence = burst();
    evidence.edge = None;
    assert!(failure_of(BURST, &evidence).contains("no edge evidence"));
}

#[test]
fn an_honest_end_with_the_source_tab_closed_notice_passes() {
    let verdict = judge_feed(HONEST_END, &honest_end());
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
}

#[test]
fn an_end_with_another_cause_is_not_an_honest_end() {
    let mut evidence = honest_end();
    edge_mut(&mut evidence).end = Some(EndRecord {
        notice: Ok(end_notice("page_took_video_back")),
        window_after_end: Ok(()),
    });
    assert!(failure_of(HONEST_END, &evidence).contains("source tab closed notice"));
}

#[test]
fn a_session_that_never_ended_is_not_an_honest_end() {
    let mut evidence = honest_end();
    edge_mut(&mut evidence).end = Some(EndRecord {
        notice: Err("timed out".into()),
        window_after_end: Ok(()),
    });
    assert!(failure_of(HONEST_END, &evidence).contains("did not end honestly"));
}

#[test]
fn a_window_that_played_out_after_leaving_the_feed_fails() {
    // Решение 370: доиграть скачанное без зрителя — провал, даже с «Source tab closed».
    let mut evidence = honest_end();
    evidence.end_notice_lines = vec![played_out_notice("tab_gone").line];
    edge_mut(&mut evidence).end = Some(EndRecord {
        notice: Ok(played_out_notice("tab_gone")),
        window_after_end: Ok(()),
    });
    assert!(failure_of(HONEST_END, &evidence).contains("notice=waiting_for_viewer"));
}

#[test]
fn a_window_that_closed_instead_of_standing_paused_fails() {
    let mut evidence = honest_end();
    edge_mut(&mut evidence).end = Some(EndRecord {
        notice: Ok(end_notice("tab_gone")),
        window_after_end: Err("the window should stand paused, but it is gone".into()),
    });
    assert!(failure_of(HONEST_END, &evidence).contains("should stand paused"));
}

#[test]
fn the_shorts_leave_still_demands_the_played_out_end() {
    let mut evidence = honest_end();
    evidence.end_notice_lines = vec![played_out_notice("tab_gone").line];
    edge_mut(&mut evidence).end = Some(EndRecord {
        notice: Ok(played_out_notice("tab_gone")),
        window_after_end: Ok(()),
    });
    let verdict = judge_feed(PLAYED_OUT_END, &evidence);
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    // Окно на паузе там, где строка ждёт доигрывания, — провал.
    assert!(failure_of(PLAYED_OUT_END, &honest_end()).contains("notice=finished"));
}

#[test]
fn a_row_that_never_observed_the_end_fails() {
    let mut evidence = honest_end();
    edge_mut(&mut evidence).end = None;
    assert!(failure_of(HONEST_END, &evidence).contains("did not observe how the session ended"));
}

#[test]
fn a_preview_adopted_into_the_window_fails_the_honest_end() {
    let mut evidence = honest_end();
    evidence.generations = vec![1, 2, 3];
    assert!(failure_of(HONEST_END, &evidence).contains("after the edge began"));
    let mut evidence = honest_end();
    evidence.shown_stream_widths = vec![640, 512, 320];
    assert!(failure_of(HONEST_END, &evidence).contains("expected exactly"));
}

#[test]
fn an_honest_end_whose_page_stimulus_did_not_happen_proves_nothing() {
    let mut evidence = honest_end();
    edge_mut(&mut evidence).stimulus = Err("the preview never played".into());
    assert!(failure_of(HONEST_END, &evidence).contains("stimulus did not happen"));
}

#[test]
fn an_honest_end_row_allows_the_end_notice_and_other_rows_do_not() {
    // Политика сообщения о конце — часть приговора: у честного конца оно нужно, у остальных запрещено.
    assert_ne!(EndNoticePolicy::Expected, EndNoticePolicy::Forbidden);
    let mut evidence = waiting();
    evidence.end_notice_lines = vec![end_notice("tab_gone").line];
    assert!(failure_of(WAITS, &evidence).contains("session end notice"));
}

#[test]
fn a_window_that_waits_at_the_feed_end_passes() {
    let verdict = judge_feed(WAITS, &waiting());
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
}

#[test]
fn a_window_gone_during_the_wait_at_the_feed_end_fails() {
    let mut evidence = waiting();
    edge_mut(&mut evidence).window_still_present = Some(Err("not controllable".into()));
    assert!(failure_of(WAITS, &evidence).contains("gone or unresponsive"));
    edge_mut(&mut evidence).window_still_present = None;
    assert!(failure_of(WAITS, &evidence).contains("never checked"));
}

#[test]
fn a_new_generation_during_the_wait_at_the_feed_end_fails() {
    let mut evidence = waiting();
    evidence.generations = vec![1, 2, 3, 4];
    assert!(failure_of(WAITS, &evidence).contains("after the edge began"));
    // Без снимка поколений «до края» приговор не может отличить новое от старого.
    let mut evidence = waiting();
    edge_mut(&mut evidence).generations_before_edge = None;
    assert!(failure_of(WAITS, &evidence).contains("did not record the generations"));
}

#[test]
fn a_calm_scroll_that_did_not_relocate_fails_every_edge_row() {
    for (expected, mut evidence) in [
        (BURST, burst()),
        (HONEST_END, honest_end()),
        (WAITS, waiting()),
    ] {
        evidence.scrolls[0].player_loaded_new_generation = Err("no".into());
        assert!(failure_of(expected, &evidence).contains("did not relocate"));
    }
}

#[test]
fn the_wrong_number_of_calm_scrolls_fails() {
    let mut evidence = burst();
    evidence.scrolls.push(good_scroll());
    assert!(failure_of(BURST, &evidence).contains("calm scroll"));
}

#[test]
fn a_scroll_whose_growth_is_not_required_may_have_no_growth_but_a_required_one_may_not() {
    // Строка «элемент убран сразу после переезда» не требует роста кадров на убранном ролике...
    let mut evidence = waiting();
    evidence.scrolls[0].frame_growth = Err("not measured".into());
    evidence.scrolls[0].frame_growth_check = FrameGrowthCheck::NotRequired;
    let verdict = judge_feed(WAITS, &evidence);
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    // ...а там, где рост обязателен, его отсутствие — провал.
    evidence.scrolls[0].frame_growth_check = FrameGrowthCheck::Required;
    assert!(failure_of(WAITS, &evidence).contains("player frames did not grow"));
}

#[test]
fn the_verdict_reports_how_fast_the_burst_was_scrolled() {
    let mut evidence = burst();
    evidence.page_report = Ok(report(&[
        ("final_reel", 5.0),
        ("burst_intermediates", 2.0),
        ("burst_gap_min_ms", 140.0),
        ("burst_gap_max_ms", 310.0),
    ]));
    let verdict = judge_feed(BURST, &evidence);
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    assert!(
        verdict.detail.contains("burst step gaps 140..310 ms"),
        "{}",
        verdict.detail
    );
}
