//! Улики `<video src>` со стороны сервера стенда (сессия 09b, решение 186): кто качал файл до и
//! после передачи. Чистые функции над журналом запросов сайта — проверяются тестами без браузера.
//!
//! Что доказывается:
//! - ДО передачи файл качал сам `<video>` страницы (`Sec-Fetch-Dest: video`) — линейка видит
//!   загрузку медиаэлемента, значит её отсутствие потом что-то значит;
//! - ПОСЛЕ передачи (с допуском на запрос «в полёте») медиаэлемент файл больше не просит —
//!   источник снят;
//! - после передачи файл качает фон экстеншена (скриптовый запрос с `Range`) — по `fetch_range`
//!   плеера; для страниц за входом — с кукой сайта и ответом сервера 206.
//!
//! Заголовки первого запроса фона печатаются в отчёт как есть: какие Cookie / Referer / Origin /
//! Sec-Fetch-Site шлёт фон каждого браузера — факт для владельца.

use std::time::{Duration, Instant};

use crate::site::{MediaRequest, MediaRequester};

/// Насколько начало нового запроса фона может разойтись с целью перемотки, с ролика. Байт
/// переводится во время линейно (ролик стенда — ровный битрейт), а плеер начинает чтение с
/// ключевого кадра до цели (GOP 2 с) и с перемежающегося звука: 4 с — с запасом на то и другое.
pub(in crate::scenarios) const SEEK_FETCH_TOLERANCE_S: f64 = 4.0;

/// Сколько после «started» ещё может прийти запрос медиаэлемента, отправленный до снятия
/// источника (запрос «в полёте»): локальный сервер отвечает за миллисекунды, 1,5 с — с запасом.
pub(in crate::scenarios) const IN_FLIGHT_GRACE: Duration = Duration::from_millis(1_500);

/// Ответ сервера «часть файла».
const HTTP_PARTIAL_CONTENT: u16 = 206;

/// Страница `<video src>` и что от её файла ждём.
#[derive(Debug, Clone, Copy)]
pub(in crate::scenarios) struct FilePage {
    /// Идентификатор страницы стенда.
    pub id: &'static str,
    /// Путь файла на сервере (по нему отбираются запросы).
    pub file_path: &'static str,
    /// Кука, которую обязан нести запрос фона (`None` — файл без входа).
    pub required_cookie: Option<&'static str>,
}

/// Страницы `<video src>` (файл — ассет `progressive-long-h264-aac`).
pub(in crate::scenarios) const FILE_PAGES: &[FilePage] = &[
    FilePage {
        id: "video-src",
        file_path: "/media/progressive/h264-aac-long.mp4",
        required_cookie: None,
    },
    FilePage {
        id: "video-src-cdn",
        file_path: "/cdn/session-media/progressive/h264-aac-long.mp4",
        required_cookie: Some("bench_cdn"),
    },
    // 09c, решение 202: путь `/cdn/media/` требует ещё и Referer своего сайта.
    FilePage {
        id: "video-src-referer",
        file_path: "/cdn/media/progressive/h264-aac-long.mp4",
        required_cookie: Some("bench_cdn"),
    },
];

/// Страница `<video src>`, если это она.
pub(in crate::scenarios) fn file_page(page_id: &str) -> Option<FilePage> {
    FILE_PAGES.iter().copied().find(|page| page.id == page_id)
}

/// Запросы к файлу страницы, разложенные по «кто» и «когда».
#[derive(Debug, Default)]
pub(in crate::scenarios) struct FileRequests {
    pub element_before: Vec<MediaRequest>,
    pub element_under_shadow: Vec<MediaRequest>,
    pub background: Vec<MediaRequest>,
    /// Собственные запросы страницы с меткой (`x-bench-page-fetch`) уже после передачи: проба
    /// того, что правило Referer фона не течёт на запросы вкладки (`referer_evidence`).
    pub page_fetches: Vec<MediaRequest>,
}

