//! Журнал запросов медиа (`/media/…` и `/cdn/…`) на стороне сервера стенда (сессия 09b).
//!
//! Зачем: у `<video src>` под тенью две стороны качают один и тот же файл — сам `<video>`
//! страницы (до передачи и после возврата) и фон экстеншена (по `fetch_range` плеера). Только
//! сервер видит обе честно:
//! - «настоящий элемент перестал качать» — после передачи нет запросов с
//!   `Sec-Fetch-Dest: video|audio` (так браузер помечает загрузку медиаэлемента);
//! - «фон качает с куками сайта» — запросы фона (`Sec-Fetch-Dest: empty` или без него) несут
//!   куку `bench_cdn`, и видно, какие ещё заголовки (Referer, Origin, Sec-Fetch-Site) шлёт фон
//!   в каждом браузере.
//!
//! Журнал ограничен по числу записей: стенд прогоняет страницы последовательно, и тысяч
//! запросов на одну страницу не бывает.

use std::sync::Mutex;
use std::time::Instant;

use super::http::{Request, Response};

/// Сколько записей держит журнал; старые вытесняются (запросы одной страницы — единицы сотен).
const MAX_LOGGED_REQUESTS: usize = 4096;

/// Заголовок-метка собственных запросов страницы стенда (имя дублирует `video-src-referer.html`).
pub const PAGE_FETCH_MARKER_HEADER: &str = "x-bench-page-fetch";

/// Префиксы путей, запросы к которым записываются (`/cdn/` — оба пути сторожа CDN).
const LOGGED_PATH_PREFIXES: [&str; 2] = ["/media/", "/cdn/"];

/// Один запрос медиа, как его увидел сервер.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRequest {
    /// Когда пришёл (по часам процесса стенда).
    pub at: Instant,
    /// Путь без строки запроса.
    pub path: String,
    /// Ответ сервера (206, 403, …).
    pub status: u16,
    /// Заголовки запроса, важные для разбора (имена — в нижнем регистре).
    pub range: Option<String>,
    pub cookie: Option<String>,
    pub referer: Option<String>,
    pub origin: Option<String>,
    pub sec_fetch_site: Option<String>,
    pub sec_fetch_mode: Option<String>,
    pub sec_fetch_dest: Option<String>,
    /// Значение заголовка-метки `x-bench-page-fetch`: так страница стенда помечает СВОИ
    /// запросы к файлу (проба утечки правила Referer, `video-src-referer`), чтобы они не
    /// спутались с запросами фона экстеншена. `None` — метки нет.
    pub page_fetch_marker: Option<String>,
    /// `Content-Range` ответа (`bytes a-b/total`; `None` — ответ не 206).
    pub content_range: Option<String>,
}

/// Отрезок файла `[first, end)` и полный размер — из `Content-Range` ответа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServedSpan {
    pub first: u64,
    pub end: u64,
    pub total: u64,
}

/// Кто сделал запрос — по `Sec-Fetch-Dest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaRequester {
    /// Медиаэлемент страницы (`video` / `audio`).
    MediaElement,
    /// Скрипт: `fetch` страницы или фона экстеншена (`empty` или заголовка нет).
    Script,
}

impl MediaRequest {
    /// Кто сделал запрос.
    #[must_use]
    pub fn requester(&self) -> MediaRequester {
        match self.sec_fetch_dest.as_deref() {
            Some("video" | "audio") => MediaRequester::MediaElement,
            _ => MediaRequester::Script,
        }
    }

    /// Какой отрезок файла сервер отдал (`None` — не 206 или размер неизвестен).
    #[must_use]
    pub fn served_span(&self) -> Option<ServedSpan> {
        let value = self.content_range.as_deref()?.strip_prefix("bytes ")?;
        let (range, total) = value.split_once('/')?;
        let (first, last) = range.split_once('-')?;
        let first: u64 = first.parse().ok()?;
        let last: u64 = last.parse().ok()?;
        Some(ServedSpan {
            first,
            end: last.checked_add(1)?,
            total: total.parse().ok()?,
        })
    }

    /// Несёт ли запрос куку с этим именем.
    #[must_use]
    pub fn has_cookie(&self, name: &str) -> bool {
        self.cookie.as_deref().is_some_and(|header| {
            header.split(';').any(|pair| {
                pair.trim()
                    .split_once('=')
                    .is_some_and(|(key, _)| key == name)
            })
        })
    }

    /// Короткая запись заголовков для отчёта стенда.
    #[must_use]
    pub fn headers_summary(&self) -> String {
        let show = |value: &Option<String>| value.as_deref().unwrap_or("-").to_owned();
        format!(
            "{} {} range={} cookie={} referer={} origin={} sec-fetch-site={} sec-fetch-mode={} sec-fetch-dest={}",
            self.status,
            self.path,
            show(&self.range),
            show(&self.cookie),
            show(&self.referer),
            show(&self.origin),
            show(&self.sec_fetch_site),
            show(&self.sec_fetch_mode),
            show(&self.sec_fetch_dest),
        )
    }
}

/// Журнал запросов медиа.
#[derive(Default)]
pub(super) struct RequestLog {
    entries: Mutex<Vec<MediaRequest>>,
}

impl RequestLog {
    /// Записывает запрос, если это медиа.
    pub(super) fn record(&self, request: &Request, response: &Response) {
        if !LOGGED_PATH_PREFIXES
            .iter()
            .any(|prefix| request.path.starts_with(prefix))
        {
            return;
        }
        let header = |name: &str| request.header(name).map(str::to_owned);
        let entry = MediaRequest {
            at: Instant::now(),
            path: request.path.clone(),
            status: response.status,
            content_range: response
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-range"))
                .map(|(_, value)| value.clone()),
            range: header("range"),
            cookie: header("cookie"),
            referer: header("referer"),
            origin: header("origin"),
            sec_fetch_site: header("sec-fetch-site"),
            sec_fetch_mode: header("sec-fetch-mode"),
            sec_fetch_dest: header("sec-fetch-dest"),
            page_fetch_marker: header(PAGE_FETCH_MARKER_HEADER),
        };
        // Отравленный замок значит, что поток сервера упал посреди записи: журнал — улика, а не
        // источник истины, поэтому продолжаем с тем, что есть.
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.len() >= MAX_LOGGED_REQUESTS {
            entries.remove(0);
        }
        entries.push(entry);
    }

    /// Все записи по порядку прихода.
    pub(super) fn snapshot(&self) -> Vec<MediaRequest> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
