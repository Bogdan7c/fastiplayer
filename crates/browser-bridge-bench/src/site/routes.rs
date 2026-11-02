//! Маршруты сайта стенда. Превращают разобранный запрос в ответ; ввод-вывода сокета здесь нет.

use std::path::PathBuf;

use crate::media::MediaCatalog;
use crate::media::safe_path::{join_inside, resolve_existing_file};

use super::cdn_guard::{CdnGuard, CdnVerdict, login_cookie_header};
use super::http::{Request, Response};
use super::reports::{BrowserLogLine, PageReport, ReportStore};
use super::static_files::serve_file;

/// Состояние, общее для обоих слушателей (один и тот же сайт на двух origin).
pub(super) struct SharedState {
    pub site_root: PathBuf,
    pub media: MediaCatalog,
    pub reports: ReportStore,
    pub cdn: CdnGuard,
}

/// Всё, что нужно обработчику запроса: общее состояние и origin именно ЭТОГО слушателя
/// (сторож CDN сверяет Referer с собственным origin, а он у двух слушателей разный).
pub(super) struct RouteContext {
    pub shared: std::sync::Arc<SharedState>,
    pub own_origin: String,
}

/// Заголовки, разрешающие странице с чужого origin ходить за медиа и слать отчёты главному.
fn with_cors(response: Response) -> Response {
    response.with_header("Access-Control-Allow-Origin", "*")
}

fn preflight_response() -> Response {
    with_cors(Response::new(204))
        .with_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        .with_header("Access-Control-Allow-Headers", "content-type, range")
        .with_header("Access-Control-Max-Age", "600")
}

/// Главная точка: выбирает обработчик по методу и пути.
pub(super) fn route(context: &RouteContext, request: &Request) -> Response {
    if request.method == "OPTIONS" {
        return preflight_response();
    }
    let is_read = matches!(request.method.as_str(), "GET" | "HEAD");
    let path = request.path.as_str();
    let range = request.header("range");
    if path == "/" {
        return if is_read {
            serve_site_file(context, "index.html", range)
        } else {
            method_not_allowed()
        };
    }
    if let Some(relative) = path.strip_prefix("/site/") {
        return if is_read {
            serve_site_file(context, relative, range)
        } else {
            method_not_allowed()
        };
    }
    if let Some(relative) = path.strip_prefix("/media/") {
        return if is_read {
            with_cors(serve_under(&context.shared.media.root, relative, range))
        } else {
            method_not_allowed()
        };
    }
    if path == "/cdn/login" {
        return if is_read {
            cdn_login()
        } else {
            method_not_allowed()
        };
    }
    if let Some(relative) = path.strip_prefix("/cdn/media/") {
        return if is_read {
            serve_cdn_media(context, request, relative)
        } else {
            method_not_allowed()
        };
    }
    match (request.method.as_str(), path) {
        ("POST", "/bench/report") => {
            with_cors(accept_report(&context.shared.reports, &request.body))
        }
        ("POST", "/bench/log") => with_cors(accept_log(&context.shared.reports, &request.body)),
        (_, "/bench/report" | "/bench/log") => method_not_allowed(),
        _ => Response::text(404, "not found"),
    }
}

fn method_not_allowed() -> Response {
    Response::text(405, "method not allowed").with_header("Allow", "GET, HEAD, POST, OPTIONS")
}

fn serve_site_file(context: &RouteContext, relative: &str, range: Option<&str>) -> Response {
    serve_under(&context.shared.site_root, relative, range)
}

/// Файл внутри корня: небезопасный путь — 400 (его никто честный не пришлёт), нет файла — 404.
fn serve_under(root: &std::path::Path, relative: &str, range: Option<&str>) -> Response {
    if join_inside(root, relative).is_none() {
        return Response::text(400, "bad path");
    }
    match resolve_existing_file(root, relative) {
        Some(path) => serve_file(&path, range),
        None => Response::text(404, "not found"),
    }
}

fn cdn_login() -> Response {
    Response::new(204).with_header("Set-Cookie", login_cookie_header())
}

fn serve_cdn_media(context: &RouteContext, request: &Request, relative: &str) -> Response {
    let verdict = context.shared.cdn.check(
        request.header("cookie"),
        request.header("referer"),
        &context.own_origin,
    );
    match verdict {
        CdnVerdict::Allow => serve_under(
            &context.shared.media.root,
            relative,
            request.header("range"),
        ),
        CdnVerdict::MissingCookie => Response::text(403, "cdn: login cookie is missing"),
        CdnVerdict::ForeignReferer => Response::text(403, "cdn: referer is not from this origin"),
    }
}

fn accept_report(store: &ReportStore, body: &[u8]) -> Response {
    match serde_json::from_slice::<PageReport>(body) {
        Ok(report) => {
            store.put_report(report);
            Response::new(204)
        }
        Err(error) => Response::text(400, &format!("bad report: {error}")),
    }
}

fn accept_log(store: &ReportStore, body: &[u8]) -> Response {
    match serde_json::from_slice::<BrowserLogLine>(body) {
        Ok(entry) => {
            store.put_log(entry);
            Response::new(204)
        }
        Err(error) => Response::text(400, &format!("bad log line: {error}")),
    }
}