/// Разложить журнал сервера: `handed_off_at` — когда фон ответил `started`.
pub(in crate::scenarios) fn sort_requests(
    page: FilePage,
    requests: &[MediaRequest],
    handed_off_at: Instant,
) -> FileRequests {
    let mut sorted = FileRequests::default();
    for request in requests
        .iter()
        .filter(|request| request.path == page.file_path)
    {
        let before = request.at < handed_off_at;
        match (request.requester(), before) {
            (MediaRequester::MediaElement, true) => sorted.element_before.push(request.clone()),
            (MediaRequester::MediaElement, false) => {
                if request.at >= handed_off_at + IN_FLIGHT_GRACE {
                    sorted.element_under_shadow.push(request.clone());
                }
            }
            (MediaRequester::Script, false) if request.page_fetch_marker.is_some() => {
                sorted.page_fetches.push(request.clone());
            }
            (MediaRequester::Script, false) => sorted.background.push(request.clone()),
            // Скриптовый запрос до передачи — проверка самой страницы (`server-range-206`).
            (MediaRequester::Script, true) => {}
        }
    }
    sorted
}

/// Проверки файла под тенью; пусто — всё честно.
pub(in crate::scenarios) fn file_failures(page: FilePage, requests: &FileRequests) -> Vec<String> {
    let mut failures = Vec::new();
    if requests.element_before.is_empty() {
        failures.push(format!(
            "the server never saw the page's own <video> load {} (Sec-Fetch-Dest: video): the ruler cannot tell a stopped download",
            page.file_path
        ));
    }
    if let Some(request) = requests.element_under_shadow.first() {
        failures.push(format!(
            "the page's own <video> kept loading the file under the shadow ({} request(s)), first: {}",
            requests.element_under_shadow.len(),
            request.headers_summary()
        ));
    }
    match requests.background.first() {
        None => failures.push(format!(
            "the extension background never requested {} after the hand-off",
            page.file_path
        )),
        Some(first) => {
            if let Some(cookie) = page.required_cookie
                && !first.has_cookie(cookie)
            {
                failures.push(format!(
                    "the background request carries no site cookie '{cookie}': {}",
                    first.headers_summary()
                ));
            }
            if let Some(refused) = requests
                .background
                .iter()
                .find(|request| request.status != HTTP_PARTIAL_CONTENT)
            {
                failures.push(format!(
                    "the server refused a background range request: {}",
                    refused.headers_summary()
                ));
            }
        }
    }
    failures
}

/// Строка для отчёта: сколько запросов чьих и заголовки первого запроса фона.
pub(in crate::scenarios) fn file_summary(requests: &FileRequests) -> String {
    let first_background = requests
        .background
        .first()
        .map_or_else(|| "-".to_owned(), MediaRequest::headers_summary);
    format!(
        "file: element requests before={} under shadow={}; background requests={}; first background request: {first_background}",
        requests.element_before.len(),
        requests.element_under_shadow.len(),
        requests.background.len(),
    )
}

