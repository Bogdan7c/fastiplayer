//! Маршрут `/cdn/hls-live/<прогон>/…` (сессия 10a3): «живой эфир» HLS из готового VOD-ролика.
//!
//! Зачем: тень экстеншена и плеер должны уметь HLS live (`duration = Infinity`, окно перемотки
//! едет вместе с эфиром), а настоящего кодировщика у стенда нет. Поэтому плейлист строится на
//! лету из сегментов ролика `hls-ts-live` (MPEG-TS, 60 сегментов по 2 с, 120 с) по часам стенда.
//!
//! Модель эфира (чистые функции [`window_at`], [`build_playlist`]):
//! - отсчёт идёт от ПЕРВОГО запроса плейлиста данного `<прогона>` (часы лежат в общем состоянии
//!   сайта, [`LiveClocks`]; каждый прогон страницы получает свой токен, поэтому начинается с нуля);
//! - сначала «опубликовано» [`INITIAL_WINDOW_SEGMENTS`] сегментов, потом по одному новому сегменту
//!   каждые `TARGETDURATION` секунд, как у настоящего кодировщика реального времени;
//! - плейлист показывает скользящее окно из последних [`WINDOW_SEGMENTS`] опубликованных сегментов;
//!   `EXT-X-MEDIA-SEQUENCE` равен номеру первого сегмента окна и растёт по часам;
//! - `EXT-X-ENDLIST` НЕ пишется никогда. Когда ролик кончился, окно замирает на последних
//!   сегментах (кодировщик «завис»). Решение осознанное: `ENDLIST` превратил бы эфир в VOD с
//!   конечным `duration`, и проверки «страница видит эфир» ломались бы в середине прогона; а
//!   ПЕТЛЯ с разрывом таймстампов (сегмент 0 после сегмента 19) запутала бы и браузер, и плеер.
//!   Ролик (120 с) заведомо длиннее прогона сценария (~1,5 мин), поэтому в прогоне эфир не замерзает.
//!
//! Имена сегментов — как в исходном ролике (`seg-NNNNN.ts`): номер в журнале сервера равен номеру
//! последовательности, и по растущим номерам видно, что фон идёт вслед за эфиром.
//!
//! Сегмент отдаётся, если он уже опубликован (его номер меньше числа опубликованных), даже если
//! уже выехал из окна плейлиста: настоящие CDN держат вышедшие сегменты ещё какое-то время. Ещё
//! не опубликованный сегмент, неизвестное имя и токен, у которого не было запроса плейлиста, —
//! 404. Заголовки как у `hls_local_segments`: `no-store`, CORS.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::http::{Body, Response};
use super::routes::RouteContext;
use super::static_files::serve_file;
use crate::media::safe_path::resolve_existing_file;

/// Префикс маршрута (под `/cdn/`, чтобы запросы попадали в журнал `request_log`).
pub const ROUTE_PREFIX: &str = "/cdn/hls-live/";
/// Каталог исходного VOD-ролика внутри каталога медиа.
pub(super) const SOURCE_DIRECTORY: &str = "hls-ts-live";
/// Имя плейлиста в маршруте и в каталоге исходного ролика.
pub const PLAYLIST_NAME: &str = "stream.m3u8";
/// Сколько сегментов опубликовано в момент первого запроса плейлиста (8 с эфира).
pub const INITIAL_WINDOW_SEGMENTS: usize = 4;
/// Сколько последних опубликованных сегментов показывает плейлист (скользящее окно).
pub const WINDOW_SEGMENTS: usize = 4;
/// Тип содержимого плейлиста.
pub(super) const PLAYLIST_CONTENT_TYPE: &str = "application/vnd.apple.mpegurl";
/// Сколько прогонов запоминают часы: стенд живёт один прогон команды, а запас защищает от утечки.
const MAX_REMEMBERED_RUNS: usize = 256;

/// Один сегмент исходного ролика.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SourceSegment {
    /// Длительность из `#EXTINF`, секунды.
    pub duration_s: f64,
    /// Имя файла сегмента рядом с исходным плейлистом (`seg-00005.ts`).
    pub name: String,
}

/// Исходный VOD-плейлист, разобранный в сегменты.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SourcePlaylist {
    /// Значение `#EXT-X-TARGETDURATION`, секунды (сегмент эфира публикуется раз в столько).
    pub target_duration_s: u32,
    pub segments: Vec<SourceSegment>,
}

