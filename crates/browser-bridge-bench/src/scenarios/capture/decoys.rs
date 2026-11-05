//! Улики «чужое видео страницы не тронуто» (09c): баннер, превью по наведению и отдельный
//! рекламный ролик не должны уходить в плеер. Сервер стенда видит это честнее страницы: файл
//! приманки качал только медиаэлемент страницы (`Sec-Fetch-Dest: video`), а скриптовых запросов
//! фона экстеншена после передачи к нему не было.
//!
//! Чистые функции над журналом запросов — проверяются без браузера.

use std::time::Instant;

use crate::site::{MediaRequest, MediaRequester};

/// Путь файла рекламного ролика `ad-preroll` на сервере стенда.
pub(in crate::scenarios) const AD_FILE_PATH: &str = "/media/progressive/ad.mp4";

/// Страница, рядом с главным видео которой играет приманка-файл.
#[derive(Debug, Clone, Copy)]
pub(in crate::scenarios) struct DecoyPage {
    /// Идентификатор страницы стенда.
    pub id: &'static str,
    /// Путь файла приманки (баннер и превью играют один и тот же файл).
    pub file_path: &'static str,
}

/// Страницы с приманкой рядом с главным видео.
pub(in crate::scenarios) const DECOY_PAGES: &[DecoyPage] = &[DecoyPage {
    id: "banner-with-main",
    file_path: "/media/progressive/h264-aac.mp4",
}];

/// Страница с приманкой, если это она.
pub(in crate::scenarios) fn decoy_page(page_id: &str) -> Option<DecoyPage> {
    DECOY_PAGES.iter().copied().find(|page| page.id == page_id)
}

/// Фон экстеншена файл `file_path` НЕ качал после `handed_off_at`, а медиаэлемент страницы его
/// качал до этого (иначе линейка слепа: отсутствие запросов фона ничего бы не доказывало).
/// Возвращает провалы и сводку.
pub(in crate::scenarios) fn decoy_not_fetched_by_background(
    file_path: &'static str,
    requests: &[MediaRequest],
    handed_off_at: Instant,
) -> (Vec<String>, String) {
    let sorted = sort_decoy_requests(file_path, requests, handed_off_at);
    let mut failures = Vec::new();
    if sorted.element_before.is_empty() {
        failures.push(format!(
            "the page's own <video> never loaded {file_path}: the ruler cannot tell a file the background left alone"
        ));
    }
    if let Some(request) = sorted.background.first() {
        failures.push(format!(
            "the extension background downloaded {file_path} ({} request(s)), the decoy went to the player: {}",
            sorted.background.len(),
            request.headers_summary()
        ));
    }
    let summary = format!(
        "decoy {file_path}: element requests before={} background requests after the hand-off={}",
        sorted.element_before.len(),
        sorted.background.len()
    );
    (failures, summary)
}

/// Запросы к файлу приманки, разложенные по «кто» и «когда».
#[derive(Debug, Default)]
struct DecoyRequests {
    /// Загрузки медиаэлемента страницы до передачи.
    element_before: Vec<MediaRequest>,
    /// Скриптовые запросы (фон экстеншена) после передачи.
    background: Vec<MediaRequest>,
}

/// Кто сделал запрос к приманке.
///
/// Причина отдельной классификации (живой прогон 09c): рекламный ролик лежит под
/// `http://ad.doubleclick.net:<порт>`, а это НЕ «потенциально надёжный» адрес (не localhost),
/// поэтому браузер не шлёт ему `Sec-Fetch-*` вообще. Обычная классификация по `Sec-Fetch-Dest`
/// записала бы загрузку самого `<video>` как «скрипт», и линейка слепла. `Origin` тоже не
/// различает: фон Firefox (замер 09c) шлёт `fetch` без него. Различает диапазон: фон всегда просит
/// ограниченный кусок (`bytes=0-65535`), элемент — открытый (`bytes=0-`). Элемент здесь — это
/// `Sec-Fetch-Dest: video|audio` ЛИБО запрос без `Sec-Fetch-Dest`, без `Origin` и с открытым
/// диапазоном.
fn requester_of_decoy(request: &MediaRequest) -> MediaRequester {
    match request.requester() {
        MediaRequester::MediaElement => MediaRequester::MediaElement,
        MediaRequester::Script
            if request.sec_fetch_dest.is_none()
                && request.origin.is_none()
                && asks_open_ended_range(request) =>
        {
            MediaRequester::MediaElement
        }
        MediaRequester::Script => MediaRequester::Script,
    }
}

/// Диапазон без конца (`bytes=0-`): так просит медиаэлемент, но не закачка фона.
fn asks_open_ended_range(request: &MediaRequest) -> bool {
    request
        .range
        .as_deref()
        .is_some_and(|range| range.ends_with('-'))
}

