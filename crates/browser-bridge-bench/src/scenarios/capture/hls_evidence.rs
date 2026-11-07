//! Улики родного HLS VOD со стороны сервера стенда (сессия 10a2): кто и что качал из каталога
//! HLS-ролика до и после передачи. Чистые функции над журналом запросов сайта — проверяются
//! тестами без браузера.
//!
//! Что доказывается (для страницы, где `m3u8` лежит прямо в `<video src>`):
//! - ДО передачи корневой плейлист просил сам `<video>` страницы (`Sec-Fetch-Dest: video`) —
//!   линейка видит загрузку медиаэлемента, значит её отсутствие потом что-то значит;
//! - ПОСЛЕ передачи (с допуском на запрос «в полёте») элемент под тенью НИЧЕГО из каталога ролика
//!   не просит: ни плейлисты, ни сегменты, ни ключ;
//! - после передачи фон экстеншена (скриптовые запросы) качает плейлист(ы) и сегменты, а у
//!   зашифрованного ролика ещё и файл ключа; плейлисты и ключ сервер отдаёт целиком (`200`),
//!   сегменты — целиком (`200`) или частью (`206`, байтовый диапазон).
//!
//! Известный риск (проверяется живым прогоном): мы опираемся на `Sec-Fetch-Dest`, которым
//! Chromium помечает запросы своего HLS-движка. Если он пометит запросы сегментов иначе, чем
//! `video`, судья примет их за фон — сводка с заголовками первых запросов нужна для разбора.

use std::time::Instant;

use super::file_evidence::{IN_FLIGHT_GRACE, SEEK_FETCH_TOLERANCE_S};
use crate::site::{MediaRequest, MediaRequester};

/// Ответ «целиком».
const HTTP_OK: u16 = 200;
/// Ответ «часть» (байтовый диапазон).
const HTTP_PARTIAL_CONTENT: u16 = 206;
/// Имя файла ключа AES-128 рядом с плейлистом (`key.bin` в `media/generate.sh`).
const KEY_FILE_NAME: &str = "key.bin";
/// Длина сегмента роликов стенда, с (`SEGMENT_SECONDS` в `media/generate.sh`).
pub(in crate::scenarios) const SEGMENT_LENGTH_S: f64 = 2.0;
/// Сколько запросов каждого вида показываем в сводке: заголовков первого хватает для разбора.
const SUMMARY_SAMPLE: usize = 1;

/// HLS-страница стенда и что от её каталога ждём.
#[derive(Debug, Clone, Copy)]
pub(in crate::scenarios) struct HlsPage {
    /// Идентификатор страницы стенда.
    pub id: &'static str,
    /// Каталог ролика на сервере со слэшем на конце: по нему отбираются запросы.
    pub asset_dir: &'static str,
    /// Сегменты зашифрованы (AES-128): фон обязан скачать и ключ.
    pub encrypted: bool,
}

/// HLS VOD страницы стенда (родной HLS в `<video src>`).
pub(in crate::scenarios) const HLS_PAGES: &[HlsPage] = &[
    HlsPage {
        id: "m3u8-in-src",
        asset_dir: "/media/hls/",
        encrypted: false,
    },
    HlsPage {
        id: "hls-fmp4-abr",
        asset_dir: "/media/hls-fmp4-abr/",
        encrypted: false,
    },
    HlsPage {
        id: "hls-aes128",
        asset_dir: "/media/hls-ts-aes128/",
        encrypted: true,
    },
    // Длинный ролик для `player-commands` (в `capture` не входит).
    HlsPage {
        id: "hls-vod-long",
        asset_dir: "/media/hls-ts-long/",
        encrypted: false,
    },
];

/// HLS-страница, если это она.
pub(in crate::scenarios) fn hls_page(page_id: &str) -> Option<HlsPage> {
    HLS_PAGES.iter().copied().find(|page| page.id == page_id)
}

/// Вид ресурса HLS-ролика по имени файла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::scenarios) enum HlsResource {
    /// Главный или медиаплейлист (`.m3u8`).
    Playlist,
    /// Медиасегмент (`.ts`, `.m4s`).
    Segment,
    /// Init-сегмент fMP4 (`EXT-X-MAP`, `.mp4`).
    InitSegment,
    /// Файл ключа AES-128.
    Key,
    /// Что-то незнакомое в каталоге ролика.
    Other,
}

