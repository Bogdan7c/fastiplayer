//! Улики Referer для `<video src>` на `/cdn/media/` (09c, решение 202): сторож CDN пускает файл
//! только с кукой входа И с Referer своего сайта. Фон экстеншена качает файл как `fetch`, поэтому
//! ему нужен Referer страницы, но только на ЕГО запрос — правило не должно течь на запросы вкладки.
//!
//! Что доказывает сервер (чистые функции над журналом запросов, без браузера):
//! - КАЖДЫЙ запрос фона после передачи несёт Referer = адрес страницы (иначе сторож ответил бы 403,
//!   но здесь проверяем и сам заголовок, не только статус);
//! - запросы самого `<video>` страницы до передачи шли с тем же Referer: элемент не изменился;
//! - пробы страницы с меткой (`x-bench-page-fetch`): запрос «без Referer» дошёл БЕЗ Referer (правило
//!   фона не потекло на вкладку), запрос «с обычным Referer» — с ним и принят (206).

use super::file_evidence::FileRequests;
use crate::site::MediaRequest;

/// Метка пробы «страница просит без Referer» (зеркало `video-src-referer.html`).
pub(in crate::scenarios) const MARKER_NO_REFERER: &str = "no-referer";
/// Метка пробы «страница просит с обычным Referer».
pub(in crate::scenarios) const MARKER_WITH_REFERER: &str = "with-referer";
/// Страница, чей файл защищён Referer.
pub(in crate::scenarios) const REFERER_PAGE_ID: &str = "video-src-referer";

