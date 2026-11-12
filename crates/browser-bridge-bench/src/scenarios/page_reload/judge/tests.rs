//! Тесты приговоров `page-reload`: настоящая замена проходит, каждая поломка ловится своим текстом.

use super::*;
use crate::scenarios::outcome::Outcome;

const FILM: ExpectedDocument = ExpectedDocument {
    view: "film",
    episode: "1",
};

fn healthy_host() -> HostLogEvidence {
    HostLogEvidence {
        caller_accepted: true,
        hello_accepted: true,
        session_granted: true,
        session_grants: 2,
    }
}

fn load(second_load: bool, view: &str, episode: &str, origin: &str) -> String {
    format!(
        "page-reload: load doc=d{second_load} second_load={second_load} view={view} ep={episode} origin={origin}"
    )
}

fn loads(lines: &[String]) -> Vec<DocumentLoad> {
    lines.iter().filter_map(|line| parse_load(line)).collect()
}

fn state(second_load: bool, ct: f64, real_frames: u64) -> DocumentState {
    DocumentState {
        document: format!("d{second_load}"),
        second_load,
        current_time_s: ct,
        real_frames,
        probe_available: true,
    }
}

/// Десять строк состояния второго документа: «успел пожить» и почти не декодировал.
fn shadowed_second_document() -> Vec<DocumentState> {
    (0..10_u32)
        .map(|second| state(true, f64::from(second), 4))
        .collect()
}

fn replacement_evidence() -> ReloadEvidence {
    ReloadEvidence {
        failed_steps: Vec::new(),
        journal: PlayerJournal {
            window_created: vec!["Browser window created".to_owned()],
            process_starts: vec![PLAYER_PROCESS_MARKER.to_owned()],
            session_opened: vec![
                "INFO Browser session opened intent=Window grant=Started".to_owned(),
                "INFO Browser session opened intent=Window grant=ReplacedPrevious".to_owned(),
            ],
            fullscreen_changes: vec![
                "Browser window fullscreen changed fullscreen=true".to_owned(),
            ],
        },
        loads: loads(&[
            load(false, "film", "1", "http://127.0.0.1:1"),
            load(true, "film", "1", "http://127.0.0.1:1"),
        ]),
        states: shadowed_second_document(),
        frame_growth: Some(Ok(60)),
        end: None,
        host: healthy_host(),
        forbidden_player_lines: Vec::new(),
        page_log: "page log".to_owned(),
    }
}

fn notice(cause: &str, notice: &str) -> EndNotice {
    EndNotice {
        notice: notice.to_owned(),
        cause: cause.to_owned(),
        end: "none".to_owned(),
        line: format!("Browser session end notice shown notice={notice} cause={cause} end=none"),
    }
}

fn end_evidence(first_signal_s: u64, other_site: bool) -> ReloadEvidence {
    let mut evidence = replacement_evidence();
    evidence.journal.session_opened.truncate(1);
    evidence.journal.session_opened[0] = "INFO Browser session opened grant=Started".to_owned();
    evidence.journal.fullscreen_changes.clear();
    evidence.frame_growth = None;
    evidence.states = if other_site {
        (0..10_u32)
            .map(|second| state(true, f64::from(second) + 0.5, u64::from(30 * (second + 1))))
            .collect()
    } else {
        Vec::new()
    };
    evidence.end = Some(EndObservation {
        first_signal: Ok(EndSignal {
            line: "The page link changed change=Left".to_owned(),
            after_navigation: Duration::from_secs(first_signal_s),
        }),
        notice: Ok(notice("tab_gone", "finished")),
        window_closed: Ok(()),
    });
    evidence
}

fn verdict_replacement(evidence: &ReloadEvidence) -> Verdict {
    judge(Expectation::Replacement, &[FILM], false, evidence)
}

#[test]
fn lines_of_the_page_are_parsed_and_foreign_lines_are_not() {
    let parsed = parse_load(&load(true, "frame", "2", "http://localhost:9"));
    assert_eq!(
        parsed,
        Some(DocumentLoad {
            document: "dtrue".to_owned(),
            second_load: true,
            view: "frame".to_owned(),
            episode: "2".to_owned(),
            origin: "http://localhost:9".to_owned(),
        })
    );
    let parsed = parse_state(
        "page-reload: state doc=k3j second_load=true ct=12.40 paused=false real_frames=7 probe=true",
    );
    assert_eq!(
        parsed,
        Some(DocumentState {
            document: "k3j".to_owned(),
            second_load: true,
            current_time_s: 12.4,
            real_frames: 7,
            probe_available: true,
        })
    );
    assert_eq!(
        parse_state("tab-return: state doc=a second_load=true ct=1 real_frames=1 probe=true"),
        None
    );
    assert_eq!(parse_load("page-reload: load doc=a"), None);
}