/// Вид ресурса по пути запроса; `None` — путь вне каталога ролика.
pub(in crate::scenarios) fn classify(page: HlsPage, path: &str) -> Option<HlsResource> {
    let file_name = path.strip_prefix(page.asset_dir)?;
    let extension = file_name.rsplit_once('.').map(|(_, extension)| extension);
    Some(if file_name == KEY_FILE_NAME {
        HlsResource::Key
    } else {
        match extension {
            Some("m3u8") => HlsResource::Playlist,
            Some("ts" | "m4s") => HlsResource::Segment,
            Some("mp4") => HlsResource::InitSegment,
            _ => HlsResource::Other,
        }
    })
}

/// Номер сегмента по имени (`seg-00007.ts`, `seg-1-00007.m4s` → 7): последняя группа цифр.
/// Сегмент номер N начинается в `N * SEGMENT_LENGTH_S` секунд ролика.
pub(in crate::scenarios) fn segment_index(path: &str) -> Option<u32> {
    let stem = path.rsplit('/').next()?.rsplit_once('.')?.0;
    stem.rsplit('-').next()?.parse().ok()
}

/// Запросы к каталогу ролика, разложенные по «кто» и «когда».
#[derive(Debug, Default)]
pub(in crate::scenarios) struct HlsRequests {
    pub element_before: Vec<MediaRequest>,
    pub element_under_shadow: Vec<MediaRequest>,
    pub background: Vec<MediaRequest>,
}

/// Разложить журнал сервера: `handed_off_at` — когда фон ответил `started`.
pub(in crate::scenarios) fn sort_requests(
    page: HlsPage,
    requests: &[MediaRequest],
    handed_off_at: Instant,
) -> HlsRequests {
    let mut sorted = HlsRequests::default();
    for request in requests
        .iter()
        .filter(|request| classify(page, &request.path).is_some())
    {
        let before = request.at < handed_off_at;
        match (request.requester(), before) {
            (MediaRequester::MediaElement, true) => sorted.element_before.push(request.clone()),
            (MediaRequester::MediaElement, false) => {
                if request.at >= handed_off_at + IN_FLIGHT_GRACE {
                    sorted.element_under_shadow.push(request.clone());
                }
            }
            (MediaRequester::Script, false) => sorted.background.push(request.clone()),
            // Скриптовый запрос до передачи — не наш случай (страница HLS сама не качает).
            (MediaRequester::Script, true) => {}
        }
    }
    sorted
}

/// Запросы фона заданного вида.
fn background_of(
    page: HlsPage,
    requests: &HlsRequests,
    wanted: HlsResource,
) -> impl Iterator<Item = &MediaRequest> {
    requests
        .background
        .iter()
        .filter(move |request| classify(page, &request.path) == Some(wanted))
}

/// Проверки каталога HLS под тенью; пусто — всё честно.
pub(in crate::scenarios) fn hls_failures(page: HlsPage, requests: &HlsRequests) -> Vec<String> {
    let mut failures = Vec::new();
    let element_asked_playlist = requests
        .element_before
        .iter()
        .any(|request| classify(page, &request.path) == Some(HlsResource::Playlist));
    if !element_asked_playlist {
        failures.push(format!(
            "the server never saw the page's own <video> load a playlist under {} (Sec-Fetch-Dest: video): the ruler cannot tell a stopped download",
            page.asset_dir
        ));
    }
    if let Some(request) = requests.element_under_shadow.first() {
        failures.push(format!(
            "the page's own <video> kept loading HLS resources under the shadow ({} request(s)), first: {}",
            requests.element_under_shadow.len(),
            request.headers_summary()
        ));
    }
    let expectations = [
        (HlsResource::Playlist, "playlist"),
        (HlsResource::Segment, "segment"),
    ];
    for (kind, label) in expectations {
        if background_of(page, requests, kind).next().is_none() {
            failures.push(format!(
                "the extension background never requested a {label} under {} after the hand-off",
                page.asset_dir
            ));
        }
    }
    if page.encrypted
        && background_of(page, requests, HlsResource::Key)
            .next()
            .is_none()
    {
        failures.push(format!(
            "the stream is AES-128 encrypted but the background never requested the key {}{KEY_FILE_NAME}",
            page.asset_dir
        ));
    }
    failures.extend(status_failures(page, requests));
    failures
}