/// Окно эфира в один момент: какие сегменты опубликованы и какие видит плейлист.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LiveWindow {
    /// Номер первого сегмента в плейлисте (он же `EXT-X-MEDIA-SEQUENCE`).
    pub first: usize,
    /// Сколько сегментов опубликовано всего (номера `0..published`); последний в окне — `published - 1`.
    pub published: usize,
}

/// Разбирает исходный плейлист; `None` — это не плейлист ролика (нет сегментов или нет длительности).
pub(super) fn parse_source_playlist(text: &str) -> Option<SourcePlaylist> {
    let mut target_duration_s: Option<u32> = None;
    let mut pending_duration: Option<f64> = None;
    let mut segments = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            target_duration_s = value.parse().ok();
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            let seconds = value.split(',').next().unwrap_or("");
            pending_duration = seconds.parse().ok();
        } else if !line.is_empty() && !line.starts_with('#') {
            // Адрес сегмента без предшествующего EXTINF — битый плейлист.
            segments.push(SourceSegment {
                duration_s: pending_duration.take()?,
                name: line.to_owned(),
            });
        }
    }
    let target_duration_s = target_duration_s.filter(|seconds| *seconds > 0)?;
    (!segments.is_empty()).then_some(SourcePlaylist {
        target_duration_s,
        segments,
    })
}

/// Окно эфира через `elapsed` после первого запроса плейлиста.
///
/// Опубликовано `INITIAL_WINDOW_SEGMENTS + elapsed / period` сегментов, но не больше, чем в ролике;
/// после конца ролика окно замирает на последних сегментах.
pub(super) fn window_at(
    elapsed: Duration,
    total_segments: usize,
    publish_period: Duration,
) -> LiveWindow {
    let period_ms = publish_period.as_millis().max(1);
    let grown = usize::try_from(elapsed.as_millis() / period_ms).unwrap_or(usize::MAX);
    let published = INITIAL_WINDOW_SEGMENTS
        .saturating_add(grown)
        .min(total_segments);
    LiveWindow {
        first: published.saturating_sub(WINDOW_SEGMENTS),
        published,
    }
}

/// Живой плейлист для окна: без `EXT-X-ENDLIST` и без `PLAYLIST-TYPE`, с растущим `MEDIA-SEQUENCE`.
pub(super) fn build_playlist(source: &SourcePlaylist, window: LiveWindow) -> String {
    let mut text = format!(
        "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{}\n#EXT-X-INDEPENDENT-SEGMENTS\n",
        source.target_duration_s, window.first
    );
    for segment in &source.segments[window.first..window.published] {
        text.push_str(&format!(
            "#EXTINF:{:.6},\n{}\n",
            segment.duration_s, segment.name
        ));
    }
    text
}

/// Решение по запросу сегмента: можно отдать или нельзя (и почему).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SegmentAccess {
    /// Сегмент уже опубликован.
    Published,
    /// Имя не из этого ролика.
    UnknownName,
    /// Сегмент ещё не вышел в эфир.
    NotYetPublished,
}

/// Можно ли отдать сегмент `name` при окне `window` (чистая функция).
pub(super) fn segment_access(
    source: &SourcePlaylist,
    window: LiveWindow,
    name: &str,
) -> SegmentAccess {
    match source
        .segments
        .iter()
        .position(|segment| segment.name == name)
    {
        None => SegmentAccess::UnknownName,
        Some(index) if index < window.published => SegmentAccess::Published,
        Some(_) => SegmentAccess::NotYetPublished,
    }
}

/// Часы эфиров: когда прогон впервые запросил плейлист. Общее состояние сайта.
#[derive(Default)]
pub(super) struct LiveClocks {
    started: Mutex<HashMap<String, Instant>>,
}

impl LiveClocks {
    /// Время эфира прогона `token` на момент `now`; первый вызов запускает часы (эфир с нуля).
    /// `None` — память часов переполнена, новый эфир не начинаем.
    pub(super) fn start_or_elapsed(&self, token: &str, now: Instant) -> Option<Duration> {
        let mut started = self
            .started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !started.contains_key(token) && started.len() >= MAX_REMEMBERED_RUNS {
            return None;
        }
        let began = *started.entry(token.to_owned()).or_insert(now);
        Some(now.saturating_duration_since(began))
    }