#[test]
fn a_real_replacement_passes() {
    let verdict = verdict_replacement(&replacement_evidence());
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
}

#[test]
fn a_replacement_that_never_happened_fails() {
    let mut evidence = replacement_evidence();
    evidence.journal.session_opened.truncate(1);
    let verdict = verdict_replacement(&evidence);
    assert_eq!(verdict.outcome, Outcome::Fail);
    assert!(
        verdict.detail.contains("ReplacedPrevious"),
        "{}",
        verdict.detail
    );
}

#[test]
fn a_second_window_or_process_fails() {
    let mut evidence = replacement_evidence();
    evidence
        .journal
        .window_created
        .push("Browser window created".to_owned());
    evidence
        .journal
        .process_starts
        .push(PLAYER_PROCESS_MARKER.to_owned());
    let detail = verdict_replacement(&evidence).detail;
    assert!(detail.contains("window was created 2"), "{detail}");
    assert!(detail.contains("process started 2"), "{detail}");
}

#[test]
fn fullscreen_must_be_entered_and_never_left() {
    let mut evidence = replacement_evidence();
    evidence.journal.fullscreen_changes.clear();
    assert!(
        verdict_replacement(&evidence)
            .detail
            .contains("never entered fullscreen")
    );
    evidence.journal.fullscreen_changes = vec![
        "x fullscreen=true".to_owned(),
        "x fullscreen=false".to_owned(),
    ];
    assert!(
        verdict_replacement(&evidence)
            .detail
            .contains("left fullscreen")
    );
}

#[test]
fn too_few_or_missing_frames_after_the_replacement_fail() {
    let mut evidence = replacement_evidence();
    evidence.frame_growth = Some(Ok(5));
    assert!(
        verdict_replacement(&evidence)
            .detail
            .contains("only 5 new frames")
    );
    evidence.frame_growth = Some(Err("timeout".to_owned()));
    assert!(
        verdict_replacement(&evidence)
            .detail
            .contains("no frames after the replacement")
    );
    evidence.frame_growth = None;
    assert_eq!(verdict_replacement(&evidence).outcome, Outcome::Fail);
}

#[test]
fn a_browser_that_decodes_the_second_document_itself_fails() {
    let mut evidence = replacement_evidence();
    evidence.states = (0..10_u32)
        .map(|second| state(true, f64::from(second), u64::from(30 * (second + 1))))
        .collect();
    assert!(
        verdict_replacement(&evidence)
            .detail
            .contains("plays it by itself")
    );
}

#[test]
fn a_second_document_that_lived_too_short_cannot_prove_anything() {
    let mut evidence = replacement_evidence();
    evidence.states.truncate(3);
    assert!(
        verdict_replacement(&evidence)
            .detail
            .contains("too short to judge")
    );
    evidence.states.clear();
    assert!(
        verdict_replacement(&evidence)
            .detail
            .contains("never reported its video state")
    );
}

#[test]
fn a_page_that_went_to_the_wrong_place_fails() {
    let mut evidence = replacement_evidence();
    evidence.loads = loads(&[
        load(false, "film", "1", "http://127.0.0.1:1"),
        load(true, "film", "1", "http://127.0.0.1:1"),
    ]);
    let wanted = ExpectedDocument {
        view: "film",
        episode: "2",
    };
    let verdict = judge(Expectation::Replacement, &[wanted], false, &evidence);
    assert!(
        verdict.detail.contains("second document(s)"),
        "{}",
        verdict.detail
    );
    // Тот же origin там, где сайт должен смениться: переход не туда.
    let verdict = judge(Expectation::Replacement, &[FILM], true, &evidence);
    assert!(
        verdict.detail.contains("expected it to change"),
        "{}",
        verdict.detail
    );
    // Второго документа не было вовсе.
    evidence.loads.truncate(1);
    assert_eq!(verdict_replacement(&evidence).outcome, Outcome::Fail);
}

