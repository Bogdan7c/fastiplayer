//! Тесты судьи атак: чистые функции над уликами, без браузера.

use super::*;
use crate::scenarios::outcome::Outcome;
use crate::scenarios::page_catalog::PAGE_SCENARIOS;
use crate::site::{PageStatus, RunId};

fn notice(name: &str, cause: &str) -> EndNotice {
    EndNotice {
        notice: name.into(),
        cause: cause.into(),
        end: "none".into(),
        line: format!("Browser session end notice shown notice={name} cause={cause} end=none"),
    }
}

fn report_of(id: &str, metrics: &[(&str, f64)]) -> PageReport {
    PageReport {
        run: RunId::new("r-1").expect("run id"),
        scenario: id.into(),
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

fn healthy_follow_up_report() -> PageReport {
    report_of(
        FOLLOW_UP_PAGE,
        &[
            ("real_probe_available", 1.0),
            ("shadow_window_s", 12.0),
            ("real_frames_in_window", 8.0),
        ],
    )
}

/// Честный конец для страницы: обрыв сеанса или (для шквала) поглощение.
fn honest_end(page: &AttackPage) -> ObservedEnd {
    match page.end_policy {
        EndPolicy::SessionAbsorbsAttack => ObservedEnd::Absorbed { frames_gained: 120 },
        _ => ObservedEnd::Notice(notice(STREAM_ERROR_NOTICE, "page_stream_error")),
    }
}

/// Метрики здоровой атаки: долетела и (для шквала) поглощена в пределах.
fn healthy_attack_metrics(page: &AttackPage) -> Vec<(&'static str, f64)> {
    let mut metrics = vec![("attack_errors", 0.0)];
    match page.landing {
        Landing::NotMeasured => {}
        Landing::BytesCrossed => metrics.push(("giant_bytes_accepted", 146_800_640.0)),
        Landing::OperationsCrossed => metrics.push(("flood_accepted_operations", 87_986.0)),
        Landing::FloodAbsorbed(_) => metrics.extend([
            ("flood_messages_per_s", 113_000.0),
            ("flood_main_thread_worst_gap_ms", 4.0),
            ("flood_clock_advance_s", 4.0),
        ]),
    }
    metrics
}

fn healthy(page: &AttackPage) -> AttackEvidence {
    AttackEvidence {
        start: Ok(()),
        aftermath: AttackAftermath {
            end: Ok(honest_end(page)),
            window_closed: Ok(()),
            hook_acknowledged: Ok(()),
        },
        attack_report: Ok(report_of(page.id, &healthy_attack_metrics(page))),
        grants_before_follow_up: 1,
        follow_up: FollowUp {
            outcome: Ok(HandOffOutcome::Started),
            frames: Ok(FrameEvidence {
                presented_frames: 240,
                position_regressions: 0,
                summary_line: "summary".into(),
            }),
            page_report: Ok(healthy_follow_up_report()),
        },
        host: HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            session_grants: 2,
        },
        forbidden_player_lines: Vec::new(),
        page_log: String::new(),
    }
}

fn verdict_of(page: &AttackPage, change: impl FnOnce(&mut AttackEvidence)) -> Verdict {
    let mut evidence = healthy(page);
    change(&mut evidence);
    judge_attack(page, &evidence)
}

#[test]
fn an_honest_end_and_a_playing_next_session_pass_for_every_attack() {
    for page in ATTACK_PAGES {
        let verdict = judge_attack(page, &healthy(page));
        assert_eq!(
            verdict.outcome,
            Outcome::Pass,
            "{}: {}",
            page.id,
            verdict.detail
        );
    }
}

#[test]
fn every_broken_link_of_the_chain_fails() {
    let page = &ATTACK_PAGES[0];
    let cases: Vec<(&str, Verdict)> = vec![
        (
            "no honest start",
            verdict_of(page, |e| e.start = Err("no frames".into())),
        ),
        (
            "the attack never ended the session",
            verdict_of(page, |e| e.aftermath.end = Err("no end".into())),
        ),
        (
            "a different notice",
            verdict_of(page, |e| {
                e.aftermath.end = Ok(ObservedEnd::Notice(notice(
                    "page_took_video_back",
                    "page_took_video_back",
                )));
            }),
        ),
        (
            "the window stayed open",
            verdict_of(page, |e| {
                e.aftermath.window_closed = Err("still open".into())
            }),
        ),
        (
            "the attack page itself failed",
            verdict_of(page, |e| {
                e.attack_report = Ok(PageReport {
                    status: PageStatus::Fail,
                    ..report_of(page.id, &[])
                });
            }),
        ),
        (
            "the clean page was not handed off",
            verdict_of(page, |e| {
                e.follow_up.outcome = Ok(HandOffOutcome::PlayerUnavailable { reason: "x".into() })
            }),
        ),
        (
            "no new session after the attack",
            verdict_of(page, |e| e.host.session_grants = 1),
        ),
        (
            "no frames of the clean page",
            verdict_of(page, |e| e.follow_up.frames = Err("no frames".into())),
        ),
        (
            "the browser decodes the clean page",
            verdict_of(page, |e| {
                e.follow_up.page_report = Ok(report_of(
                    FOLLOW_UP_PAGE,
                    &[
                        ("real_probe_available", 1.0),
                        ("shadow_window_s", 12.0),
                        ("real_frames_in_window", 300.0),
                    ],
                ));
            }),
        ),
        (
            "a panic in the player log",
            verdict_of(page, |e| {
                e.forbidden_player_lines = vec!["thread panicked".into()]
            }),
        ),
    ];
    for (what, verdict) in cases {
        assert_eq!(verdict.outcome, Outcome::Fail, "{what} must fail");
    }
}

#[test]
fn a_rejected_generation_without_an_end_notice_fails_on_every_page() {
    // Отказ поколения без сообщения о конце оставил бы зрителя перед спиннером.
    for page in ATTACK_PAGES {
        let verdict = verdict_of(page, |e| {
            e.aftermath.end = Ok(ObservedEnd::GenerationRejected(
                "Stream generation rejected reason=UnparsableMedia".into(),
            ))
        });
        assert_eq!(verdict.outcome, Outcome::Fail, "{}", page.id);
    }
}

#[test]
fn the_player_must_name_garbage_as_unparsable_media() {
    assert!(
        check_rejection_reason(
            "INFO Stream generation rejected generation=1 reason=UnparsableMedia"
        )
        .is_ok()
    );
    assert!(
        check_rejection_reason(
            "INFO Stream generation rejected generation=1 reason=PlaybackFailed"
        )
        .is_err()
    );
}

#[test]
fn the_demux_error_is_expected_only_on_the_garbage_page() {
    let line = "ERROR PlayerEvent::FatalError kind=DemuxError Reading error packet: lane #0";
    let with_the_line = |e: &mut AttackEvidence| e.forbidden_player_lines = vec![line.into()];
    let garbage = verdict_of(page_with_id("attack-garbage"), with_the_line);
    assert_eq!(garbage.outcome, Outcome::Pass, "{}", garbage.detail);
    let clean_page = verdict_of(page_with_id("attack-event-flood"), with_the_line);
    assert_eq!(clean_page.outcome, Outcome::Fail);
    // Паника на странице мусора остаётся провалом: разрешена одна конкретная ошибка.
    let panic_on_garbage = verdict_of(page_with_id("attack-garbage"), |e| {
        e.forbidden_player_lines = vec!["thread panicked".into()]
    });
    assert_eq!(panic_on_garbage.outcome, Outcome::Fail);
}

#[test]
fn an_end_with_another_cause_is_not_the_attack_ending_the_session() {
    // Вкладка умерла или фон упал: сообщение то же, но сеанс закрыла не реакция на атаку.
    for page in ATTACK_PAGES
        .iter()
        .filter(|page| page.end_policy != EndPolicy::SessionAbsorbsAttack)
    {
        let verdict = verdict_of(page, |e| {
            e.aftermath.end = Ok(ObservedEnd::Notice(notice(
                STREAM_ERROR_NOTICE,
                "protocol_violation",
            )))
        });
        assert_eq!(verdict.outcome, Outcome::Fail, "{}", page.id);
    }
}

fn page_with_id(id: &str) -> &'static AttackPage {
    ATTACK_PAGES
        .iter()
        .find(|page| page.id == id)
        .unwrap_or_else(|| panic!("{id} is not an attack page"))
}