fn sort_decoy_requests(
    file_path: &str,
    requests: &[MediaRequest],
    handed_off_at: Instant,
) -> DecoyRequests {
    let mut sorted = DecoyRequests::default();
    for request in requests.iter().filter(|request| request.path == file_path) {
        let before = request.at < handed_off_at;
        match (requester_of_decoy(request), before) {
            (MediaRequester::MediaElement, true) => sorted.element_before.push(request.clone()),
            // Элемент после передачи (реклама играет по кругу) фона не касается.
            (MediaRequester::MediaElement, false) => {}
            (MediaRequester::Script, false) => sorted.background.push(request.clone()),
            // Скриптовый запрос до передачи — дело самой страницы.
            (MediaRequester::Script, true) => {}
        }
    }
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn request(at: Instant, dest: &str) -> MediaRequest {
        MediaRequest {
            at,
            path: AD_FILE_PATH.to_owned(),
            status: 206,
            range: Some("bytes=0-".to_owned()),
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

    #[test]
    fn a_decoy_only_the_element_loaded_is_honest() {
        let handed = Instant::now();
        let log = [request(handed - Duration::from_secs(2), "video")];
        let (failures, summary) = decoy_not_fetched_by_background(AD_FILE_PATH, &log, handed);
        assert!(failures.is_empty(), "{failures:#?}");
        assert!(summary.contains("background requests after the hand-off=0"));
    }

    #[test]
    fn a_background_download_of_the_decoy_fails() {
        let handed = Instant::now();
        let log = [
            request(handed - Duration::from_secs(2), "video"),
            request(handed + Duration::from_secs(1), "empty"),
        ];
        let (failures, _) = decoy_not_fetched_by_background(AD_FILE_PATH, &log, handed);
        assert_eq!(failures.len(), 1, "{failures:#?}");
        assert!(failures[0].contains("background downloaded"));
    }

    /// Реклама под не-localhost адресом: браузер не шлёт `Sec-Fetch-*`, но и `Origin` у элемента нет.
    fn untrusted_host_request(at: Instant, origin: Option<&str>) -> MediaRequest {
        MediaRequest {
            sec_fetch_dest: None,
            origin: origin.map(str::to_owned),
            range: Some("bytes=0-".to_owned()),
            ..request(at, "video")
        }
    }

    #[test]
    fn an_element_load_without_sec_fetch_headers_still_counts_as_the_element() {
        let handed = Instant::now();
        let log = [untrusted_host_request(
            handed - Duration::from_secs(2),
            None,
        )];
        let (failures, summary) = decoy_not_fetched_by_background(AD_FILE_PATH, &log, handed);
        assert!(failures.is_empty(), "{failures:#?}");
        assert!(summary.contains("element requests before=1"), "{summary}");
    }

    #[test]
    fn a_background_fetch_with_origin_and_no_sec_fetch_headers_fails() {
        let handed = Instant::now();
        let log = [
            untrusted_host_request(handed - Duration::from_secs(2), None),
            untrusted_host_request(
                handed + Duration::from_secs(1),
                Some("chrome-extension://x"),
            ),
        ];
        let (failures, _) = decoy_not_fetched_by_background(AD_FILE_PATH, &log, handed);
        assert_eq!(failures.len(), 1, "{failures:#?}");
        assert!(failures[0].contains("background downloaded"));
    }

    /// Фон Firefox на не-localhost адресе: нет ни `Sec-Fetch-*`, ни `Origin` (замер 09c).
    #[test]
    fn a_firefox_background_fetch_without_any_marker_fails_by_its_bounded_range() {
        let handed = Instant::now();
        let log = [
            untrusted_host_request(handed - Duration::from_secs(2), None),
            MediaRequest {
                range: Some("bytes=0-65535".to_owned()),
                ..untrusted_host_request(handed + Duration::from_secs(1), None)
            },
        ];
        let (failures, _) = decoy_not_fetched_by_background(AD_FILE_PATH, &log, handed);
        assert_eq!(failures.len(), 1, "{failures:#?}");
        assert!(failures[0].contains("background downloaded"));
    }

    #[test]
    fn a_looping_element_after_the_hand_off_is_not_the_background() {
        let handed = Instant::now();
        let log = [
            untrusted_host_request(handed - Duration::from_secs(2), None),
            untrusted_host_request(handed + Duration::from_secs(1), None),
        ];
        let (failures, _) = decoy_not_fetched_by_background(AD_FILE_PATH, &log, handed);
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn a_blind_ruler_fails_instead_of_passing_silently() {
        let handed = Instant::now();
        let (failures, _) = decoy_not_fetched_by_background(AD_FILE_PATH, &[], handed);
        assert_eq!(failures.len(), 1, "{failures:#?}");
        assert!(failures[0].contains("ruler"));
    }

    #[test]
    fn the_table_knows_the_banner_page() {
        assert_eq!(
            decoy_page("banner-with-main").map(|page| page.file_path),
            Some("/media/progressive/h264-aac.mp4")
        );
        assert!(decoy_page("mse-fmp4").is_none());
    }

    /// Пути файлов приманок — те, под которыми ролики лежат на сервере (по скрипту генерации).
    #[test]
    fn decoy_paths_match_the_generated_media_layout() {
        let script = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("media/generate.sh"),
        )
        .expect("the media generator script");
        assert!(
            script.contains("register_asset ad-preroll file progressive/ad.mp4"),
            "the ad file moved: update AD_FILE_PATH"
        );
        assert_eq!(AD_FILE_PATH, "/media/progressive/ad.mp4");
        assert!(
            script.contains("register_asset progressive-h264-aac file progressive/h264-aac.mp4"),
            "the banner file moved: update DECOY_PAGES"
        );
        assert!(script.contains("progressive-long-h264-aac file progressive/h264-aac-long.mp4"));
    }
}
