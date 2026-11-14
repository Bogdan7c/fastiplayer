//! Тесты маршрута `hls-live-sleep`: чистая модель «сна» сервера и настоящий сервер стенда.
//!
//! Настоящий сервер проверяется без ожидания 30 с: тест сам запускает часы прогона «в прошлом»
//! (часы лежат в общем состоянии сайта, тест — потомок модуля `site` и видит его поля).

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;

use super::super::hls_live::{SourcePlaylist, SourceSegment, WINDOW_SEGMENTS};
use super::super::{StandSite, StandSiteConfig};
use super::*;

/// Ролик в чистых тестах: столько же сегментов, сколько у настоящего `hls-ts-live` после удлинения.
const TOTAL_SEGMENTS: usize = 300;

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

fn state_at(elapsed: Duration) -> SleepingState {
    sleeping_state_at(
        elapsed,
        TOTAL_SEGMENTS,
        SOURCE_SEGMENT_PERIOD,
        SLEEP_SCHEDULE,
    )
}

/// Номера сегментов, которые показывает плейлист окна.
fn listed_segments(window: LiveWindow) -> BTreeSet<usize> {
    (window.first..window.published).collect()
}

#[test]
fn the_server_is_awake_before_the_sleep_asleep_inside_it_and_awake_again_after() {
    let schedule = SLEEP_SCHEDULE;
    let just_before = schedule.falls_asleep_after - Duration::from_millis(1);
    assert!(matches!(state_at(secs(0)), SleepingState::Awake(_)));
    assert!(matches!(state_at(just_before), SleepingState::Awake(_)));
    // Сон — полуоткрытый интервал: засыпание входит, пробуждение — уже наяву.
    assert!(matches!(
        state_at(schedule.falls_asleep_after),
        SleepingState::Asleep(_)
    ));
    assert!(matches!(
        state_at(schedule.wakes_at() - Duration::from_millis(1)),
        SleepingState::Asleep(_)
    ));
    assert!(matches!(
        state_at(schedule.wakes_at()),
        SleepingState::Awake(_)
    ));
}

#[test]
fn the_window_keeps_moving_while_the_server_sleeps() {
    // Во сне окно считается теми же часами, что наяву: сервер «спит» только для зрителя.
    let asleep_at = SLEEP_SCHEDULE.falls_asleep_after + secs(10);
    let window = state_at(asleep_at).window();
    assert_eq!(
        window,
        window_at(asleep_at, TOTAL_SEGMENTS, SOURCE_SEGMENT_PERIOD)
    );
    assert!(
        window.published
            > state_at(SLEEP_SCHEDULE.falls_asleep_after)
                .window()
                .published,
        "the window stood still during the sleep: {window:?}"
    );
}

#[test]
fn after_waking_the_playlist_shares_no_segment_with_any_playlist_seen_before_the_sleep() {
    // Все плейлисты, которые сервер успел показать до сна (с шагом 100 мс — чаще, чем любой плеер).
    let mut seen_before_sleep = BTreeSet::new();
    let mut elapsed = Duration::ZERO;
    while elapsed < SLEEP_SCHEDULE.falls_asleep_after {
        if let SleepingState::Awake(window) = state_at(elapsed) {
            seen_before_sleep.extend(listed_segments(window));
        }
        elapsed += Duration::from_millis(100);
    }
    let SleepingState::Awake(woken) = state_at(SLEEP_SCHEDULE.wakes_at()) else {
        panic!("the server must be awake at the wake moment");
    };
    let shared: Vec<usize> = listed_segments(woken)
        .intersection(&seen_before_sleep)
        .copied()
        .collect();
    assert!(
        shared.is_empty(),
        "the woken window {woken:?} still lists segments seen before the sleep: {shared:?}"
    );
    // Окно после пробуждения полное (сегментов в ролике хватает): эфиру есть куда идти дальше.
    assert_eq!(woken.published - woken.first, WINDOW_SEGMENTS);
    // Порог «новых» сегментов для судьи: ничего с таким номером и дальше до сна не показывали,
    // и всё окно после пробуждения лежит за ним.
    let threshold = first_segment_unseen_before_sleep(SLEEP_SCHEDULE, SOURCE_SEGMENT_PERIOD);
    assert!(seen_before_sleep.iter().all(|&index| index < threshold));
    assert!(woken.first >= threshold, "{woken:?} vs {threshold}");
    assert_eq!(
        threshold,
        window_at(
            SLEEP_SCHEDULE.falls_asleep_after,
            TOTAL_SEGMENTS,
            SOURCE_SEGMENT_PERIOD
        )
        .published
    );
}

