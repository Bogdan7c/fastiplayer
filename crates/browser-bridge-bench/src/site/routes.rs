//! Маршруты сайта стенда. Превращают разобранный запрос в ответ; ввод-вывода сокета здесь нет.

use std::path::PathBuf;

use crate::media::MediaCatalog;
use crate::media::safe_path::{join_inside, resolve_existing_file};

use super::cdn_guard::{CdnGuard, CdnVerdict, RefererRule, login_cookie_header};
use super::hls_live;
use super::hls_live_ends;
use super::hls_local_segments;
use super::http::{Request, Response};
use super::reports::{BrowserLogLine, PageReport, ReportStore};
use super::request_log::RequestLog;
use super::run_id::RunId;
use super::static_files::serve_file;

/// Состояние, общее для обоих слушателей (один и тот же сайт на двух origin).
pub(super) struct SharedState {
    pub site_root: PathBuf,
    pub media: MediaCatalog,
    pub reports: ReportStore,
    pub cdn: CdnGuard,
    /// Запросы медиа с заголовками (сессия 09b: кто качает `<video src>`).
    pub requests: RequestLog,
    /// Запись потока страницы для `replay` (`/bench/recording`); `None`, пока команда не выбрала файл.
    pub recording: std::sync::Mutex<Option<PathBuf>>,
    /// Часы «живых» HLS-эфиров по прогонам (`hls_live`, сессия 10a3).
    pub hls_live: hls_live::LiveClocks,
    /// Часы «кончающихся» HLS-эфиров по прогонам (`hls_live_ends`, сессия 16d1): свои, чтобы
    /// эфиры `hls-live` и `hls-live-ends` одного прогона не делили начало отсчёта.
    pub hls_live_ends: hls_live::LiveClocks,
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

/// Главная точка: ответ на запрос; запросы медиа записываются в журнал вместе со статусом.
pub(super) fn route(context: &RouteContext, request: &Request) -> Response {
    let response = route_request(context, request);
    context.shared.requests.record(request, &response);
    response
}

/// Выбирает обработчик по методу и пути.
fn route_request(context: &RouteContext, request: &Request) -> Response {
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
    // Те же файлы, что под `/media/`, но под другим префиксом: журнал отличает «локальные» сегменты
    // сценария `local-address` от публичных (см. `hls_local_segments`).
    if let Some(relative) = path.strip_prefix(hls_local_segments::LOCAL_TARGET_PREFIX) {
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
            serve_cdn_media(context, request, relative, RefererRule::OwnOrigin)
        } else {
            method_not_allowed()
        };
    }
    if let Some(relative) = path.strip_prefix("/cdn/session-media/") {
        return if is_read {
            serve_cdn_media(context, request, relative, RefererRule::NotChecked)
        } else {
            method_not_allowed()
        };
    }
    if let Some(relative) = path.strip_prefix(REDIRECT_TO_MAIN_ORIGIN_PREFIX) {
        return if is_read {
            redirect_to_main_origin(context, relative)
        } else {
            method_not_allowed()
        };
    }
    if let Some(relative) = path.strip_prefix(hls_local_segments::ROUTE_PREFIX) {
        return if is_read {
            hls_local_segments::serve(context, relative)
        } else {
            method_not_allowed()
        };
    }
    if let Some(relative) = path.strip_prefix(hls_live::ROUTE_PREFIX) {
        return if is_read {
            hls_live::serve(context, relative, range)
        } else {
            method_not_allowed()
        };
    }
    if let Some(relative) = path.strip_prefix(hls_live_ends::ROUTE_PREFIX) {
        return if is_read {
            hls_live_ends::serve(context, relative, range)
        } else {
            method_not_allowed()
        };
    }
    if path == "/bench/recording" {
        return if is_read {
            with_cors(serve_recording(&context.shared, range))
        } else {
            method_not_allowed()
        };
    }
    if let Some(rest) = path.strip_prefix("/bench/commands/") {
        return if is_read {
            with_cors(serve_commands(&context.shared.reports, rest))
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

/// Путь, который отвечает 302 на тот же файл под настоящим адресом сервера стенда
/// (`http://127.0.0.1:<порт>/media/…`). Сценарий `local-address`: «публичный» файл ведёт на локальный
/// адрес. Лежит под `/cdn/`, поэтому шаг перенаправления попадает в журнал запросов.
const REDIRECT_TO_MAIN_ORIGIN_PREFIX: &str = "/cdn/redirect-local/";

fn redirect_to_main_origin(context: &RouteContext, relative: &str) -> Response {
    Response::new(302)
        .with_header(
            "Location",
            format!("{}/media/{relative}", context.own_origin),
        )
        .with_header("Access-Control-Allow-Origin", "*")
}

fn method_not_allowed() -> Response {
    Response::text(405, "method not allowed").with_header("Allow", "GET, HEAD, POST, OPTIONS")
}

fn serve_site_file(context: &RouteContext, relative: &str, range: Option<&str>) -> Response {
    // CORS нужен субтитрам: страница главного origin качает `.vtt` со ВТОРОГО origin (`subtitles-fetch`).
    with_cors(serve_under(&context.shared.site_root, relative, range))
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

/// Файл записи потока страницы, выбранный командой `replay` (404, пока файл не выбран).
fn serve_recording(shared: &SharedState, range: Option<&str>) -> Response {
    let selected = match shared.recording.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    match selected {
        Some(path) => serve_file(&path, range),
        None => Response::text(404, "no recording selected"),
    }
}

fn cdn_login() -> Response {
    Response::new(204).with_header("Set-Cookie", login_cookie_header())
}

fn serve_cdn_media(
    context: &RouteContext,
    request: &Request,
    relative: &str,
    referer_rule: RefererRule,
) -> Response {
    let verdict = context.shared.cdn.check(
        request.header("cookie"),
        request.header("referer"),
        &context.own_origin,
        referer_rule,
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

/// `GET /bench/commands/<run>/<сколько страница уже получила>` → JSON-массив новых команд.
///
/// Страница опрашивает этот адрес сама (стенд не может толкнуть её извне), так робот
/// передаёт странице «передача началась», «заверши и отчитайся» и т. п.
fn serve_commands(store: &ReportStore, rest: &str) -> Response {
    let parsed = rest.split_once('/').and_then(|(run, seen)| {
        let run = RunId::new(run).ok()?;
        let seen = seen.parse::<usize>().ok()?;
        Some((run, seen))
    });
    let Some((run, seen)) = parsed else {
        return Response::text(400, "expected /bench/commands/<run>/<seen>");
    };
    match serde_json::to_string(&store.commands_since(&run, seen)) {
        Ok(body) => Response::text(200, &body),
        Err(error) => Response::text(500, &format!("cannot encode commands: {error}")),
    }
}
