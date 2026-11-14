//! Тесты строки `hls-live-sleep`: договор со страницей и сервером стенда, разбор журнала плеера и
//! приговор судьи на синтетических прогонах (здоровый прогон и каждая поломка по отдельности).

use std::time::{Duration, Instant};

use super::journal::{
    NETWORK_RECOVERED_MARKER, REJOIN_MARKER, journal_needles, read_sleep_journal,
};
use super::judge::*;
use crate::browsers::BrowserKind;
use crate::observe::HostLogEvidence;
use crate::scenarios::outcome::Outcome;
use crate::scenarios::page_catalog::PAGE_SCENARIOS;
use crate::site::{
    HLS_LIVE_ROUTE_PREFIX, HLS_LIVE_SLEEP_ROUTE_PREFIX, HLS_LIVE_SLEEP_SCHEDULE,
    HLS_LIVE_SLEEP_SEGMENT_PERIOD, MediaRequest, PageReport, PageStatus, RunId,
    first_segment_unseen_before_sleep,
};

// --- договор со страницей и сервером ---

/// Исходник страницы вместе с её телом (`site/pages/<id>.html` и `lib/<id>-page.js`).
fn page_source(page_id: &str) -> String {
    let site = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("site");
    let mut source = std::fs::read_to_string(site.join(format!("pages/{page_id}.html")))
        .unwrap_or_else(|error| panic!("page {page_id}: {error}"));
    let lib = format!("lib/{page_id}-page.js");
    source.push_str(
        &std::fs::read_to_string(site.join(&lib))
            .unwrap_or_else(|error| panic!("page lib {lib}: {error}")),
    );
    source
}

#[test]
fn the_page_plays_the_sleeping_stand_route_and_not_the_plain_live_one() {
    let source = page_source(HLS_LIVE_SLEEP_PAGE);
    assert!(source.contains(HLS_LIVE_SLEEP_ROUTE_PREFIX));
    assert!(!source.contains(HLS_LIVE_ROUTE_PREFIX));
}

#[test]
fn the_page_is_in_the_catalog_and_may_be_unsupported_only_in_firefox() {
    let scenario = PAGE_SCENARIOS
        .iter()
        .find(|scenario| scenario.id == HLS_LIVE_SLEEP_PAGE)
        .expect("the page is in the catalog");
    assert!(scenario.may_be_unsupported_in(BrowserKind::Firefox));
    assert!(!scenario.may_be_unsupported_in(BrowserKind::Chromium));
}

#[test]
fn the_robot_waits_for_the_rejoin_longer_than_the_server_sleeps() {
    // Строку склейки ждём от первых кадров: таймаут обязан пережить весь сон с запасом.
    let timeout = HLS_LIVE_SLEEP_SCHEDULE.wakes_at() + super::REJOIN_GRACE;
    assert!(timeout > HLS_LIVE_SLEEP_SCHEDULE.wakes_at() + Duration::from_secs(20));
    // Судья требует разброс 503 меньше, чем длится сон: иначе здоровый прогон не прошёл бы.
    assert!(MIN_OUTAGE_SPAN < HLS_LIVE_SLEEP_SCHEDULE.sleeps_for);
}

// --- журнал плеера ---

fn summary(frames: u64, regressions: u64) -> String {
    format!(
        "INFO Browser playback summary kind=periodic presented_frames={frames} taken_frames={frames} position_regressions={regressions} state=playing"
    )
}

fn rejoin_line() -> String {
    format!("INFO {REJOIN_MARKER}: resume_at=26s join=32s edge 38s -> 40s")
}

fn recovered_line() -> String {
    format!("INFO {NETWORK_RECOVERED_MARKER} 7 failed attempts")
}

fn healthy_journal() -> Vec<String> {
    vec![
        summary(240, 0),
        summary(480, 0),
        recovered_line(),
        rejoin_line(),
        summary(600, 0),
        summary(840, 0),
    ]
}