#[test]
fn a_giant_attack_that_never_landed_fails_even_with_an_honest_end() {
    let page = page_with_id("attack-giant-segments");
    let verdict = verdict_of(page, |e| {
        e.attack_report = Ok(report_of(
            page.id,
            &[("attack_errors", 0.0), ("giant_bytes_accepted", 0.0)],
        ));
    });
    assert_eq!(verdict.outcome, Outcome::Fail);
    assert!(
        verdict.detail.contains("never landed"),
        "{}",
        verdict.detail
    );
}

#[test]
fn an_absorbed_flood_passes_and_prints_its_cost() {
    let page = page_with_id("attack-worker-flood");
    let verdict = judge_attack(page, &healthy(page));
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    assert!(
        verdict.detail.contains("worst main-thread gap 4.0 ms"),
        "{}",
        verdict.detail
    );
}

#[test]
fn a_flood_that_ends_the_session_or_freezes_the_page_fails() {
    let page = page_with_id("attack-worker-flood");
    let ended = verdict_of(page, |e| {
        e.aftermath.end = Ok(ObservedEnd::Notice(notice(
            STREAM_ERROR_NOTICE,
            "page_stream_error",
        )));
    });
    assert_eq!(
        ended.outcome,
        Outcome::Fail,
        "an end of the session is not absorbing"
    );
    let no_playback = verdict_of(page, |e| {
        e.aftermath.end = Err("playback did not continue after the absorbed attack".into());
    });
    assert_eq!(no_playback.outcome, Outcome::Fail);
    let frozen = verdict_of(page, |e| {
        e.attack_report = Ok(report_of(
            page.id,
            &[
                ("flood_messages_per_s", 113_000.0),
                ("flood_main_thread_worst_gap_ms", 2_000.0),
                ("flood_clock_advance_s", 4.0),
            ],
        ));
    });
    assert_eq!(frozen.outcome, Outcome::Fail);
}