#[test]
fn the_sleep_fits_into_the_players_network_wait_budget_and_outlasts_the_window() {
    // Бюджет ожидания сети плеера по умолчанию — 60 с (`network.reconnect_wait_ms`): сон короче, иначе
    // строка проверяла бы честную ошибку, а не ожидание.
    assert!(SLEEP_SCHEDULE.sleeps_for < secs(60));
    // За сон окно уезжает больше чем на свою длину: склейка неизбежна.
    let slid = SLEEP_SCHEDULE.sleeps_for.as_secs() / SOURCE_SEGMENT_PERIOD.as_secs();
    assert!(
        slid > WINDOW_SEGMENTS as u64,
        "the window slides only {slid} segments"
    );
}

#[test]
fn the_source_period_matches_the_segment_length_of_the_generated_clip() {
    // Судья считает номера сегментов по этой константе; ролик режет `media/generate.sh`.
    let script = include_str!("../../../media/generate.sh");
    let expected = format!("SEGMENT_SECONDS={}\n", SOURCE_SEGMENT_PERIOD.as_secs());
    assert!(
        script.contains(&expected),
        "generate.sh must cut {expected}"
    );
}

#[test]
fn a_source_playlist_with_another_period_still_drives_the_route_model() {
    // Модель берёт период из исходного плейлиста, а не из константы: у ролика с периодом 4 с окно
    // растёт вдвое медленнее (защита от молчаливой подмены периода в маршруте).
    let source = SourcePlaylist {
        target_duration_s: 4,
        segments: (0..50)
            .map(|index| SourceSegment {
                duration_s: 4.0,
                name: format!("seg-{index:05}.ts"),
            })
            .collect(),
    };
    let period = Duration::from_secs(u64::from(source.target_duration_s));
    let state = sleeping_state_at(secs(8), source.segments.len(), period, SLEEP_SCHEDULE);
    assert_eq!(state.window().published, INITIAL_WINDOW_SEGMENTS + 2);
}

/// Сырой GET; возвращает статус и тело.
fn get(origin: &str, path: &str) -> (u16, Vec<u8>) {
    let host_port = origin.strip_prefix("http://").expect("http origin");
    let mut stream = TcpStream::connect(host_port).expect("connect");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n"
    )
    .expect("send");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read");
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("head end");
    let head = String::from_utf8_lossy(&raw[..split]).to_lowercase();
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("status code");
    (status, raw[split + 4..].to_vec())
}

/// Сайт стенда с исходным роликом из `segment_count` сегментов (каждый — 16 байт со своим номером).
fn site_with_source(dir: &tempfile::TempDir, segment_count: usize) -> StandSite {
    let media_root = dir.path().join("media");
    let source_dir = media_root.join(SOURCE_DIRECTORY);
    std::fs::create_dir_all(&source_dir).expect("media dir");
    let mut original = String::from(
        "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n",
    );
    for index in 0..segment_count {
        original.push_str(&format!("#EXTINF:2.000000,\nseg-{index:05}.ts\n"));
        std::fs::write(
            source_dir.join(format!("seg-{index:05}.ts")),
            [index as u8; 16],
        )
        .expect("segment");
    }
    original.push_str("#EXT-X-ENDLIST\n");
    std::fs::write(source_dir.join(PLAYLIST_NAME), original).expect("playlist");
    std::fs::write(
        media_root.join("catalog.json"),
        r#"{"version":1,"assets":{"hls-ts-live":{"kind":"hls","url":"hls-ts-live/stream.m3u8","duration_s":120.0}}}"#,
    )
    .expect("catalog");
    let site_root = dir.path().join("site");
    std::fs::create_dir_all(&site_root).expect("site dir");
    let media = crate::media::MediaCatalog::load(&media_root).expect("catalog loads");
    StandSite::start(StandSiteConfig { site_root, media }).expect("site starts")
}

