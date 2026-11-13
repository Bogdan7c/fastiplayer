//! Тесты маршрута `hls-live-ends`: чистая модель конца эфира и настоящий сервер стенда.

use std::io::{Read, Write};
use std::net::TcpStream;

use super::super::hls_live::{PLAYLIST_NAME, SOURCE_DIRECTORY, SourceSegment, WINDOW_SEGMENTS};
use super::super::{StandSite, StandSiteConfig};
use super::*;

const PERIOD: Duration = Duration::from_secs(2);
/// Порог конца в чистых тестах: небольшой, чтобы числа были наглядны.
const ENDS_AFTER: usize = 10;

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

fn source(segment_count: usize) -> SourcePlaylist {
    SourcePlaylist {
        target_duration_s: 2,
        segments: (0..segment_count)
            .map(|index| SourceSegment {
                duration_s: 2.0,
                name: format!("seg-{index:05}.ts"),
            })
            .collect(),
    }
}

/// Плейлист эфира через `elapsed` секунд (ролик из 60 сегментов, порог `ENDS_AFTER`).
fn playlist_at(elapsed_s: u64) -> String {
    let state = ending_window_at(secs(elapsed_s), 60, PERIOD, ENDS_AFTER);
    build_ending_playlist(&source(60), state)
}

fn media_sequence(playlist: &str) -> usize {
    playlist
        .lines()
        .find_map(|line| line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
        .expect("media sequence line")
        .parse()
        .expect("number")
}

#[test]
fn the_stream_is_on_air_without_endlist_until_the_threshold_segment_is_published() {
    // Начальное окно 4 сегмента, дальше по одному раз в 2 с: порог 10 достигается на 12-й секунде.
    for elapsed_s in [0, 2, 6, 11] {
        let state = ending_window_at(secs(elapsed_s), 60, PERIOD, ENDS_AFTER);
        assert!(!state.ended, "ended too early at {elapsed_s} s: {state:?}");
        assert!(
            state.window.published < ENDS_AFTER,
            "published {} at {elapsed_s} s",
            state.window.published
        );
        let playlist = playlist_at(elapsed_s);
        assert!(!playlist.contains("ENDLIST"), "{playlist}");
    }
}

#[test]
fn endlist_appears_exactly_at_the_threshold_and_only_once_at_the_end_of_the_playlist() {
    // Через 11 999 мс опубликовано 9 сегментов, через 12 000 мс - 10 (порог).
    let before = ending_window_at(Duration::from_millis(11_999), 60, PERIOD, ENDS_AFTER);
    let at_threshold = ending_window_at(secs(12), 60, PERIOD, ENDS_AFTER);
    assert!(!before.ended);
    assert!(at_threshold.ended);
    assert_eq!(at_threshold.window.published, ENDS_AFTER);

    let playlist = playlist_at(12);
    assert_eq!(playlist.matches("#EXT-X-ENDLIST").count(), 1, "{playlist}");
    assert!(
        playlist.ends_with("seg-00009.ts\n#EXT-X-ENDLIST\n"),
        "{playlist}"
    );
    // Живой плейлист не объявлял себя записью: ни PLAYLIST-TYPE, ни ENDLIST в начале.
    assert!(!playlist.contains("PLAYLIST-TYPE"), "{playlist}");
}

#[test]
fn the_window_is_frozen_after_the_end_and_publishes_no_new_segments() {
    let at_end = ending_window_at(secs(12), 60, PERIOD, ENDS_AFTER);
    for later_s in [13, 30, 500] {
        assert_eq!(
            ending_window_at(secs(later_s), 60, PERIOD, ENDS_AFTER),
            at_end,
            "the window moved at {later_s} s"
        );
    }
    let frozen = playlist_at(500);
    assert_eq!(frozen, playlist_at(12));
    // Окно - последние сегменты до порога, номер последовательности больше не растёт.
    assert_eq!(media_sequence(&frozen), ENDS_AFTER - WINDOW_SEGMENTS);
    assert_eq!(frozen.matches("#EXTINF:").count(), WINDOW_SEGMENTS);
    assert!(!frozen.contains("seg-00010.ts"), "{frozen}");
}

#[test]
fn a_clip_shorter_than_the_threshold_ends_together_with_the_clip() {
    // Ролик из 6 сегментов: эфир кончается, когда опубликованы все шесть, а не висит без ENDLIST.
    let state = ending_window_at(secs(100), 6, PERIOD, ENDS_AFTER);
    assert!(state.ended);
    assert_eq!(state.window.published, 6);
    let on_air = ending_window_at(secs(0), 6, PERIOD, ENDS_AFTER);
    assert!(!on_air.ended, "4 of 6 segments are published at the start");
    let text = build_ending_playlist(&source(6), state);
    assert!(text.ends_with("seg-00005.ts\n#EXT-X-ENDLIST\n"), "{text}");
}

#[test]
fn segment_access_follows_the_frozen_window_after_the_end() {
    let source = source(60);
    let ended = ending_window_at(secs(500), 60, PERIOD, ENDS_AFTER).window;
    // Все сегменты до порога отдаются (в том числе уехавшие из окна), дальше эфира нет.
    assert_eq!(
        segment_access(&source, ended, "seg-00000.ts"),
        SegmentAccess::Published
    );
    assert_eq!(
        segment_access(&source, ended, "seg-00009.ts"),
        SegmentAccess::Published
    );
    assert_eq!(
        segment_access(&source, ended, "seg-00010.ts"),
        SegmentAccess::NotYetPublished
    );
}

/// Сырой GET; возвращает статус, заголовки (в нижнем регистре) и тело.
fn get(origin: &str, path: &str) -> (u16, String, Vec<u8>) {
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
    (status, head, raw[split + 4..].to_vec())
}

/// Сайт стенда с исходным роликом из `segment_count` сегментов (каждый - 16 байт со своим номером).
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
        r#"{"version":1,"assets":{"hls-ts-live":{"kind":"hls","url":"hls-ts-live/stream.m3u8","duration_s":16.0}}}"#,
    )
    .expect("catalog");
    let site_root = dir.path().join("site");
    std::fs::create_dir_all(&site_root).expect("site dir");
    let media = crate::media::MediaCatalog::load(&media_root).expect("catalog loads");
    StandSite::start(StandSiteConfig { site_root, media }).expect("site starts")
}