    /// Время эфира, если прогон уже запрашивал плейлист (сегменты без плейлиста часы не запускают).
    pub(super) fn elapsed_if_started(&self, token: &str, now: Instant) -> Option<Duration> {
        let started = self
            .started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        started
            .get(token)
            .map(|began| now.saturating_duration_since(*began))
    }
}

/// Токен прогона допустим в пути: непустой, буквы, цифры, `-`, `_`.
pub(super) fn is_valid_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Ответ на запрос `relative` (путь после [`ROUTE_PREFIX`]: `<прогон>/stream.m3u8` или `<прогон>/seg-NNNNN.ts`).
pub(super) fn serve(context: &RouteContext, relative: &str, range: Option<&str>) -> Response {
    let Some((token, name)) = relative.split_once('/') else {
        return Response::text(404, "not found");
    };
    if !is_valid_token(token) || name.contains('/') {
        return Response::text(404, "not found");
    }
    let media_root = &context.shared.media.root;
    let Some(source) = load_source(media_root) else {
        return Response::text(
            500,
            "the source playlist of the live stream is not readable",
        );
    };
    let publish_period = Duration::from_secs(u64::from(source.target_duration_s));
    let now = Instant::now();
    if name == PLAYLIST_NAME {
        let Some(elapsed) = context.shared.hls_live.start_or_elapsed(token, now) else {
            return Response::text(503, "too many live runs remembered");
        };
        let window = window_at(elapsed, source.segments.len(), publish_period);
        return live_headers(
            Response::new(200)
                .with_header("Content-Type", PLAYLIST_CONTENT_TYPE)
                .with_body(Body::Bytes(build_playlist(&source, window).into_bytes())),
        );
    }
    let Some(elapsed) = context.shared.hls_live.elapsed_if_started(token, now) else {
        return Response::text(404, "no playlist was requested for this run");
    };
    let window = window_at(elapsed, source.segments.len(), publish_period);
    match segment_access(&source, window, name) {
        SegmentAccess::UnknownName => Response::text(404, "unknown segment"),
        SegmentAccess::NotYetPublished => Response::text(404, "segment is not published yet"),
        SegmentAccess::Published => {
            let relative_file = format!("{SOURCE_DIRECTORY}/{name}");
            match resolve_existing_file(media_root, &relative_file) {
                Some(path) => live_headers(serve_file(&path, range)),
                None => Response::text(404, "not found"),
            }
        }
    }
}

/// Читает и разбирает исходный плейлист ролика (каждый запрос: файл маленький, кэш не нужен).
pub(super) fn load_source(media_root: &std::path::Path) -> Option<SourcePlaylist> {
    let path = resolve_existing_file(media_root, &format!("{SOURCE_DIRECTORY}/{PLAYLIST_NAME}"))?;
    parse_source_playlist(&std::fs::read_to_string(path).ok()?)
}