/// Запускает часы прогона `run` так, будто первый плейлист был запрошен `ago` назад.
fn start_clock_in_the_past(site: &StandSite, run: &str, ago: Duration) {
    let started = Instant::now()
        .checked_sub(ago)
        .expect("the monotonic clock is older than the test offset");
    site.shared
        .hls_live_sleep
        .start_or_elapsed(run, started)
        .expect("the clock starts");
}

fn playlist_path(run: &str) -> String {
    format!("{ROUTE_PREFIX}{run}/{PLAYLIST_NAME}")
}

#[test]
fn the_real_route_answers_503_on_the_playlist_while_asleep_but_keeps_serving_segments() {
    let dir = tempfile::tempdir().expect("temp dir");
    let site = site_with_source(&dir, 60);
    let origin = site.main_origin();
    // Середина сна: 10 с после засыпания.
    start_clock_in_the_past(
        &site,
        "asleep",
        SLEEP_SCHEDULE.falls_asleep_after + secs(10),
    );

    let (status, body) = get(&origin, &playlist_path("asleep"));
    assert_eq!(status, ASLEEP_STATUS, "{}", String::from_utf8_lossy(&body));
    // Опубликованный по часам сегмент отдаётся и во сне — как у CDN.
    let (status, body) = get(&origin, &format!("{ROUTE_PREFIX}asleep/seg-00020.ts"));
    assert_eq!((status, body), (200, vec![20; 16]));
    // Статус «сна» виден сценарию в журнале запросов.
    assert!(site.media_requests().iter().any(|request| {
        request.path == playlist_path("asleep") && request.status == ASLEEP_STATUS
    }));
}

#[test]
fn the_real_route_serves_a_window_without_old_segments_after_waking() {
    let dir = tempfile::tempdir().expect("temp dir");
    let site = site_with_source(&dir, 60);
    let origin = site.main_origin();
    start_clock_in_the_past(&site, "woken", SLEEP_SCHEDULE.wakes_at() + secs(1));

    let (status, body) = get(&origin, &playlist_path("woken"));
    assert_eq!(status, 200);
    let text = String::from_utf8(body).expect("utf8 playlist");
    let threshold = first_segment_unseen_before_sleep(SLEEP_SCHEDULE, SOURCE_SEGMENT_PERIOD);
    let listed: Vec<usize> = text
        .lines()
        .filter_map(|line| line.strip_prefix("seg-")?.strip_suffix(".ts")?.parse().ok())
        .collect();
    assert_eq!(listed.len(), WINDOW_SEGMENTS, "{text}");
    assert!(
        listed.iter().all(|&index| index >= threshold),
        "the woken playlist lists a segment seen before the sleep: {text}"
    );
    assert!(!text.contains("ENDLIST"), "the stream stays live: {text}");
}

#[test]
fn the_real_route_is_an_ordinary_live_stream_before_the_sleep_with_its_own_clock() {
    let dir = tempfile::tempdir().expect("temp dir");
    let site = site_with_source(&dir, 60);
    let origin = site.main_origin();
    // Сегмент до плейлиста: часов нет.
    assert_eq!(
        get(&origin, &format!("{ROUTE_PREFIX}fresh/seg-00000.ts")).0,
        404
    );
    let (status, body) = get(&origin, &playlist_path("fresh"));
    assert_eq!(status, 200);
    let text = String::from_utf8(body).expect("utf8 playlist");
    assert!(text.contains("#EXT-X-MEDIA-SEQUENCE:0\n"), "{text}");
    assert!(text.contains("seg-00003.ts"), "{text}");
    // Часы свои: плейлист `hls-live` того же прогона не запускает часы этого маршрута.
    assert_eq!(get(&origin, "/cdn/hls-live/other/stream.m3u8").0, 200);
    assert_eq!(
        get(&origin, &format!("{ROUTE_PREFIX}other/seg-00000.ts")).0,
        404
    );
}