#[test]
fn the_iframe_origin_is_not_the_tab_origin() {
    let mut evidence = replacement_evidence();
    evidence.loads = loads(&[
        load(false, "frame", "1", "http://127.0.0.1:1"),
        load(false, FRAME_INNER_VIEW, "1", "http://localhost:2"),
        load(true, "frame", "1", "http://127.0.0.1:1"),
        load(true, FRAME_INNER_VIEW, "1", "http://localhost:2"),
    ]);
    let expected = [
        ExpectedDocument {
            view: "frame",
            episode: "1",
        },
        ExpectedDocument {
            view: FRAME_INNER_VIEW,
            episode: "1",
        },
    ];
    let verdict = judge(Expectation::Replacement, &expected, false, &evidence);
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
}

#[test]
fn host_and_forbidden_player_lines_fail_the_row() {
    let mut evidence = replacement_evidence();
    evidence.host.session_granted = false;
    evidence.forbidden_player_lines = vec!["panic at x".to_owned()];
    let detail = verdict_replacement(&evidence).detail;
    assert!(detail.contains("did not grant the session"), "{detail}");
    assert!(
        detail.contains("forbidden player line: panic at x"),
        "{detail}"
    );
}

#[test]
fn a_failed_step_is_reported() {
    let mut evidence = replacement_evidence();
    evidence
        .failed_steps
        .push("replacement: timeout".to_owned());
    assert!(
        verdict_replacement(&evidence)
            .detail
            .contains("replacement: timeout")
    );
}

#[test]
fn ending_after_the_wait_passes_and_ending_early_fails() {
    let judge_end = |evidence: &ReloadEvidence| {
        judge(
            Expectation::EndsAfterWait,
            &[ExpectedDocument {
                view: "film",
                episode: "1",
            }],
            false,
            evidence,
        )
    };
    let verdict = judge_end(&end_evidence(10, false));
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    let early = judge_end(&end_evidence(2, false));
    assert_eq!(early.outcome, Outcome::Fail);
    assert!(early.detail.contains("too early"), "{}", early.detail);
}

#[test]
fn the_end_must_say_the_source_tab_closed_and_close_the_window() {
    let mut evidence = end_evidence(10, false);
    evidence.end.as_mut().unwrap().notice =
        Ok(notice("page_took_video_back", "page_took_video_back"));
    evidence.end.as_mut().unwrap().window_closed =
        Err("the player window did not close".to_owned());
    let detail = judge(Expectation::EndsAfterWait, &[FILM], false, &evidence).detail;
    assert!(
        detail.contains("notice=finished, cause=tab_gone"),
        "{detail}"
    );
    assert!(detail.contains("did not close"), "{detail}");
    evidence.end = None;
    assert!(
        judge(Expectation::EndsAfterWait, &[FILM], false, &evidence)
            .detail
            .contains("never waited")
    );
}

#[test]
fn an_end_without_replacement_must_not_open_a_second_session() {
    let mut evidence = end_evidence(10, false);
    evidence
        .journal
        .session_opened
        .push("Browser session opened grant=ReplacedPrevious".to_owned());
    let detail = judge(Expectation::EndsAfterWait, &[FILM], false, &evidence).detail;
    assert!(detail.contains("exactly one session"), "{detail}");
}

#[test]
fn another_site_ends_at_once_and_the_browser_plays_by_itself() {
    let judge_other =
        |evidence: &ReloadEvidence| judge(Expectation::EndsAtOnce, &[FILM], true, evidence);
    let mut evidence = end_evidence(2, true);
    evidence.loads = loads(&[
        load(false, "film", "1", "http://127.0.0.1:1"),
        load(true, "film", "1", "http://localhost:2"),
    ]);
    let verdict = judge_other(&evidence);
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    // Ждал, как при странице без видео: «сразу» не получилось.
    let mut late = evidence.loads.clone();
    let _ = late.pop();
    let mut waited = end_evidence(10, true);
    waited.loads = evidence.loads.clone();
    assert!(judge_other(&waited).detail.contains("too late"));
    // Видео другого сайта браузер не играл (его кто-то заглушил).
    let mut silent = end_evidence(2, true);
    silent.loads = evidence.loads.clone();
    silent.states = (0..10_u32)
        .map(|second| state(true, f64::from(second), 1))
        .collect();
    assert!(
        judge_other(&silent)
            .detail
            .contains("did not play the other-site video itself")
    );
}