#[test]
fn the_route_serves_an_on_air_playlist_for_a_long_clip_and_keeps_its_own_clock() {
    let dir = tempfile::tempdir().expect("temp dir");
    let site = site_with_source(&dir, 30);
    let origin = site.main_origin();
    let ends = |run: &str, name: &str| format!("{HLS_LIVE_ENDS_PREFIX}{run}/{name}");

    // Сегмент до плейлиста: часов нет.
    assert_eq!(get(&origin, &ends("run-1", "seg-00000.ts")).0, 404);

    let (status, head, body) = get(&origin, &ends("run-1", PLAYLIST_NAME));
    assert_eq!(status, 200);
    assert!(head.contains("cache-control: no-store"), "{head}");
    assert!(head.contains("access-control-allow-origin: *"), "{head}");
    assert!(head.contains("application/vnd.apple.mpegurl"), "{head}");
    let text = String::from_utf8(body).expect("utf8 playlist");
    assert!(!text.contains("ENDLIST"), "on air at the start: {text}");
    assert!(text.contains("seg-00003.ts"), "{text}");
    assert!(!text.contains("seg-00004.ts"), "{text}");

    // Опубликованный сегмент отдаётся с содержимым файла, будущий и чужой - 404.
    assert_eq!(
        get(&origin, &ends("run-1", "seg-00002.ts")).0,
        200,
        "published segment"
    );
    assert_eq!(get(&origin, &ends("run-1", "seg-00005.ts")).0, 404);
    assert_eq!(get(&origin, &ends("run-1", "seg-12345.ts")).0, 404);
    assert_eq!(get(&origin, &ends("run-1", "a/b.ts")).0, 404);

    // Часы маршрута свои: плейлист `hls-live` того же прогона не запускает часы `hls-live-ends`.
    let live = |run: &str, name: &str| format!("/cdn/hls-live/{run}/{name}");
    assert_eq!(get(&origin, &live("run-2", PLAYLIST_NAME)).0, 200);
    assert_eq!(get(&origin, &ends("run-2", "seg-00000.ts")).0, 404);
}

#[test]
fn the_route_ends_the_stream_at_once_when_the_clip_is_shorter_than_the_threshold() {
    let dir = tempfile::tempdir().expect("temp dir");
    // Четыре сегмента = начальное окно: ролик исчерпан в первый же момент.
    let site = site_with_source(&dir, 4);
    let origin = site.main_origin();
    let playlist_path = format!("{HLS_LIVE_ENDS_PREFIX}run-1/{PLAYLIST_NAME}");

    let (status, _, body) = get(&origin, &playlist_path);
    assert_eq!(status, 200);
    let text = String::from_utf8(body).expect("utf8 playlist");
    assert!(text.ends_with("seg-00003.ts\n#EXT-X-ENDLIST\n"), "{text}");
    // Все четыре сегмента отдаются с содержимым исходных файлов.
    for index in 0..4u8 {
        let (status, _, body) = get(
            &origin,
            &format!("{HLS_LIVE_ENDS_PREFIX}run-1/seg-{index:05}.ts"),
        );
        assert_eq!((status, body), (200, vec![index; 16]));
    }
    // Запросы видны сценарию в журнале.
    assert!(
        site.media_requests()
            .iter()
            .any(|request| request.path == playlist_path && request.status == 200)
    );
}

/// Тот же префикс, что экспортирует модуль (тесты ходят по нему на настоящий сервер).
const HLS_LIVE_ENDS_PREFIX: &str = ROUTE_PREFIX;