#[test]
fn absorbing_is_not_an_honest_end_for_pages_that_must_end_the_session() {
    let page = page_with_id("attack-event-flood");
    let verdict = verdict_of(page, |e| {
        e.aftermath.end = Ok(ObservedEnd::Absorbed { frames_gained: 100 });
    });
    assert_eq!(verdict.outcome, Outcome::Fail);
}

#[test]
fn end_lines_are_classified_by_their_marker() {
    let line = "INFO fastiplayer: Browser session end notice shown notice=stream_error cause=protocol_violation end=none";
    match classify_end_line(line) {
        Ok(ObservedEnd::Notice(found)) => {
            assert_eq!(
                (found.notice.as_str(), found.cause.as_str()),
                ("stream_error", "protocol_violation")
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        classify_end_line("WARN Stream generation rejected generation=2 reason=UnparsableMedia"),
        Ok(ObservedEnd::GenerationRejected(_))
    ));
    // Маркер конца без полей — сломанный формат журнала, а не «какой-то конец».
    assert!(classify_end_line("Browser session end notice shown").is_err());
}

#[test]
fn attack_pages_exist_in_the_catalog_and_declare_the_attack_hook() {
    let site = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("site");
    let shared = std::fs::read_to_string(site.join("lib/attack-page.js")).expect("shared body");
    assert!(
        shared.contains("'attack'"),
        "the shared body must declare the attack hook"
    );
    for page in ATTACK_PAGES {
        let entry = PAGE_SCENARIOS
            .iter()
            .find(|scenario| scenario.id == page.id)
            .unwrap_or_else(|| panic!("{} is not in the page catalog", page.id));
        let text = std::fs::read_to_string(site.join("pages").join(format!("{}.html", page.id)))
            .expect("attack page");
        assert!(
            text.contains("runAttackPage("),
            "{} must use the shared attack body",
            page.id
        );
        // Воркер в Firefox невозможен: страница честно `unsupported`, стенд это допускает.
        assert_eq!(
            entry.may_be_unsupported_in(BrowserKind::Firefox),
            page.id == "attack-worker-flood",
            "{}",
            page.id
        );
    }
}