/// `player-commands` на файле: тень держится через паузу и перемотки, а после закрытия окна
/// плеера (`returned_at`) настоящий элемент снова сам грузит свой файл — возврат честный.
/// Возвращает провалы и сводку.
pub(in crate::scenarios) fn commands_file_evidence(
    page: FilePage,
    requests: &[MediaRequest],
    handed_off_at: Instant,
    returned_at: Option<Instant>,
) -> (Vec<String>, String) {
    let mut sorted = sort_requests(page, requests, handed_off_at);
    // Запросы элемента после возврата — не «качал под тенью», а положенная перезагрузка файла.
    let reloaded: Vec<MediaRequest> = match returned_at {
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
    let mut failures = file_failures(page, &sorted);
    if returned_at.is_some() && reloaded.is_empty() {
        failures.push(
            "after the player window closed the page's own <video> never loaded its file again: the source was not returned".to_owned(),
        );
    }
    let summary = format!(
        "{}; element requests after the return={}",
        file_summary(&sorted),
        reloaded.len()
    );
    (failures, summary)
}

/// Передний край того, что плеер сейчас держит: конец отрезка САМОГО ПОЗДНЕГО запроса фона в
/// `[from, until)` и полный размер файла. Не «наибольший конец за всё время»: окно предзагрузки
/// мало (1 МиБ), и после перемотки назад дальние байты уже выброшены — держится только окно
/// от курсора. На паузе окно заполнено и стоит, поэтому последний запрос и есть его край.
/// `None` — фон ещё ничего не получил.
pub(in crate::scenarios) fn fetch_front(
    page: FilePage,
    requests: &[MediaRequest],
    from: Instant,
    until: Instant,
) -> Option<(u64, u64)> {
    requests
        .iter()
        .filter(|request| {
            request.path == page.file_path
                && request.requester() == MediaRequester::Script
                && request.at >= from
                && request.at < until
        })
        .filter_map(|request| request.served_span().map(|span| (request.at, span)))
        .max_by_key(|(at, _)| *at)
        .map(|(_, span)| (span.end, span.total))
}

/// Начало запрошенного диапазона (`Range: bytes=a-…`).
fn requested_start(request: &MediaRequest) -> Option<u64> {
    request
        .range
        .as_deref()?
        .strip_prefix("bytes=")?
        .split('-')
        .next()?
        .parse()
        .ok()
}

/// Что известно о перемотке за скачанное, для проверки по журналу сервера.
#[derive(Debug, Clone, Copy)]
pub(in crate::scenarios) struct SeekBeyondFetched {
    /// Когда плееру велели перемотать.
    pub seek_at: Instant,
    /// Передний край окна предзагрузки до перемотки, байт (`fetch_front`).
    pub fetched_end: u64,
    /// Полный размер файла.
    pub total: u64,
    pub target_s: f64,
    pub duration_s: f64,
}

/// Перемотка плеера за скачанное дошла до сети: после неё фон запросил диапазон, который
/// начинается за уже полученным и у цели (±`SEEK_FETCH_TOLERANCE_S`). Возвращает улики числами.
pub(in crate::scenarios) fn seek_fetch_check(
    page: FilePage,
    requests: &[MediaRequest],
    seek: SeekBeyondFetched,
) -> Result<String, String> {
    let seconds_at = |byte: u64| byte as f64 / seek.total as f64 * seek.duration_s;
    let after: Vec<(u64, f64)> = requests
        .iter()
        .filter(|request| {
            request.path == page.file_path
                && request.requester() == MediaRequester::Script
                && request.at >= seek.seek_at
        })
        .filter_map(requested_start)
        .map(|start| (start, seconds_at(start)))
        .collect();
    let hit = after.iter().find(|(start, at_s)| {
        *start >= seek.fetched_end && (at_s - seek.target_s).abs() <= SEEK_FETCH_TOLERANCE_S
    });
    let starts: Vec<String> = after
        .iter()
        .take(6)
        .map(|(start, at_s)| format!("{start}(~{at_s:.1}s)"))
        .collect();
    let facts = format!(
        "window front before the seek: byte {} of {} (~{:.1} s); seek to {:.1} s; background range starts after the seek: [{}]",
        seek.fetched_end,
        seek.total,
        seconds_at(seek.fetched_end),
        seek.target_s,
        starts.join(", ")
    );
    match hit {
        Some((start, at_s)) => Ok(format!(
            "{facts}; target request at byte {start} (~{at_s:.1} s)"
        )),
        None => Err(format!(
            "no new background fetch_range near the seek target: {facts}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: FilePage = FilePage {
        id: "video-src-cdn",
        file_path: "/cdn/media/f.mp4",
        required_cookie: Some("bench_cdn"),
    };

    fn request(at: Instant, dest: &str, status: u16, cookie: Option<&str>) -> MediaRequest {
        MediaRequest {
            at,
            path: PAGE.file_path.to_owned(),
            status,
            range: Some("bytes=0-".to_owned()),
            cookie: cookie.map(str::to_owned),
            referer: None,
            origin: None,
            sec_fetch_site: None,
            sec_fetch_mode: None,
            sec_fetch_dest: Some(dest.to_owned()),
            page_fetch_marker: None,
            content_range: None,
        }
    }

    #[test]
    fn element_before_then_background_with_cookie_is_honest() {
        let start = Instant::now();
        let handed = start + Duration::from_secs(5);
        let log = [
            request(start, "video", 206, Some("bench_cdn=1")),
            request(handed + Duration::from_millis(200), "video", 206, None),
            request(
                handed + Duration::from_secs(1),
                "empty",
                206,
                Some("a=b; bench_cdn=1"),
            ),
        ];
        let sorted = sort_requests(PAGE, &log, handed);
        assert_eq!(sorted.element_before.len(), 1);
        assert!(
            sorted.element_under_shadow.is_empty(),
            "a request in flight right after the hand-off is tolerated"
        );
        assert_eq!(sorted.background.len(), 1);
        assert!(file_failures(PAGE, &sorted).is_empty());
    }

    #[test]
    fn element_still_loading_missing_cookie_and_refusal_are_failures() {
        let start = Instant::now();
        let handed = start + Duration::from_secs(5);
        let log = [
            request(start, "video", 206, Some("bench_cdn=1")),
            request(
                handed + Duration::from_secs(3),
                "video",
                206,
                Some("bench_cdn=1"),
            ),
            request(handed + Duration::from_secs(1), "empty", 403, None),
        ];
        let failures = file_failures(PAGE, &sort_requests(PAGE, &log, handed));
        assert_eq!(failures.len(), 3, "{failures:#?}");
        assert!(failures[0].contains("kept loading"));
        assert!(failures[1].contains("no site cookie"));
        assert!(failures[2].contains("refused"));
    }

    #[test]
    fn after_the_return_the_element_must_reload_and_that_is_not_a_shadow_leak() {
        let start = Instant::now();
        let handed = start + Duration::from_secs(5);
        let returned = handed + Duration::from_secs(30);
        let mut log = vec![
            request(start, "video", 206, Some("bench_cdn=1")),
            request(
                handed + Duration::from_secs(1),
                "empty",
                206,
                Some("bench_cdn=1"),
            ),
        ];
        let (failures, _) = commands_file_evidence(PAGE, &log, handed, Some(returned));
        assert_eq!(
            failures.len(),
            1,
            "no reload after the return: {failures:#?}"
        );
        log.push(request(
            returned + Duration::from_millis(300),
            "video",
            206,
            Some("bench_cdn=1"),
        ));
        let (failures, summary) = commands_file_evidence(PAGE, &log, handed, Some(returned));
        assert!(failures.is_empty(), "{failures:#?}");
        assert!(summary.contains("after the return=1"));
    }

    fn served(at: Instant, first: u64, last: u64, total: u64) -> MediaRequest {
        MediaRequest {
            range: Some(format!("bytes={first}-{}", last + 1_000_000)),
            content_range: Some(format!("bytes {first}-{last}/{total}")),
            ..request(at, "empty", 206, Some("bench_cdn=1"))
        }
    }

    #[test]
    fn a_seek_beyond_the_fetched_bytes_must_bring_a_fetch_at_the_target() {
        let start = Instant::now();
        let seek_at = start + Duration::from_secs(10);
        let total = 7_500_000;
        // Дальний отрезок раньше (до перемотки назад), окно сейчас — последний запрос.
        let mut log = vec![
            served(start, 0, 65_535, total),
            served(start + Duration::from_secs(1), 5_000_000, 6_000_000, total),
            served(start + Duration::from_secs(2), 65_536, 1_114_111, total),
        ];
        let (fetched_end, file_total) = fetch_front(PAGE, &log, start, seek_at).expect("front");
        assert_eq!(
            (fetched_end, file_total),
            (1_114_112, total),
            "the latest span, not the farthest"
        );
        let seek = SeekBeyondFetched {
            seek_at,
            fetched_end,
            total,
            target_s: 18.0,
            duration_s: 30.0,
        };
        assert!(
            seek_fetch_check(PAGE, &log, seek).is_err(),
            "no request after the seek"
        );
        // Запрос после перемотки, но продолжение старого места — не перемотка.
        log.push(served(
            seek_at + Duration::from_millis(50),
            1_114_112,
            2_000_000,
            total,
        ));
        assert!(seek_fetch_check(PAGE, &log, seek).is_err());
        // Запрос у цели (18 с из 30 ≈ байт 4,5 млн).
        log.push(served(
            seek_at + Duration::from_millis(80),
            4_400_000,
            5_000_000,
            total,
        ));
        let facts = seek_fetch_check(PAGE, &log, seek).expect("the target was fetched");
        assert!(facts.contains("target request at byte 4400000"), "{facts}");
    }

    #[test]
    fn no_element_before_and_no_background_are_failures() {
        let handed = Instant::now();
        let failures = file_failures(PAGE, &sort_requests(PAGE, &[], handed));
        assert_eq!(failures.len(), 2, "{failures:#?}");
    }
}