/// Referer «с этого origin»: сам origin либо origin и дальше `/` (соседний порт не подходит).
fn referer_is_from(request: &MediaRequest, page_origin: &str) -> bool {
    request.referer.as_deref().is_some_and(|value| {
        value
            .strip_prefix(page_origin)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// Страница с защитой Referer?
pub(in crate::scenarios) fn is_referer_page(page_id: &str) -> bool {
    page_id == REFERER_PAGE_ID
}

/// Провалы по Referer; пусто — всё честно. `page_origin` — origin страницы стенда.
pub(in crate::scenarios) fn referer_failures(
    requests: &FileRequests,
    page_origin: &str,
) -> Vec<String> {
    let mut failures = Vec::new();
    if requests.background.is_empty() {
        failures.push("no background request: the Referer cannot be judged".to_owned());
    }
    for request in &requests.background {
        if !referer_is_from(request, page_origin) {
            failures.push(format!(
                "a background request does not carry the page origin {page_origin} as Referer: {}",
                request.headers_summary()
            ));
            break;
        }
    }
    if let Some(request) = requests
        .element_before
        .iter()
        .find(|request| !referer_is_from(request, page_origin) || request.status >= 400)
    {
        failures.push(format!(
            "the page's own <video> request changed (expected the page Referer and a success): {}",
            request.headers_summary()
        ));
    }
    // Пробы страницы с нужной меткой.
    fn probes<'a>(
        requests: &'a FileRequests,
        marker: &'a str,
    ) -> impl Iterator<Item = &'a MediaRequest> {
        requests
            .page_fetches
            .iter()
            .filter(move |request| request.page_fetch_marker.as_deref() == Some(marker))
    }
    let probe = |marker| probes(requests, marker);
    if probe(MARKER_NO_REFERER).next().is_none() || probe(MARKER_WITH_REFERER).next().is_none() {
        failures.push(
            "the page's own leak probes never reached the server (the hook did not run)".to_owned(),
        );
    }
    for request in probe(MARKER_NO_REFERER) {
        if request.referer.is_some() {
            failures.push(format!(
                "the extension's Referer rule leaked onto a request of the tab: {}",
                request.headers_summary()
            ));
        }
    }
    for request in probe(MARKER_WITH_REFERER) {
        if !referer_is_from(request, page_origin) || request.status != 206 {
            failures.push(format!(
                "the page's ordinary request lost its own Referer or was refused: {}",
                request.headers_summary()
            ));
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const ORIGIN: &str = "http://127.0.0.1:4000";

    fn request(referer: Option<&str>, status: u16, marker: Option<&str>) -> MediaRequest {
        MediaRequest {
            at: Instant::now(),
            path: "/cdn/media/f.mp4".into(),
            status,
            range: Some("bytes=0-99".into()),
            cookie: Some("bench_cdn=1".into()),
            referer: referer.map(str::to_owned),
            origin: None,
            sec_fetch_site: None,
            sec_fetch_mode: None,
            sec_fetch_dest: Some("empty".into()),
            page_fetch_marker: marker.map(str::to_owned),
            content_range: None,
        }
    }

    fn page_referer() -> String {
        format!("{ORIGIN}/site/pages/video-src-referer.html?run=r")
    }

    fn honest() -> FileRequests {
        let referer = page_referer();
        FileRequests {
            element_before: vec![request(Some(&referer), 206, None)],
            element_under_shadow: Vec::new(),
            background: vec![request(Some(&format!("{ORIGIN}/")), 206, None)],
            page_fetches: vec![
                request(None, 403, Some(MARKER_NO_REFERER)),
                request(Some(&referer), 206, Some(MARKER_WITH_REFERER)),
            ],
        }
    }

    #[test]
    fn honest_traffic_has_no_failures() {
        assert_eq!(referer_failures(&honest(), ORIGIN), Vec::<String>::new());
    }

    #[test]
    fn a_background_request_without_the_page_referer_fails() {
        let mut requests = honest();
        requests.background.push(request(None, 403, None));
        let failures = referer_failures(&requests, ORIGIN);
        assert_eq!(failures.len(), 1, "{failures:#?}");
        assert!(failures[0].contains("background request"));
        // Referer соседнего порта — чужой.
        let mut neighbour = honest();
        neighbour.background[0] = request(Some("http://127.0.0.1:40001/"), 206, None);
        assert_eq!(referer_failures(&neighbour, ORIGIN).len(), 1);
        // Нет ни одного запроса фона — судить нечего.
        let mut silent = honest();
        silent.background.clear();
        assert!(!referer_failures(&silent, ORIGIN).is_empty());
    }

    #[test]
    fn a_rule_leak_onto_the_tab_is_caught_by_the_probe() {
        let mut requests = honest();
        requests.page_fetches[0] = request(Some(&page_referer()), 206, Some(MARKER_NO_REFERER));
        let failures = referer_failures(&requests, ORIGIN);
        assert!(
            failures.iter().any(|text| text.contains("leaked")),
            "{failures:#?}"
        );
    }

    #[test]
    fn a_changed_page_request_or_a_missing_probe_fails() {
        let mut changed = honest();
        changed.element_before[0] = request(None, 403, None);
        assert!(
            referer_failures(&changed, ORIGIN)
                .iter()
                .any(|text| text.contains("own <video> request changed"))
        );
        let mut lost = honest();
        lost.page_fetches[1] = request(None, 403, Some(MARKER_WITH_REFERER));
        assert!(
            referer_failures(&lost, ORIGIN)
                .iter()
                .any(|text| text.contains("ordinary request lost"))
        );
        let mut absent = honest();
        absent.page_fetches.clear();
        assert!(
            referer_failures(&absent, ORIGIN)
                .iter()
                .any(|text| text.contains("never reached the server"))
        );
    }

    /// Метки и заголовок, которыми страница помечает свои пробы, совпадают с теми, что ждёт сервер
    /// и судья (иначе проба утечки молча превратилась бы в «проба не дошла»).
    #[test]
    fn the_page_uses_the_markers_and_header_the_judge_and_the_server_expect() {
        let page = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("site/pages/video-src-referer.html"),
        )
        .expect("the video-src-referer page");
        assert!(page.contains(&format!("'{MARKER_NO_REFERER}'")));
        assert!(page.contains(&format!("'{MARKER_WITH_REFERER}'")));
        assert!(page.contains(&format!(
            "const PAGE_FETCH_MARKER_HEADER = '{}'",
            crate::site::PAGE_FETCH_MARKER_HEADER
        )));
        assert!(page.contains("/cdn/media/"));
    }
}
