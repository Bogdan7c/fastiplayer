//! Тесты приговора `series-memory-autoplay`: каждое правило ломается своей синтетикой.

use super::super::super::outcome::Outcome;
use super::super::storage::{BENCH_SERIES_KEY, choice_fingerprint, parse_memory};
use super::*;
use crate::observe::HostLogEvidence;
use crate::site::{PageReport, PageStatus, RunId};

const START: &str = "generation cause=start episode=1 dub=dub-a quality=video-360 from=0";
const PICK: &str = "generation cause=dub episode=1 dub=dub-b quality=video-360 from=0";
const AUTOPLAY: &str = "generation cause=autoplay episode=2 dub=dub-a quality=video-360 from=0";
const RESTORE: &str = "generation cause=dub episode=2 dub=dub-b quality=video-360 from=0";

fn record(generation_lines: &[&str]) -> SessionRecord {
    SessionRecord {
        generation_lines: generation_lines
            .iter()
            .map(|line| (*line).to_owned())
            .collect(),
        host: HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            session_grants: 1,
        },
        page_report: Ok(PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: "series-variants".into(),
            status: PageStatus::Pass,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: Default::default(),
        }),
        forbidden_player_lines: Vec::new(),
        page_log: String::new(),
        extension_notes: Vec::new(),
    }
}

fn fingerprint() -> String {
    choice_fingerprint("127.0.0.1", BENCH_SERIES_KEY)
}

fn remembered_dub_b() -> StoredMemory {
    parse_memory(&format!(
        r#"{{"version":1,"series":{{"{}":{{"audio":"Dub B","used_at":1}}}}}}"#,
        fingerprint()
    ))
    .expect("memory")
}

fn session(lines: &[&str]) -> AutoplaySession {
    AutoplaySession {
        record: record(lines),
        failed_steps: Vec::new(),
        memory_after_pick: None,
        windows_before_end: Some(WindowCount(1)),
        windows_at_end: Some(WindowCount(1)),
        dub_applied_after: None,
        episode_two_start: None,
    }
}

fn good() -> AutoplayEvidence {
    let mut remembered = session(&[START, PICK, AUTOPLAY, RESTORE]);
    remembered.memory_after_pick = Some(remembered_dub_b());
    remembered.windows_before_end = Some(WindowCount(2));
    remembered.windows_at_end = Some(WindowCount(2));
    remembered.dub_applied_after = Some(Duration::from_millis(1_200));
    remembered.episode_two_start = Some(EpisodeTwoStart {
        position_micros: 11_000_000,
        since_dub_switch: Duration::from_secs(10),
    });
    AutoplayEvidence {
        page: "series-variants",
        fingerprint: fingerprint(),
        control: session(&[START, AUTOPLAY]),
        remembered,
        quiet_window: Duration::from_secs(15),
    }
}

fn failure_text(evidence: &AutoplayEvidence) -> String {
    let verdict = judge_autoplay(evidence);
    assert_eq!(verdict.outcome, Outcome::Fail, "{}", verdict.detail);
    verdict.detail
}

#[test]
fn a_dub_restored_after_the_page_autoplay_with_a_quiet_control_passes() {
    let verdict = judge_autoplay(&good());
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    assert!(verdict.detail.contains("in 1.2 s"), "{}", verdict.detail);
}

#[test]
fn an_autoplay_that_kept_the_picked_dub_proves_nothing_and_fails() {
    // Страница сама осталась на Dub B: память тут ни при чём.
    let kept = "generation cause=autoplay episode=2 dub=dub-b quality=video-360 from=0";
    let mut evidence = good();
    evidence.remembered.record = record(&[START, PICK, kept]);
    let detail = failure_text(&evidence);
    assert!(
        detail.contains("expected the site default dub-a"),
        "{detail}"
    );
}