/// Статусы ответов фону: плейлисты и ключ — `200`, сегменты — `200` или `206`.
fn status_failures(page: HlsPage, requests: &HlsRequests) -> Vec<String> {
    requests
        .background
        .iter()
        .filter_map(|request| {
            let resource = classify(page, &request.path)?;
            let allowed: &[u16] = match resource {
                HlsResource::Playlist | HlsResource::Key => &[HTTP_OK],
                HlsResource::Segment | HlsResource::InitSegment | HlsResource::Other => {
                    &[HTTP_OK, HTTP_PARTIAL_CONTENT]
                }
            };
            (!allowed.contains(&request.status)).then(|| {
                format!(
                    "the server answered a background {resource:?} request with {} (allowed {allowed:?}): {}",
                    request.status,
                    request.headers_summary()
                )
            })
        })
        .collect()
}

/// Строка для отчёта: сколько запросов чьих и заголовки первых запросов фона.
pub(in crate::scenarios) fn hls_summary(page: HlsPage, requests: &HlsRequests) -> String {
    let count = |kind| background_of(page, requests, kind).count();
    let first_of = |kind| {
        background_of(page, requests, kind)
            .take(SUMMARY_SAMPLE)
            .map(MediaRequest::headers_summary)
            .next()
            .unwrap_or_else(|| "-".to_owned())
    };
    format!(
        "hls: element requests before={} under shadow={}; background playlists={} segments={} inits={} keys={}; first background playlist request: {}; first background segment request: {}",
        requests.element_before.len(),
        requests.element_under_shadow.len(),
        count(HlsResource::Playlist),
        count(HlsResource::Segment),
        count(HlsResource::InitSegment),
        count(HlsResource::Key),
        first_of(HlsResource::Playlist),
        first_of(HlsResource::Segment),
    )
}

/// `player-commands` на HLS: тень держится через паузу и перемотки, а после закрытия окна плеера
/// (`returned_at`) настоящий элемент снова сам грузит свой плейлист — возврат честный.
/// Возвращает провалы и сводку.
pub(in crate::scenarios) fn commands_hls_evidence(
    page: HlsPage,
    requests: &[MediaRequest],
    handed_off_at: Instant,
    returned_at: Option<Instant>,
) -> (Vec<String>, String) {
    let mut sorted = sort_requests(page, requests, handed_off_at);
    // Запросы элемента после возврата — не «качал под тенью», а положенная перезагрузка источника.
    let reloaded = match returned_at {
        Some(returned) => {
            let (after, under): (Vec<_>, Vec<_>) = sorted
                .element_under_shadow
                .drain(..)
                .partition(|request| request.at >= returned);
            sorted.element_under_shadow = under;
            after
        }
        None => Vec::new(),
    };
    let mut failures = hls_failures(page, &sorted);
    if returned_at.is_some() && reloaded.is_empty() {
        failures.push(
            "after the player window closed the page's own <video> never loaded its playlist again: the source was not returned".to_owned(),
        );
    }
    let summary = format!(
        "{}; element requests after the return={}",
        hls_summary(page, &sorted),
        reloaded.len()
    );
    (failures, summary)
}

/// Отсортированные номера сегментов, которые запросил фон (скрипт) в моменты, подходящие под `in_window`.
fn background_segment_indexes(
    page: HlsPage,
    requests: &[MediaRequest],
    in_window: impl Fn(Instant) -> bool,
) -> Vec<u32> {
    let mut indexes: Vec<u32> = requests
        .iter()
        .filter(|request| {
            request.requester() == MediaRequester::Script
                && in_window(request.at)
                && classify(page, &request.path) == Some(HlsResource::Segment)
        })
        .filter_map(|request| segment_index(&request.path))
        .collect();
    indexes.sort_unstable();
    indexes.dedup();
    indexes
}

/// Номера сегментов, которые фон запросил в промежутке `[from, until)`.
pub(in crate::scenarios) fn fetched_segment_indexes(
    page: HlsPage,
    requests: &[MediaRequest],
    from: Instant,
    until: Instant,
) -> Vec<u32> {
    background_segment_indexes(page, requests, |at| at >= from && at < until)
}

/// Цель перемотки вперёд: начало самого дальнего сегмента, которого фон ещё не скачал, если
/// после него остаётся `room_after_s` секунд ролика (чтобы было что играть до сводки плеера).
/// `None` — скачано всё, что можно, и перематывать «за скачанное» некуда.
pub(in crate::scenarios) fn unfetched_seek_target(
    fetched: &[u32],
    duration_s: f64,
    room_after_s: f64,
) -> Option<f64> {
    let last_start_s = duration_s - room_after_s;
    if last_start_s < 0.0 {
        return None;
    }
    let last_index = (last_start_s / SEGMENT_LENGTH_S).floor() as u32;
    (0..=last_index)
        .rev()
        .find(|index| !fetched.contains(index))
        .map(|index| f64::from(index) * SEGMENT_LENGTH_S)
}