/// Заголовки эфира: ничего не кэшировать, CORS для страницы с другого origin.
pub(super) fn live_headers(response: Response) -> Response {
    response
        .with_header("Cache-Control", "no-store")
        .with_header("Access-Control-Allow-Origin", "*")
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use super::super::{StandSite, StandSiteConfig};
    use super::*;

    const PERIOD: Duration = Duration::from_secs(2);

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

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn the_window_starts_full_and_slides_by_one_segment_per_period() {
        assert_eq!(
            window_at(secs(0), 20, PERIOD),
            LiveWindow {
                first: 0,
                published: 4
            }
        );
        // Менее периода — то же окно.
        assert_eq!(
            window_at(Duration::from_millis(1999), 20, PERIOD).published,
            4
        );
        // Через период опубликован пятый сегмент, окно из четырёх последних уехало на один.
        assert_eq!(
            window_at(secs(2), 20, PERIOD),
            LiveWindow {
                first: 1,
                published: 5
            }
        );
        assert_eq!(
            window_at(secs(10), 20, PERIOD),
            LiveWindow {
                first: 5,
                published: 9
            }
        );
    }

    #[test]
    fn the_media_sequence_in_the_playlist_grows_with_the_clock() {
        let source = source(20);
        let sequence_at = |seconds| {
            let text = build_playlist(&source, window_at(secs(seconds), 20, PERIOD));
            let line = text
                .lines()
                .find_map(|line| line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
                .expect("media sequence line")
                .to_owned();
            line.parse::<usize>().expect("number")
        };
        assert_eq!(sequence_at(0), 0);
        assert_eq!(sequence_at(6), 3);
        assert_eq!(sequence_at(20), 10);
        // Первый сегмент в плейлисте — это и есть номер последовательности.
        let text = build_playlist(&source, window_at(secs(6), 20, PERIOD));
        assert!(text.contains("\nseg-00003.ts\n"), "{text}");
        assert!(!text.contains("seg-00002.ts"), "{text}");
        assert_eq!(text.matches("#EXTINF:").count(), WINDOW_SEGMENTS);
    }

    #[test]
    fn the_playlist_never_ends_and_holds_the_last_window_after_the_asset_is_over() {
        let source = source(20);
        // Ролик кончается на 32-й секунде эфира (4 + 16 сегментов); дальше окно замирает.
        let at_end = window_at(secs(32), 20, PERIOD);
        let long_after = window_at(secs(500), 20, PERIOD);
        assert_eq!(at_end, long_after);
        assert_eq!(
            long_after,
            LiveWindow {
                first: 16,
                published: 20
            }
        );
        let text = build_playlist(&source, long_after);
        assert!(!text.contains("#EXT-X-ENDLIST"), "{text}");
        assert!(!text.contains("PLAYLIST-TYPE"), "{text}");
        assert!(text.contains("#EXT-X-MEDIA-SEQUENCE:16\n"), "{text}");
        assert!(text.ends_with("seg-00019.ts\n"), "{text}");
    }

    #[test]
    fn a_short_asset_publishes_everything_at_once_without_underflow() {
        let source = source(2);
        let window = window_at(secs(0), 2, PERIOD);
        assert_eq!(
            window,
            LiveWindow {
                first: 0,
                published: 2
            }
        );
        assert_eq!(
            build_playlist(&source, window).matches("#EXTINF:").count(),
            2
        );
    }

    #[test]
    fn segment_access_separates_published_future_and_unknown_names() {
        let source = source(20);
        let window = window_at(secs(6), 20, PERIOD); // опубликовано 0..7, окно 3..7
        assert_eq!(
            segment_access(&source, window, "seg-00001.ts"),
            SegmentAccess::Published
        );
        assert_eq!(
            segment_access(&source, window, "seg-00006.ts"),
            SegmentAccess::Published
        );
        assert_eq!(
            segment_access(&source, window, "seg-00007.ts"),
            SegmentAccess::NotYetPublished
        );
        assert_eq!(
            segment_access(&source, window, "seg-99999.ts"),
            SegmentAccess::UnknownName
        );
        assert_eq!(
            segment_access(&source, window, "../stream.m3u8"),
            SegmentAccess::UnknownName
        );
    }

    #[test]
    fn the_source_playlist_is_parsed_and_a_broken_one_is_rejected() {
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:2.000000,\nseg-00000.ts\n#EXTINF:1.500000,\nseg-00001.ts\n#EXT-X-ENDLIST\n";
        let parsed = parse_source_playlist(text).expect("parses");
        assert_eq!(parsed.target_duration_s, 2);
        assert_eq!(parsed.segments.len(), 2);
        assert_eq!(parsed.segments[1].name, "seg-00001.ts");
        assert!((parsed.segments[1].duration_s - 1.5).abs() < 1e-9);
        assert_eq!(parse_source_playlist("#EXTM3U\n"), None);
        // Сегмент без EXTINF и плейлист без TARGETDURATION — не годятся.
        assert_eq!(
            parse_source_playlist("#EXTM3U\n#EXT-X-TARGETDURATION:2\nseg-00000.ts\n"),
            None
        );
        assert_eq!(
            parse_source_playlist("#EXTM3U\n#EXTINF:2.0,\nseg-00000.ts\n"),
            None
        );
    }

    #[test]
    fn clocks_start_on_the_first_playlist_per_run_and_segments_do_not_start_them() {
        let clocks = LiveClocks::default();
        let t0 = Instant::now();
        // Сегмент до плейлиста: часов нет.
        assert_eq!(clocks.elapsed_if_started("run-a", t0), None);
        assert_eq!(clocks.start_or_elapsed("run-a", t0), Some(Duration::ZERO));
        assert_eq!(
            clocks.start_or_elapsed("run-a", t0 + secs(7)),
            Some(secs(7))
        );
        // Другой прогон начинается с нуля независимо от первого.
        assert_eq!(
            clocks.start_or_elapsed("run-b", t0 + secs(7)),
            Some(Duration::ZERO)
        );
        assert_eq!(
            clocks.elapsed_if_started("run-a", t0 + secs(9)),
            Some(secs(9))
        );
    }

    #[test]
    fn clocks_refuse_new_runs_when_memory_is_full_but_keep_known_ones() {
        let clocks = LiveClocks::default();
        let t0 = Instant::now();
        for index in 0..MAX_REMEMBERED_RUNS {
            assert!(
                clocks
                    .start_or_elapsed(&format!("run-{index}"), t0)
                    .is_some()
            );
        }
        assert_eq!(clocks.start_or_elapsed("one-too-many", t0), None);
        assert_eq!(
            clocks.start_or_elapsed("run-0", t0 + secs(1)),
            Some(secs(1))
        );
    }

    #[test]
    fn tokens_allow_only_safe_characters() {
        assert!(is_valid_token("hls-live-1730_abc"));
        assert!(!is_valid_token(""));
        assert!(!is_valid_token(".."));
        assert!(!is_valid_token("a b"));
        assert!(!is_valid_token("a%2fb"));
    }

    /// Сырой GET; возвращает статус, заголовки (в нижнем регистре одной строкой) и тело.
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

    #[test]
    fn the_route_serves_a_sliding_playlist_published_segments_and_logs_the_numbers() {
        let dir = tempfile::tempdir().expect("temp dir");
        let media_root = dir.path().join("media");
        let source_dir = media_root.join(SOURCE_DIRECTORY);
        std::fs::create_dir_all(&source_dir).expect("media dir");
        let mut original = String::from(
            "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n",
        );
        for index in 0..8 {
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
        let site = StandSite::start(StandSiteConfig { site_root, media }).expect("site starts");
        let origin = site.main_origin();
        let playlist = |run: &str| format!("/cdn/hls-live/{run}/{PLAYLIST_NAME}");

        // Сегмент до плейлиста и неизвестный токен: 404, часы не запускаются.
        assert_eq!(get(&origin, "/cdn/hls-live/run-1/seg-00000.ts").0, 404);

        let (status, head, body) = get(&origin, &playlist("run-1"));
        assert_eq!(status, 200);
        assert!(head.contains("cache-control: no-store"), "{head}");
        assert!(head.contains("access-control-allow-origin: *"), "{head}");
        let text = String::from_utf8(body).expect("utf8 playlist");
        assert!(text.contains("#EXT-X-MEDIA-SEQUENCE:0\n"), "{text}");
        assert!(!text.contains("ENDLIST"), "{text}");
        assert!(text.contains("seg-00003.ts"), "{text}");
        assert!(!text.contains("seg-00004.ts"), "{text}");

        // Опубликованный сегмент отдаётся с содержимым исходного файла; будущий и чужой — 404.
        let (status, _, body) = get(&origin, "/cdn/hls-live/run-1/seg-00002.ts");
        assert_eq!((status, body), (200, vec![2u8; 16]));
        assert_eq!(get(&origin, "/cdn/hls-live/run-1/seg-00005.ts").0, 404);
        assert_eq!(get(&origin, "/cdn/hls-live/run-1/seg-12345.ts").0, 404);
        assert_eq!(get(&origin, "/cdn/hls-live/run-1/stream.m3u8x").0, 404);
        assert_eq!(get(&origin, "/cdn/hls-live/run-1").0, 404);
        assert_eq!(get(&origin, "/cdn/hls-live/run-1/a/b.ts").0, 404);
        // Другой токен не видит часов первого.
        assert_eq!(get(&origin, "/cdn/hls-live/run-2/seg-00000.ts").0, 404);

        // Запросы видны сценарию в журнале с номерами сегментов в пути.
        let logged = site.media_requests();
        assert!(
            logged
                .iter()
                .any(|request| request.path == "/cdn/hls-live/run-1/seg-00002.ts"
                    && request.status == 200)
        );
        assert!(
            logged
                .iter()
                .any(|request| request.path == playlist("run-1") && request.status == 200)
        );
    }
}