#[test]
fn the_journal_counts_frames_from_the_last_summary_before_the_rejoin() {
    let journal = read_sleep_journal(&healthy_journal());
    let rejoin = journal.rejoin.expect("rejoin seen");
    assert_eq!(rejoin.frames_before, 480);
    assert_eq!(rejoin.frames_after, Some(840));
    assert_eq!(rejoin.frames_gained(), 360);
    assert!(journal.network_recovered);
    assert_eq!(journal.max_position_regressions, 0);
    assert!(journal.rejections.is_empty());
}

#[test]
fn the_journal_without_summaries_after_the_rejoin_gains_nothing() {
    let journal = read_sleep_journal(&[summary(480, 0), rejoin_line()]);
    let rejoin = journal.rejoin.expect("rejoin seen");
    assert_eq!(rejoin.frames_after, None);
    assert_eq!(rejoin.frames_gained(), 0);
}

#[test]
fn the_needles_select_every_line_the_judge_reads() {
    let needles = journal_needles();
    for line in [
        summary(1, 0),
        rejoin_line(),
        recovered_line(),
        "WARN Stream generation rejected: x".to_owned(),
    ] {
        assert!(
            needles.iter().any(|needle| line.contains(needle)),
            "no needle selects {line}"
        );
    }
}

// --- судья ---

const RUN_PATH: &str = "/cdn/hls-live-sleep/r-1";

/// Запрос к маршруту прогона: `dest` — `Sec-Fetch-Dest` (`video` — элемент, `empty` — фон).
fn request(name: &str, dest: &str, status: u16, at: Instant) -> MediaRequest {
    MediaRequest {
        at,
        path: format!("{RUN_PATH}/{name}"),
        status,
        range: None,
        cookie: None,
        referer: None,
        origin: None,
        sec_fetch_site: None,
        sec_fetch_mode: None,
        sec_fetch_dest: Some(dest.to_owned()),
        page_fetch_marker: None,
        content_range: None,
    }
}

fn segment(index: usize) -> String {
    format!("seg-{index:05}.ts")
}

/// Синтетический прогон, повторяющий расписание маршрута: старт в `base`, сон 30–50 с.
struct Timeline {
    base: Instant,
}

impl Timeline {
    fn at(&self, seconds: f64) -> Instant {
        self.base + Duration::from_secs_f64(seconds)
    }
}

fn healthy() -> (Timeline, SleepEvidence) {
    let timeline = Timeline {
        base: Instant::now(),
    };
    let mut requests = vec![
        // Элемент страницы до передачи.
        request("stream.m3u8", "video", 200, timeline.at(0.0)),
        request(&segment(0), "video", 200, timeline.at(0.2)),
    ];
    // Фон наяву: плейлист каждые 2 с, сегменты вслед за эфиром (окно до сна — до сегмента 18).
    for second in (10..30).step_by(2) {
        let second_f = f64::from(second);
        requests.push(request("stream.m3u8", "empty", 200, timeline.at(second_f)));
        requests.push(request(
            &segment(3 + second as usize / 2),
            "empty",
            200,
            timeline.at(second_f + 0.1),
        ));
    }
    // Сон: повторы плеера с растущими паузами — 503.
    for second in [30.2, 30.7, 31.7, 33.7, 37.7, 45.7] {
        requests.push(request("stream.m3u8", "empty", 503, timeline.at(second)));
    }
    // Пробуждение: плейлист снова 200, сегменты нового окна (25..29).
    requests.push(request("stream.m3u8", "empty", 200, timeline.at(53.7)));
    for (offset, index) in [26, 27, 28].into_iter().enumerate() {
        requests.push(request(
            &segment(index),
            "empty",
            200,
            timeline.at(53.8 + offset as f64),
        ));
    }
    let evidence = SleepEvidence {
        steps: vec![StepEvidence {
            name: "session-granted",
            result: Ok(()),
        }],
        requests,
        handed_off_at: timeline.at(8.0),
        first_frames_at: Some(timeline.at(15.0)),
        journal: healthy_journal(),
        host: HostLogEvidence {
            caller_accepted: true,
            hello_accepted: true,
            session_granted: true,
            session_grants: 1,
        },
        page_report: Ok(PageReport {
            run: RunId::new("r-1").expect("run id"),
            scenario: HLS_LIVE_SLEEP_PAGE.into(),
            status: PageStatus::Pass,
            checks: Vec::new(),
            user_agent: String::new(),
            elapsed_ms: 0,
            metrics: Default::default(),
        }),
        forbidden_player_lines: Vec::new(),
        page_log: "page log".into(),
    };
    (timeline, evidence)
}