/// Перемотка дошла до сети: после неё фон запросил сегмент, которого не было раньше, и он лежит
/// у цели (±`SEEK_FETCH_TOLERANCE_S`). Возвращает улики числами.
pub(in crate::scenarios) fn seek_fetch_check(
    page: HlsPage,
    requests: &[MediaRequest],
    fetched_before: &[u32],
    seek_at: Instant,
    target_s: f64,
) -> Result<String, String> {
    let fetched_after = background_segment_indexes(page, requests, |at| at >= seek_at);
    let new_indexes: Vec<u32> = fetched_after
        .iter()
        .copied()
        .filter(|index| !fetched_before.contains(index))
        .collect();
    let near_target = new_indexes.iter().any(|index| {
        (f64::from(*index) * SEGMENT_LENGTH_S - target_s).abs() <= SEEK_FETCH_TOLERANCE_S
    });
    if near_target {
        Ok(format!(
            "seek to {target_s:.1} s: segments {new_indexes:?} were new (already fetched before: {} segment(s))",
            fetched_before.len()
        ))
    } else {
        Err(format!(
            "after the seek to {target_s:.1} s the background requested no segment that it had not requested before near the target (new segments: {new_indexes:?}, fetched before: {fetched_before:?})"
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const ABR: HlsPage = HlsPage {
        id: "hls-fmp4-abr",
        asset_dir: "/media/hls-fmp4-abr/",
        encrypted: false,
    };
    const AES: HlsPage = HlsPage {
        id: "hls-aes128",
        asset_dir: "/media/hls-ts-aes128/",
        encrypted: true,
    };

    /// Запрос с заданными «кем» (`Sec-Fetch-Dest`), временем и ответом.
    fn request(path: &str, dest: Option<&str>, at: Instant, status: u16) -> MediaRequest {
        MediaRequest {
            at,
            path: path.to_owned(),
            status,
            range: None,
            cookie: None,
            referer: None,
            origin: None,
            sec_fetch_site: None,
            sec_fetch_mode: None,
            sec_fetch_dest: dest.map(str::to_owned),
            page_fetch_marker: None,
            content_range: None,
        }
    }

    /// Честный прогон: элемент просит корневой плейлист до передачи, фон качает всё после.
    fn healthy_log(page: HlsPage, handed_off_at: Instant) -> Vec<MediaRequest> {
        let before = handed_off_at - Duration::from_millis(500);
        let after = handed_off_at + Duration::from_millis(300);
        let dir = page.asset_dir;
        let mut log = vec![
            request(&format!("{dir}master.m3u8"), Some("video"), before, 200),
            request(&format!("{dir}master.m3u8"), Some("empty"), after, 200),
            request(&format!("{dir}stream-0.m3u8"), Some("empty"), after, 200),
            request(&format!("{dir}seg-0-00000.m4s"), None, after, 206),
            request(&format!("{dir}seg-0-00001.m4s"), None, after, 200),
        ];
        if page.encrypted {
            log.push(request(&format!("{dir}key.bin"), None, after, 200));
        }
        log
    }

    fn failures_for(page: HlsPage, log: &[MediaRequest], handed_off_at: Instant) -> Vec<String> {
        hls_failures(page, &sort_requests(page, log, handed_off_at))
    }

    #[test]
    fn a_healthy_hand_off_has_no_failures() {
        let handed_off_at = Instant::now();
        assert!(failures_for(ABR, &healthy_log(ABR, handed_off_at), handed_off_at).is_empty());
        assert!(failures_for(AES, &healthy_log(AES, handed_off_at), handed_off_at).is_empty());
    }

    #[test]
    fn requests_outside_the_asset_directory_are_ignored() {
        let handed_off_at = Instant::now();
        let mut log = healthy_log(ABR, handed_off_at);
        // Элемент под тенью качает ЧУЖОЙ файл (например, ролик другой страницы): это не наше дело.
        log.push(request(
            "/media/progressive/h264-aac.mp4",
            Some("video"),
            handed_off_at + Duration::from_secs(5),
            206,
        ));
        assert!(failures_for(ABR, &log, handed_off_at).is_empty());
    }

    #[test]
    fn an_element_that_keeps_loading_under_the_shadow_fails() {
        let handed_off_at = Instant::now();
        let mut log = healthy_log(ABR, handed_off_at);
        log.push(request(
            "/media/hls-fmp4-abr/seg-0-00005.m4s",
            Some("video"),
            handed_off_at + IN_FLIGHT_GRACE + Duration::from_millis(1),
            200,
        ));
        let failures = failures_for(ABR, &log, handed_off_at);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("kept loading HLS resources under the shadow"));
    }

    #[test]
    fn an_element_request_in_flight_right_after_the_hand_off_is_tolerated() {
        let handed_off_at = Instant::now();
        let mut log = healthy_log(ABR, handed_off_at);
        log.push(request(
            "/media/hls-fmp4-abr/seg-0-00002.m4s",
            Some("video"),
            handed_off_at + Duration::from_millis(100),
            200,
        ));
        assert!(failures_for(ABR, &log, handed_off_at).is_empty());
    }

    #[test]
    fn without_the_elements_own_playlist_request_the_ruler_is_blind() {
        let handed_off_at = Instant::now();
        let mut log = healthy_log(ABR, handed_off_at);
        log.retain(|request| request.sec_fetch_dest.as_deref() != Some("video"));
        let failures = failures_for(ABR, &log, handed_off_at);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("never saw the page's own <video>"));
    }

    #[test]
    fn a_background_that_downloads_nothing_fails_for_playlists_and_segments() {
        let handed_off_at = Instant::now();
        let log = vec![request(
            "/media/hls-fmp4-abr/master.m3u8",
            Some("video"),
            handed_off_at - Duration::from_secs(1),
            200,
        )];
        let failures = failures_for(ABR, &log, handed_off_at);
        assert!(
            failures
                .iter()
                .any(|f| f.contains("never requested a playlist"))
        );
        assert!(
            failures
                .iter()
                .any(|f| f.contains("never requested a segment"))
        );
    }

    #[test]
    fn an_encrypted_stream_needs_the_key_downloaded_by_the_background() {
        let handed_off_at = Instant::now();
        let mut log = healthy_log(AES, handed_off_at);
        log.retain(|request| !request.path.ends_with("key.bin"));
        let failures = failures_for(AES, &log, handed_off_at);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("never requested the key"));
        // Тот же журнал у незашифрованного ролика ключа не требует.
        assert!(failures_for(ABR, &healthy_log(ABR, handed_off_at), handed_off_at).is_empty());
    }

    #[test]
    fn status_codes_are_judged_per_resource_kind() {
        let handed_off_at = Instant::now();
        let after = handed_off_at + Duration::from_millis(300);
        // Плейлист и ключ — только 200; сегмент — 200 или 206; 404 и 403 — провал.
        let mut log = healthy_log(AES, handed_off_at);
        log.push(request(
            "/media/hls-ts-aes128/stream.m3u8",
            None,
            after,
            206,
        ));
        log.push(request("/media/hls-ts-aes128/key.bin", None, after, 403));
        log.push(request(
            "/media/hls-ts-aes128/seg-00003.ts",
            None,
            after,
            404,
        ));
        let failures = failures_for(AES, &log, handed_off_at);
        assert_eq!(failures.len(), 3, "{failures:?}");
        assert!(
            failures
                .iter()
                .any(|f| f.contains("Playlist") && f.contains("206"))
        );
        assert!(
            failures
                .iter()
                .any(|f| f.contains("Key") && f.contains("403"))
        );
        assert!(
            failures
                .iter()
                .any(|f| f.contains("Segment") && f.contains("404"))
        );
    }

    #[test]
    fn resources_are_classified_by_name() {
        let kind = |path: &str| classify(ABR, path);
        assert_eq!(
            kind("/media/hls-fmp4-abr/master.m3u8"),
            Some(HlsResource::Playlist)
        );
        assert_eq!(
            kind("/media/hls-fmp4-abr/seg-1-00004.m4s"),
            Some(HlsResource::Segment)
        );
        assert_eq!(
            kind("/media/hls-fmp4-abr/init-0.mp4"),
            Some(HlsResource::InitSegment)
        );
        assert_eq!(kind("/media/hls-fmp4-abr/key.bin"), Some(HlsResource::Key));
        assert_eq!(kind("/media/hls-fmp4-abr/readme"), Some(HlsResource::Other));
        assert_eq!(kind("/media/hls/seg-00000.ts"), None);
    }

    #[test]
    fn segment_index_is_the_last_digit_group_of_the_name() {
        assert_eq!(segment_index("/media/hls-ts-long/seg-00007.ts"), Some(7));
        assert_eq!(
            segment_index("/media/hls-fmp4-abr/seg-1-00012.m4s"),
            Some(12)
        );
        assert_eq!(segment_index("/media/hls-ts-long/stream.m3u8"), None);
    }

    #[test]
    fn the_seek_target_is_the_farthest_unfetched_segment_with_room_left() {
        // 40 с, 12 с на доигрывание: последний подходящий сегмент — №14 (28 с).
        assert_eq!(unfetched_seek_target(&[0, 1, 2], 40.0, 12.0), Some(28.0));
        // Дальние уже скачаны — берём ближайший свободный ниже.
        assert_eq!(
            unfetched_seek_target(&[0, 1, 13, 14], 40.0, 12.0),
            Some(24.0)
        );
        // Скачано всё до самого конца допустимого — перематывать некуда.
        let everything: Vec<u32> = (0..=14).collect();
        assert_eq!(unfetched_seek_target(&everything, 40.0, 12.0), None);
        // Ролик короче запаса.
        assert_eq!(unfetched_seek_target(&[], 10.0, 12.0), None);
    }

    #[test]
    fn a_seek_is_proven_only_by_a_new_segment_near_the_target() {
        let handed_off_at = Instant::now();
        let seek_at = handed_off_at + Duration::from_secs(10);
        let after_seek = seek_at + Duration::from_millis(400);
        let dir = "/media/hls-ts-long/";
        let page = HlsPage {
            id: "hls-vod-long",
            asset_dir: dir,
            encrypted: false,
        };
        let early = request(&format!("{dir}seg-00001.ts"), None, handed_off_at, 200);
        let fetched_before =
            fetched_segment_indexes(page, std::slice::from_ref(&early), handed_off_at, seek_at);
        assert_eq!(fetched_before, vec![1]);

        // Новый сегмент у цели (28 с = №14) — доказательство.
        let near = request(&format!("{dir}seg-00014.ts"), None, after_seek, 200);
        let proof = seek_fetch_check(page, &[early.clone(), near], &fetched_before, seek_at, 28.0);
        assert!(proof.is_ok_and(|facts| facts.contains("[14]")));

        // Повтор уже скачанного №1 у цели не считается; новый, но далеко от цели — тоже.
        let again = request(&format!("{dir}seg-00001.ts"), None, after_seek, 200);
        let far = request(&format!("{dir}seg-00002.ts"), None, after_seek, 200);
        let refused = seek_fetch_check(
            page,
            &[early.clone(), again, far],
            &fetched_before,
            seek_at,
            28.0,
        );
        assert!(refused.is_err_and(|message| message.contains("no segment")));

        // Запрос ЭЛЕМЕНТА (не фона) у цели не доказывает работу плеера.
        let by_element = request(
            &format!("{dir}seg-00014.ts"),
            Some("video"),
            after_seek,
            200,
        );
        assert!(
            seek_fetch_check(page, &[early, by_element], &fetched_before, seek_at, 28.0).is_err()
        );
    }

    #[test]
    fn commands_evidence_demands_the_playlist_to_be_loaded_again_after_the_return() {
        let handed_off_at = Instant::now();
        let returned_at = handed_off_at + Duration::from_secs(20);
        let mut log = healthy_log(ABR, handed_off_at);
        let (failures, _) = commands_hls_evidence(ABR, &log, handed_off_at, Some(returned_at));
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("never loaded its playlist again"));

        // Элемент после возврата вновь грузит плейлист: это не «качал под тенью».
        log.push(request(
            "/media/hls-fmp4-abr/master.m3u8",
            Some("video"),
            returned_at + Duration::from_millis(200),
            200,
        ));
        let (failures, summary) =
            commands_hls_evidence(ABR, &log, handed_off_at, Some(returned_at));
        assert!(failures.is_empty(), "{failures:?}");
        assert!(summary.contains("after the return=1"));
        // Но за миг ДО возврата тот же запрос был бы нарушением тени.
        let mut early = healthy_log(ABR, handed_off_at);
        early.push(request(
            "/media/hls-fmp4-abr/master.m3u8",
            Some("video"),
            returned_at - Duration::from_secs(1),
            200,
        ));
        let (failures, _) = commands_hls_evidence(ABR, &early, handed_off_at, Some(returned_at));
        assert!(failures.iter().any(|f| f.contains("under the shadow")));
    }
}