#[test]
fn a_dub_that_was_not_restored_on_episode_2_fails() {
    let mut evidence = good();
    evidence.remembered.record = record(&[START, PICK, AUTOPLAY]);
    evidence.remembered.dub_applied_after = None;
    let detail = failure_text(&evidence);
    assert!(
        detail.contains("expected exactly one switch of episode 2"),
        "{detail}"
    );
    assert!(detail.contains("never restored"), "{detail}");
}

#[test]
fn a_restore_before_the_autoplay_or_to_another_dub_fails() {
    let to_a = "generation cause=dub episode=2 dub=dub-a quality=video-360 from=0";
    for lines in [
        [START, PICK, RESTORE, AUTOPLAY],
        [START, PICK, AUTOPLAY, to_a],
    ] {
        let mut evidence = good();
        evidence.remembered.record = record(&lines);
        let detail = failure_text(&evidence);
        assert!(
            detail.contains("expected exactly one switch of episode 2"),
            "{detail}"
        );
    }
}

#[test]
fn a_control_that_switched_dub_without_memory_fails() {
    let mut evidence = good();
    evidence.control.record = record(&[START, AUTOPLAY, RESTORE]);
    let detail = failure_text(&evidence);
    assert!(
        detail.contains("control: the page switched dub"),
        "{detail}"
    );
}

#[test]
fn a_control_without_autoplay_fails() {
    let mut evidence = good();
    evidence.control.record = record(&[START]);
    let detail = failure_text(&evidence);
    assert!(
        detail.contains("control: the page autoplayed episode 2 0 times"),
        "{detail}"
    );
}

#[test]
fn a_restore_slower_than_the_control_window_makes_the_control_vacuous() {
    let mut evidence = good();
    evidence.remembered.dub_applied_after = Some(Duration::from_secs(16));
    let detail = failure_text(&evidence);
    assert!(
        detail.contains("later than the 15 s control window"),
        "{detail}"
    );
}

#[test]
fn episode_2_restarted_far_from_its_beginning_fails() {
    // Вернулись на секунду конца серии 1 (≈ 28 с) через 2 с после смены озвучки.
    let mut evidence = good();
    evidence.remembered.episode_two_start = Some(EpisodeTwoStart {
        position_micros: 28_000_000,
        since_dub_switch: Duration::from_secs(2),
    });
    let detail = failure_text(&evidence);
    assert!(
        detail.contains("did not start from the beginning"),
        "{detail}"
    );
    // Позиция ровно на границе запаса ещё честна.
    let mut evidence = good();
    evidence.remembered.episode_two_start = Some(EpisodeTwoStart {
        position_micros: 5_000_000,
        since_dub_switch: Duration::from_secs(2),
    });
    assert_eq!(judge_autoplay(&evidence).outcome, Outcome::Pass);
}

#[test]
fn a_new_player_window_during_the_autoplay_fails() {
    let mut evidence = good();
    evidence.remembered.windows_at_end = Some(WindowCount(3));
    let detail = failure_text(&evidence);
    assert!(
        detail.contains("remembered: the player created windows"),
        "{detail}"
    );
    let mut evidence = good();
    evidence.control.windows_before_end = None;
    let detail = failure_text(&evidence);
    assert!(
        detail.contains("control: the player windows were not counted"),
        "{detail}"
    );
}

#[test]
fn a_missing_memory_entry_after_the_pick_fails() {
    let mut evidence = good();
    evidence.remembered.memory_after_pick =
        Some(parse_memory(r#"{"version":1,"series":{}}"#).expect("memory"));
    let detail = failure_text(&evidence);
    assert!(
        detail.contains("no entry under the fingerprint"),
        "{detail}"
    );
}

#[test]
fn a_failed_step_is_reported_with_its_session_name() {
    let mut evidence = good();
    evidence
        .control
        .failed_steps
        .push("episode-1-ended-on-page: timeout".to_owned());
    let detail = failure_text(&evidence);
    assert!(
        detail.contains("control: episode-1-ended-on-page: timeout"),
        "{detail}"
    );
}