fn failed_with(evidence: &SleepEvidence, needle: &str) {
    let verdict = judge_hls_live_sleep(evidence);
    assert_eq!(verdict.outcome, Outcome::Fail, "{}", verdict.detail);
    assert!(
        verdict.detail.contains(needle),
        "expected '{needle}' in: {}",
        verdict.detail
    );
}

#[test]
fn a_stream_that_waits_out_the_sleep_and_rejoins_the_new_window_passes() {
    let (_, evidence) = healthy();
    let verdict = judge_hls_live_sleep(&evidence);
    assert_eq!(verdict.outcome, Outcome::Pass, "{}", verdict.detail);
    assert!(verdict.detail.contains("6 x 503"), "{}", verdict.detail);
    assert!(
        verdict.detail.contains("new-window segments after wake: 3"),
        "{}",
        verdict.detail
    );
    assert!(verdict.detail.contains("+360 frames"), "{}", verdict.detail);
}

#[test]
fn the_server_facts_split_the_background_requests_around_the_sleep() {
    let (timeline, evidence) = healthy();
    let facts = read_server_facts(&evidence.requests);
    let answers = facts.asleep_answers.expect("503 answers");
    assert_eq!(answers.count, 6);
    assert_eq!(answers.first_at, timeline.at(30.2));
    assert_eq!(answers.last_at, timeline.at(45.7));
    assert!(facts.playlist_ok_before_sleep);
    assert!(facts.playlist_ok_after_wake);
    assert_eq!(facts.new_window_segments_after_wake, 3);
}

#[test]
fn element_requests_and_other_routes_do_not_count_as_the_background() {
    let (timeline, mut evidence) = healthy();
    // 503 элементу страницы и 503 чужому маршруту не делают «фон видел сон».
    evidence.requests.retain(|request| request.status != 503);
    evidence
        .requests
        .push(request("stream.m3u8", "video", 503, timeline.at(31.0)));
    let mut foreign = request("stream.m3u8", "empty", 503, timeline.at(32.0));
    foreign.path = "/cdn/hls-live/r-1/stream.m3u8".to_owned();
    evidence.requests.push(foreign);
    assert_eq!(read_server_facts(&evidence.requests).asleep_answers, None);
    failed_with(&evidence, "the server sleep was not observed");
}

#[test]
fn a_hand_off_after_the_server_fell_asleep_makes_the_run_invalid() {
    let (timeline, mut evidence) = healthy();
    evidence.handed_off_at = timeline.at(31.0);
    failed_with(&evidence, "handed off only after the server fell asleep");
}

#[test]
fn first_frames_after_the_server_fell_asleep_make_the_run_invalid() {
    let (timeline, mut evidence) = healthy();
    evidence.first_frames_at = Some(timeline.at(40.0));
    failed_with(&evidence, "first frames only after the server fell asleep");
}

#[test]
fn no_background_playlist_before_the_sleep_makes_the_run_invalid() {
    let (timeline, mut evidence) = healthy();
    let first_503 = timeline.at(30.2);
    evidence.requests.retain(|request| {
        !(request.sec_fetch_dest.as_deref() == Some("empty")
            && request.path.ends_with("stream.m3u8")
            && request.at < first_503)
    });
    failed_with(&evidence, "got no playlist before the server fell asleep");
}

#[test]
fn a_player_that_gave_up_after_the_quick_transport_retries_fails() {
    let (timeline, mut evidence) = healthy();
    // Старое поведение: три быстрые попытки (~0,3 с) и всё — нет ни долгих повторов, ни пробуждения.
    evidence
        .requests
        .retain(|request| request.at < timeline.at(30.0));
    for second in [30.2, 30.3, 30.5] {
        evidence
            .requests
            .push(request("stream.m3u8", "empty", 503, timeline.at(second)));
    }
    evidence.journal = vec![summary(240, 0), summary(480, 0)];
    let verdict = judge_hls_live_sleep(&evidence);
    assert_eq!(verdict.outcome, Outcome::Fail, "{}", verdict.detail);
    for needle in [
        "the player did not wait for the network",
        "never got the playlist after the server woke up",
        "no 'HLS live network recovered' line",
        "no 'rejoined near the live edge' line",
        "fetched no segment of the new window",
    ] {
        assert!(
            verdict.detail.contains(needle),
            "{needle}: {}",
            verdict.detail
        );
    }
}

#[test]
fn segments_of_the_old_window_after_the_wake_are_not_the_new_window() {
    let (timeline, mut evidence) = healthy();
    let threshold =
        first_segment_unseen_before_sleep(HLS_LIVE_SLEEP_SCHEDULE, HLS_LIVE_SLEEP_SEGMENT_PERIOD);
    // После сна фон докачивает только старые сегменты (номера, бывшие в плейлистах до сна).
    evidence
        .requests
        .retain(|request| request.at <= timeline.at(53.7) || !request.path.contains("seg-"));
    evidence.requests.push(request(
        &segment(threshold - 1),
        "empty",
        200,
        timeline.at(54.0),
    ));
    failed_with(&evidence, "fetched no segment of the new window");
}

#[test]
fn a_missing_rejoin_line_fails_even_when_the_server_looks_healthy() {
    let (_, mut evidence) = healthy();
    evidence
        .journal
        .retain(|line| !line.contains(REJOIN_MARKER));
    failed_with(&evidence, "no 'rejoined near the live edge' line");
}

#[test]
fn too_few_frames_after_the_rejoin_fail() {
    let (_, mut evidence) = healthy();
    evidence.journal = vec![
        summary(480, 0),
        recovered_line(),
        rejoin_line(),
        summary(520, 0),
    ];
    failed_with(&evidence, "presented only 40 frame(s) after the rejoin");
}

#[test]
fn a_position_regression_fails_the_rejoin() {
    let (_, mut evidence) = healthy();
    evidence.journal.push(summary(900, 1));
    failed_with(&evidence, "the playback position went back 1 time(s)");
}

#[test]
fn a_rejected_generation_fails_the_row() {
    let (_, mut evidence) = healthy();
    evidence
        .journal
        .push("WARN Stream generation rejected: live refresh continuity failed".to_owned());
    failed_with(&evidence, "the player rejected the stream");
}

#[test]
fn a_failed_robot_step_fails_the_row() {
    let (_, mut evidence) = healthy();
    evidence.steps.push(StepEvidence {
        name: "player-plays-after-the-rejoin",
        result: Err("frames did not grow".to_owned()),
    });
    failed_with(&evidence, "step 'player-plays-after-the-rejoin'");
}

#[test]
fn a_failed_page_report_fails_the_row() {
    let (_, mut evidence) = healthy();
    if let Ok(report) = evidence.page_report.as_mut() {
        report.status = PageStatus::Fail;
    }
    let verdict = judge_hls_live_sleep(&evidence);
    assert_eq!(verdict.outcome, Outcome::Fail, "{}", verdict.detail);
}
